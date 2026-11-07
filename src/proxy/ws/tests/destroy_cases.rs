use super::*;
use std::pin::Pin;
use std::task::{Context, Poll};

#[derive(Clone, Default)]
pub(crate) struct Recorded(pub Arc<std::sync::Mutex<Vec<u8>>>);
impl AsyncWrite for Recorded {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

pub(crate) async fn destroy_script() -> FrameScript {
    let info = apply::build_req_info(
        "GET",
        "ws",
        "fixture.test",
        80,
        "/",
        &hyper::HeaderMap::new(),
        None,
    );
    let spec = FrameScriptSpec::for_request(
        "ctx.handleSendToServerFrame=function(){ctx.destroy();};".into(),
        &info,
        "fixture.test",
        &Default::default(),
    )
    .unwrap();
    FrameScript::start(spec).await.script.unwrap()
}
pub(crate) async fn destroy(script: &FrameScript) {
    let outcome = script
        .relay_controlled(ScriptFrame {
            to_server: true,
            data: b"trigger".to_vec(),
            binary: false,
        })
        .await;
    assert!(matches!(outcome, ControlFlow::Break(_)));
}

#[test]
fn waiting_ws_write_cannot_resume_after_destroy() {
    rt().block_on(async {
        let script = destroy_script().await;
        let bytes = Recorded::default();
        let state = state_with(Plugins::new());
        let writers = Arc::new(SessionWriters::new(tokio::io::sink(), bytes.clone()));
        let ctx = FrameCtx {
            direction: "receive",
            to_server: false,
            script: Some(script.clone()),
            hooks: Vec::new(),
            mode: DirMode::Pass,
            state,
            session: Some(7),
            writers: writers.clone(),
        };
        let hold = writers.to_client.lock().await;
        let mut writer = writers.to_client();
        let write = deliver(&mut writer, true, OPCODE_TEXT, b"queued", &ctx);
        tokio::pin!(write);
        assert!(std::future::poll_fn(|cx| Poll::Ready(write.as_mut().poll(cx).is_pending())).await);
        // Mark destruction in the real VM, before the transport's abort arm
        // runs. Releasing the lock makes the stale write immediately runnable.
        destroy(&script).await;
        drop(hold);
        assert!(matches!(write.await, Sent::Failed));
        assert!(bytes.0.lock().unwrap().is_empty());
    });
}

#[test]
fn destroy_does_not_wait_for_a_backpressured_opposite_writer() {
    rt().block_on(async {
        let state = state_with(Plugins::new());
        let (mut client, client_io) = tokio::io::duplex(64);
        let (mut server, origin_io) = tokio::io::duplex(16 * 1024);
        let info = apply::build_req_info("GET", "ws", "fixture.test", 80, "/", &hyper::HeaderMap::new(), None);
        let spec = FrameScriptSpec::for_request(
            "ctx.handleSendToServerFrame=function(){ctx.sendToClient('queued injection');ctx.destroy();};".into(), &info, "fixture.test", &Default::default());
        let task = tokio::spawn(capturing_tunnel(client_io, origin_io, spec, FramePlan::default(), FrameFlow::default(), state.clone(), 7));
        gate_of(&state).await;
        write_frame(&mut server, true, OPCODE_BINARY, &[42; 8192], false).await.unwrap();
        // Seeing capture means this direction already reached the writer. Its
        // 8 KiB output cannot fit in the unread 64-byte client buffer.
        until("opposite writer is full", || !state.ws_frames.lock().unwrap().is_empty()).await;
        write_frame(&mut client, true, OPCODE_TEXT, b"destroy", true).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), task).await.expect("abort must not flush a blocked injection").unwrap();
        assert_eq!(server.read(&mut [0]).await.unwrap(), 0);
        let mut received = Vec::new();
        client.read_to_end(&mut received).await.unwrap();
        assert!(received.len() <= 64, "only bytes written before destroy may remain");
        assert!(state.ws_write.lock().unwrap().is_empty());
        assert!(state.ws_pause.lock().unwrap().is_empty());
    });
}
