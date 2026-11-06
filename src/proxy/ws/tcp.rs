//! Inspected TCP bytes: scripts, one held chunk per direction, and half-close.
//! Reuses the console's direction controls, not WebSocket framing or timers.

use super::*;

const CHUNK: usize = 64 * 1024;

/// Inspect raw TCP using initially passing directions. Preserves the public
/// entry point for embedders; rule-aware callers pass flow explicitly below.
pub async fn inspected_relay<C, O>(
    client: C,
    origin: O,
    script: Option<FrameScriptSpec>,
    state: Arc<AppState>,
    session: Option<u64>,
) -> io::Result<()>
where
    C: AsyncRead + AsyncWrite + Unpin,
    O: AsyncRead + AsyncWrite + Unpin,
{
    inspected_relay_with_flow(client, origin, script, FrameFlow::default(), state, session).await
}

pub(crate) async fn inspected_relay_with_flow<C, O>(
    client: C,
    origin: O,
    script: Option<FrameScriptSpec>,
    flow: FrameFlow,
    state: Arc<AppState>,
    session: Option<u64>,
) -> io::Result<()>
where
    C: AsyncRead + AsyncWrite + Unpin,
    O: AsyncRead + AsyncWrite + Unpin,
{
    let started = match script {
        Some(spec) => FrameScript::start(spec).await,
        None => FrameScriptStart::default(),
    };
    let pause = Arc::new(SessionPause::new(flow));
    let signal = tokio::sync::watch::channel(false).0;
    if let Some(id) = session {
        state.ws_pause.lock().unwrap().insert(id, pause.clone());
        state.tcp_closed.lock().unwrap().insert(id, signal.clone());
    }
    let _registration = Registration {
        state: state.clone(),
        session,
        signal: signal.clone(),
    };
    let mut closed = signal.subscribe();
    let (client_read, client_write) = tokio::io::split(client);
    let (origin_read, origin_write) = tokio::io::split(origin);
    let pipe = TunnelPipe {
        to_server: tokio::sync::Mutex::new(origin_write),
        to_client: tokio::sync::Mutex::new(client_write),
        script: started.script,
        pause,
        state,
        session,
    };
    let relay = async {
        // Script injections bypass peer holds, as they do for WebSocket.
        for sent in &started.sent {
            pipe.write(sent.to_server, &sent.data).await?;
        }
        // A normal EOF shuts down only its write half. An I/O error or explicit
        // disconnect cancels both directions, including a backpressured writer.
        tokio::try_join!(pipe.pump(client_read, true), pipe.pump(origin_read, false))?;
        Ok(())
    };
    tokio::select! {
        result = relay => result,
        _ = closed.wait_for(|v| *v) => Ok(()),
    }
}

struct Registration {
    state: Arc<AppState>,
    session: Option<u64>,
    signal: tokio::sync::watch::Sender<bool>,
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.signal.send_replace(true);
        if let Some(id) = self.session {
            self.state.ws_pause.lock().unwrap().remove(&id);
            self.state.tcp_closed.lock().unwrap().remove(&id);
        }
    }
}

fn direction(to_server: bool) -> &'static str {
    if to_server { "send" } else { "receive" }
}

struct TunnelPipe<S, C> {
    to_server: tokio::sync::Mutex<S>,
    to_client: tokio::sync::Mutex<C>,
    script: Option<FrameScript>,
    pause: Arc<SessionPause>,
    state: Arc<AppState>,
    /// The session the chunks are filed under; `None` for a hidden tunnel,
    /// whose script runs and whose chunks are not kept.
    session: Option<u64>,
}

impl<S, C> TunnelPipe<S, C>
where
    S: AsyncWrite + Unpin,
    C: AsyncWrite + Unpin,
{
    /// Write one chunk toward either end, and record it as a frame.
    async fn write(&self, to_server: bool, data: &[u8]) -> io::Result<()> {
        self.write_bytes(to_server, data).await?;
        self.record(to_server, data, false, false);
        Ok(())
    }

    async fn write_bytes(&self, to_server: bool, data: &[u8]) -> io::Result<()> {
        if to_server {
            let mut w = self.to_server.lock().await;
            w.write_all(data).await?;
            w.flush().await?;
        } else {
            let mut w = self.to_client.lock().await;
            w.write_all(data).await?;
            w.flush().await?;
        }
        Ok(())
    }

    fn record(&self, to_server: bool, data: &[u8], held: bool, ignored: bool) {
        if let Some(session) = self.session {
            let opcode = if std::str::from_utf8(data).is_ok() {
                OPCODE_TEXT
            } else {
                OPCODE_BINARY
            };
            let mut frame = WsFrame::new(session, direction(to_server), opcode, data);
            frame.held = held;
            frame.ignored = ignored;
            self.state.record_frame(frame);
        }
    }

    /// A single chunk is enough to display a pause. Stop reading after it;
    /// unlike WebSocket, raw TCP has no safe synthetic keep-alive bytes.
    async fn transfer(&self, to_server: bool, data: &[u8]) -> io::Result<()> {
        let gate = self
            .pause
            .of(if to_server { Dir::Send } else { Dir::Receive });
        let mode = {
            let flow = gate.flow.lock().unwrap();
            // Discards before this chunk existed must not affect it. Taking the
            // mode and clearing that history share set_mode's transition lock.
            gate.discard_held.store(false, Ordering::Release);
            if flow.0 == DirMode::Pause {
                gate.held.store(1, Ordering::Release);
            }
            flow.0
        };
        if mode == DirMode::Pass {
            return self.write(to_server, data).await;
        }
        self.record(
            to_server,
            data,
            mode == DirMode::Pause,
            mode == DirMode::Ignore,
        );
        if mode == DirMode::Ignore {
            return Ok(());
        }
        let discard = loop {
            let mode = {
                let flow = gate.flow.lock().unwrap();
                if gate.discard_held.swap(false, Ordering::AcqRel) {
                    DirMode::Ignore
                } else {
                    flow.0
                }
            };
            match mode {
                DirMode::Pass => break false,
                DirMode::Ignore => break true,
                DirMode::Pause => gate.released().await,
            }
        };
        if !discard {
            self.write_bytes(to_server, data).await?;
        }
        if let Some(session) = self.session {
            if discard {
                ignore_held_marks(&self.state, session, direction(to_server), 1);
            } else {
                clear_held_marks(&self.state, session, direction(to_server), 1);
            }
        }
        gate.held.store(0, Ordering::Release);
        Ok(())
    }

    /// No more bytes will come this way: tell the far end, as a plain relay's
    /// half-close does.
    async fn finish(&self, to_server: bool) {
        if to_server {
            let _ = self.to_server.lock().await.shutdown().await;
        } else {
            let _ = self.to_client.lock().await.shutdown().await;
        }
    }

    /// One direction: read a chunk, let the script see it, write what is left.
    async fn pump<R: AsyncRead + Unpin>(&self, mut from: R, to_server: bool) -> io::Result<()> {
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = match from.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(err) => return Err(err),
            };
            let chunk = &buf[..n];
            let Some(script) = self.script.as_ref().filter(|s| s.handles(to_server)) else {
                self.transfer(to_server, chunk).await?;
                continue;
            };
            let outcome = script
                .relay(ScriptFrame {
                    to_server,
                    data: chunk.to_vec(),
                    binary: true,
                })
                .await;
            for sent in &outcome.sent {
                self.write(sent.to_server, &sent.data).await?;
            }
            if let Some(kept) = outcome.frame {
                self.transfer(to_server, &kept.data).await?;
            }
        }
        self.finish(to_server).await;
        Ok(())
    }
}
