use super::*;

type RawWire = (
    DuplexStream,
    DuplexStream,
    tokio::task::JoinHandle<io::Result<()>>,
);

fn raw(state: &Arc<AppState>, id: Option<u64>, flow: FrameFlow, script: Option<&str>) -> RawWire {
    let (client, c) = tokio::io::duplex(1024);
    let (server, s) = tokio::io::duplex(1024);
    let info = apply::build_req_info(
        "CONNECT",
        "tunnel",
        "db.test",
        5432,
        "/",
        &hyper::HeaderMap::new(),
        None,
    );
    let script = script.and_then(|s| {
        FrameScriptSpec::for_request(s.into(), &info, "db.test", &Default::default())
    });
    (
        client,
        server,
        tokio::spawn(inspected_relay_with_flow(
            c,
            s,
            script,
            flow,
            state.clone(),
            id,
        )),
    )
}

fn abort(state: &AppState, id: u64) {
    state.tcp_closed.lock().unwrap()[&id].send_replace(true);
}

async fn read(peer: &mut DuplexStream, expected: &[u8]) {
    let mut buf = vec![0; expected.len()];
    tokio::time::timeout(Duration::from_secs(3), peer.read_exact(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(buf, expected);
}

#[test]
fn rapid_ignore_pass_discards_the_old_chunk_but_not_a_later_pause() {
    rt().block_on(async {
        let state = state_with(Plugins::new());
        let (mut client, mut server, task) = raw(
            &state,
            Some(7),
            FrameFlow {
                send: DirMode::Pause,
                ..Default::default()
            },
            None,
        );
        let pause = gate_of(&state).await;
        client.write_all(b"old").await.unwrap();
        until("held", || pause.send.held() == 1).await;
        // No yield between transitions: Notify may coalesce them.
        pause.send.set_mode(DirMode::Ignore).unwrap();
        pause.send.set_mode(DirMode::Pass).unwrap();
        client.write_all(b"next").await.unwrap();
        read(&mut server, b"next").await;
        pause.send.set_mode(DirMode::Ignore).unwrap();
        pause.send.set_mode(DirMode::Pause).unwrap();
        client.write_all(b"fresh").await.unwrap();
        until("new held", || pause.send.held() == 1).await;
        pause.send.release();
        read(&mut server, b"fresh").await;
        abort(&state, 7);
        task.await.unwrap().unwrap();
        let frames = state.ws_frames.lock().unwrap();
        assert!(
            frames
                .iter()
                .any(|f| f.preview == "old" && f.ignored && !f.held)
        );
        assert!(
            frames
                .iter()
                .any(|f| f.preview == "fresh" && !f.ignored && !f.held)
        );
        assert!(state.ws_pause.lock().unwrap().is_empty());
    });
}

#[test]
fn pause_bounds_read_ahead_and_disconnect_releases_a_blocked_sender() {
    rt().block_on(async {
        let state = state_with(Plugins::new());
        let (mut client, mut server, task) = raw(
            &state,
            Some(7),
            FrameFlow {
                send: DirMode::Pause,
                ..Default::default()
            },
            None,
        );
        let pause = gate_of(&state).await;
        let sender = tokio::spawn(async move { client.write_all(&vec![42; 1024 * 1024]).await });
        until("held", || pause.send.held() == 1).await;
        assert!(
            !sender.is_finished(),
            "a paused reader must backpressure its peer"
        );
        {
            let frames = state.ws_frames.lock().unwrap();
            assert_eq!(frames.len(), 1);
            assert!(frames[0].held && frames[0].len <= 64 * 1024);
        }
        abort(&state, 7);
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(sender.await.unwrap().is_err());
        assert_eq!(server.read(&mut [0]).await.unwrap(), 0);
        assert!(state.tcp_closed.lock().unwrap().is_empty());
        assert!(state.ws_pause.lock().unwrap().is_empty());
    });
}

#[test]
fn disconnect_also_cancels_a_backpressured_writer_without_affecting_other_sessions() {
    rt().block_on(async {
        let state = state_with(Plugins::new());
        let (mut client, _blocked_server, task) = raw(&state, Some(7), FrameFlow::default(), None);
        gate_of(&state).await;
        let (mut other, mut peer, other_task) = raw(&state, Some(8), FrameFlow::default(), None);
        let sending = tokio::spawn(async move { client.write_all(&vec![42; 1024 * 1024]).await });
        until("writer filled peer", || {
            !state.ws_frames.lock().unwrap().is_empty()
        })
        .await;
        assert!(!sending.is_finished());
        abort(&state, 7);
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(sending.await.unwrap().is_err());
        other.write_all(b"unaffected").await.unwrap();
        read(&mut peer, b"unaffected").await;
        assert!(!state.ws_pause.lock().unwrap().contains_key(&7));
        abort(&state, 8);
        other_task.await.unwrap().unwrap();
    });
}

#[test]
fn paused_script_output_and_injections_preserve_the_existing_script_contract() {
    rt().block_on(async {
        let state = state_with(Plugins::new());
        let script = "var c = ctx; c.handleSendToServerFrame = function(buf, opts) { c.sendToClient('ack'); return 'S:' + buf; };";
        let (mut client, mut server, task) = raw(&state, Some(7), FrameFlow { send: DirMode::Pause, ..Default::default() }, Some(script));
        let pause = gate_of(&state).await;
        client.write_all(b"one").await.unwrap();
        read(&mut client, b"ack").await;
        until("transformed chunk held", || pause.send.held() == 1).await;
        assert!(state.ws_frames.lock().unwrap().iter().any(|f| f.held && f.preview == "S:one"));
        pause.send.release();
        read(&mut server, b"S:one").await;
        pause.send.set_mode(DirMode::Ignore).unwrap();
        client.write_all(b"two").await.unwrap();
        read(&mut client, b"ack").await;
        until("ignored transformed chunk", || state.ws_frames.lock().unwrap().iter().any(|f| f.ignored && f.preview == "S:two")).await;
        abort(&state, 7);
        task.await.unwrap().unwrap();
    });
}

#[test]
fn hidden_tunnel_rules_work_without_exposing_a_control_handle_or_capture() {
    rt().block_on(async {
        let state = state_with(Plugins::new());
        let (mut client, mut server, task) = raw(
            &state,
            None,
            FrameFlow {
                send: DirMode::Ignore,
                ..Default::default()
            },
            None,
        );
        client.write_all(b"hidden").await.unwrap();
        server.write_all(b"reply").await.unwrap();
        read(&mut client, b"reply").await;
        drop(client);
        assert_eq!(server.read(&mut [0]).await.unwrap(), 0);
        drop(server);
        task.await.unwrap().unwrap();
        assert!(state.ws_pause.lock().unwrap().is_empty());
        assert!(state.ws_frames.lock().unwrap().is_empty());
    });
}

#[test]
fn read_failure_cancels_a_stalled_reverse_writer_and_cleans_registration() {
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::ReadBuf;
    struct ResetPeer;
    impl AsyncRead for ResetPeer {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "fixture reset",
            )))
        }
    }
    impl AsyncWrite for ResetPeer {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }
    rt().block_on(async {
        let state = state_with(Plugins::new());
        let (mut client, io) = tokio::io::duplex(1024);
        client.write_all(b"blocked write").await.unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(3),
            inspected_relay(io, ResetPeer, None, state.clone(), Some(7)),
        )
        .await
        .expect("one failed direction must not wait for the other")
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
        assert!(state.ws_pause.lock().unwrap().is_empty());
        assert_eq!(client.read(&mut [0]).await.unwrap(), 0);
    });
}
