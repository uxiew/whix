use super::*;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};
use tokio::io::ReadBuf;

#[derive(Default)]
struct FlushLatch {
    released: AtomicBool,
    waiting: Notify,
    waker: std::sync::Mutex<Option<Waker>>,
}

impl FlushLatch {
    fn release(&self) {
        self.released.store(true, Ordering::Release);
        if let Some(waker) = self.waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}

// Bytes can reach the peer before a buffered writer's flush completes. Hold
// that boundary explicitly rather than relying on thread scheduling luck.
struct GatedFlush {
    inner: DuplexStream,
    latch: Arc<FlushLatch>,
}

impl AsyncRead for GatedFlush {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for GatedFlush {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        {
            let mut waker = self.latch.waker.lock().unwrap();
            if !self.latch.released.load(Ordering::Acquire) {
                *waker = Some(cx.waker().clone());
                self.latch.waiting.notify_one();
                return Poll::Pending;
            }
        }
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[test]
fn close_rejects_injections_even_while_its_flush_is_backpressured() {
    rt().block_on(async {
        for dir in [Dir::Send, Dir::Receive] {
            let state = state_with(Plugins::new());
            let (mut client, client_io) = tokio::io::duplex(64);
            let (mut server, upstream_io) = tokio::io::duplex(64);
            let latch = Arc::new(FlushLatch::default());
            let wrap = |inner| GatedFlush {
                inner,
                latch: latch.clone(),
            };
            let mut tunnel = if dir == Dir::Send {
                tokio::spawn(capturing_tunnel(
                    client_io,
                    wrap(upstream_io),
                    None,
                    FramePlan::default(),
                    FrameFlow::default(),
                    state.clone(),
                    7,
                ))
            } else {
                tokio::spawn(capturing_tunnel(
                    wrap(client_io),
                    upstream_io,
                    None,
                    FramePlan::default(),
                    FrameFlow::default(),
                    state.clone(),
                    7,
                ))
            };
            gate_of(&state).await;
            let writers = state.ws_write.lock().unwrap()[&7].clone();
            let (sender, receiver) = if dir == Dir::Send {
                (&mut client, &mut server)
            } else {
                (&mut server, &mut client)
            };
            write_frame(sender, true, OPCODE_CLOSE, &[3, 232], dir == Dir::Send)
                .await
                .unwrap();
            assert_eq!(
                read_frame(receiver).await.unwrap().unwrap().opcode,
                OPCODE_CLOSE
            );
            latch.waiting.notified().await;
            for direction in ["send", "receive"] {
                let sent = tokio::time::timeout(
                    Duration::from_millis(100),
                    writers.send_binary(direction, b"too late"),
                )
                .await
                .expect("Close is on the wire: reject new data without waiting for the writer");
                assert!(!sent, "no new data once a Close is being delivered");
            }
            assert!(writers.live(), "Release and Abort remain available");
            latch.release();
            write_frame(receiver, true, OPCODE_CLOSE, &[3, 232], dir == Dir::Receive)
                .await
                .unwrap();
            assert_eq!(
                read_frame(sender).await.unwrap().unwrap().opcode,
                OPCODE_CLOSE
            );
            tokio::time::timeout(Duration::from_secs(1), &mut tunnel)
                .await
                .unwrap()
                .unwrap();
            assert!(state.ws_write.lock().unwrap().is_empty());
        }
    });
}
