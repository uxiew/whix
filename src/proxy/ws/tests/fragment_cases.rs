use super::*;

async fn next(peer: &mut DuplexStream, opcode: u8, data: &[u8]) {
    let got = tokio::time::timeout(Duration::from_secs(3), read_frame(peer))
        .await
        .expect("frame within deadline")
        .unwrap()
        .expect("not EOF");
    assert!(got.fin, "script output is one complete message");
    assert_eq!(got.opcode, opcode);
    assert_eq!(got.payload, data);
}

#[test]
fn fragmented_messages_reach_the_script_once_and_controls_still_pass() {
    rt().block_on(async {
        let state = state_with(Plugins::new());
        let plan = plan_for(&state, "ws.test enable://websocket\n");
        let script = "var n = 0; ctx.handleSendToServerFrame = function(buf) { return 'N' + (++n) + ':' + buf; };";
        let mut wire = spawn_tunnel(&state, plan, Some(script.into()));
        write_frame(&mut wire.client, false, OPCODE_TEXT, b"a", true).await.unwrap();
        write_frame(&mut wire.client, true, OPCODE_PING, b"ping", true).await.unwrap();
        next(&mut wire.server, OPCODE_PING, b"ping").await;
        write_frame(&mut wire.client, false, OPCODE_CONTINUATION, b"b", true).await.unwrap();
        write_frame(&mut wire.client, true, OPCODE_CONTINUATION, b"c", true).await.unwrap();
        next(&mut wire.server, OPCODE_TEXT, b"N1:abc").await;
        write_frame(&mut wire.client, true, OPCODE_TEXT, b"d", true).await.unwrap();
        next(&mut wire.server, OPCODE_TEXT, b"N2:d").await;
        finish(wire).await;
    });
}

#[test]
fn fragmented_binary_utf8_and_both_directions_keep_their_bytes() {
    rt().block_on(async {
        let state = state_with(Plugins::new());
        let plan = plan_for(&state, "ws.test enable://websocket\n");
        let script = "ctx.handleSendToServerFrame = function(buf) { return buf; }; ctx.handleSendToClientFrame = function(buf) { return buf; };";
        let mut wire = spawn_tunnel(&state, plan, Some(script.into()));
        write_frame(&mut wire.client, false, OPCODE_BINARY, &[0, 255], true).await.unwrap();
        write_frame(&mut wire.server, false, OPCODE_TEXT, &[0xe4], false).await.unwrap();
        write_frame(&mut wire.client, true, OPCODE_CONTINUATION, &[128], true).await.unwrap();
        write_frame(&mut wire.server, true, OPCODE_CONTINUATION, &[0xb8, 0xad], false).await.unwrap();
        next(&mut wire.server, OPCODE_BINARY, &[0, 255, 128]).await;
        next(&mut wire.client, OPCODE_TEXT, "中".as_bytes()).await;
        finish(wire).await;
    });
}

#[test]
fn fragmented_drop_and_handler_injections_do_not_poison_following_messages() {
    rt().block_on(async {
        let state = state_with(Plugins::new());
        let plan = plan_for(&state, "ws.test enable://websocket\n");
        let script = r#"var c = ctx;
            c.handleSendToServerFrame = function(buf, opts) {
                if (opts.frameScript) return buf;
                if (String(buf) === 'drop') return null;
                c.sendToServer('extra'); c.sendToClient('ack:' + buf); return buf;
            };"#;
        let mut wire = spawn_tunnel(&state, plan, Some(script.into()));
        for message in [b"drop", b"keep"] {
            write_frame(&mut wire.client, false, OPCODE_TEXT, &message[..2], true)
                .await
                .unwrap();
            write_frame(
                &mut wire.client,
                true,
                OPCODE_CONTINUATION,
                &message[2..],
                true,
            )
            .await
            .unwrap();
        }
        next(&mut wire.server, OPCODE_TEXT, b"extra").await;
        next(&mut wire.server, OPCODE_TEXT, b"keep").await;
        next(&mut wire.client, OPCODE_TEXT, b"ack:keep").await;
        write_frame(&mut wire.client, true, OPCODE_TEXT, b"next", true)
            .await
            .unwrap();
        next(&mut wire.server, OPCODE_TEXT, b"extra").await;
        next(&mut wire.server, OPCODE_TEXT, b"next").await;
        next(&mut wire.client, OPCODE_TEXT, b"ack:next").await;
        finish(wire).await;
    });
}

#[test]
fn fragmented_pause_release_and_ignore_use_complete_message_boundaries() {
    rt().block_on(async {
        let state = state_with(Plugins::new());
        let plan = plan_for(&state, "ws.test enable://websocket\n");
        let script = "ctx.handleSendToServerFrame = function(buf) { return 'S:' + buf; };";
        let mut wire = spawn_tunnel_with(
            &state,
            plan,
            Some(script.into()),
            FrameFlow {
                send: DirMode::Pause,
                ..Default::default()
            },
        );
        until("registered", || {
            state.ws_pause.lock().unwrap().contains_key(&7)
        })
        .await;
        let gate = state.ws_pause.lock().unwrap()[&7].clone();
        let writer = state.ws_write.lock().unwrap()[&7].clone();
        write_frame(&mut wire.client, false, OPCODE_TEXT, b"ab", true)
            .await
            .unwrap();
        until("partial buffered", || gate.send.fragmented()).await;
        assert!(gate.send.set_mode(DirMode::Ignore).is_err());
        assert!(!writer.send("send", b"mid-fragment").await);
        assert!(stays_silent(&mut wire.server).await);
        write_frame(&mut wire.client, true, OPCODE_CONTINUATION, b"c", true)
            .await
            .unwrap();
        until("complete message held", || gate.send.held() == 1).await;
        assert!(!gate.send.fragmented());
        assert_eq!(gate.send.release(), 1);
        next(&mut wire.server, OPCODE_TEXT, b"S:abc").await;
        until("released", || gate.send.held() == 0).await;
        assert!(writer.send("send", b"after").await);
        next(&mut wire.server, OPCODE_TEXT, b"after").await;

        gate.send.set_mode(DirMode::Ignore).unwrap();
        write_frame(&mut wire.client, false, OPCODE_TEXT, b"drop", true)
            .await
            .unwrap();
        until("ignored partial buffered", || gate.send.fragmented()).await;
        assert!(gate.send.set_mode(DirMode::Pass).is_err());
        write_frame(&mut wire.client, true, OPCODE_CONTINUATION, b"me", true)
            .await
            .unwrap();
        until("ignored message complete", || !gate.send.fragmented()).await;
        assert!(stays_silent(&mut wire.server).await);
        gate.send.set_mode(DirMode::Pass).unwrap();
        write_frame(&mut wire.client, true, OPCODE_TEXT, b"next", true)
            .await
            .unwrap();
        next(&mut wire.server, OPCODE_TEXT, b"S:next").await;
        finish(wire).await;
        let frames = state.ws_frames.lock().unwrap();
        assert!(frames.iter().any(|f| f.preview == "S:abc" && !f.held));
        assert!(frames.iter().any(|f| f.preview == "S:dropme" && f.ignored));
        assert!(!frames.iter().any(|f| f.opcode == "continuation"));
    });
}

#[test]
fn closing_during_reassembly_discards_the_partial_message_and_keeps_the_reply() {
    rt().block_on(async {
        let state = state_with(Plugins::new());
        let plan = plan_for(&state, "ws.test enable://websocket\n");
        let script = "ctx.handleSendToServerFrame = function(buf) { return 'unexpected'; };";
        let mut wire = spawn_tunnel(&state, plan, Some(script.into()));
        write_frame(&mut wire.client, false, OPCODE_TEXT, b"partial", true)
            .await
            .unwrap();
        let close = 1000u16.to_be_bytes();
        write_frame(&mut wire.client, true, OPCODE_CLOSE, &close, true)
            .await
            .unwrap();
        next(&mut wire.server, OPCODE_CLOSE, &close).await;
        write_frame(&mut wire.server, true, OPCODE_CLOSE, &close, false)
            .await
            .unwrap();
        next(&mut wire.client, OPCODE_CLOSE, &close).await;
        tokio::time::timeout(Duration::from_secs(3), wire.tunnel)
            .await
            .unwrap()
            .unwrap();
        assert!(state.ws_pause.lock().unwrap().is_empty());
        assert!(state.ws_write.lock().unwrap().is_empty());
        assert_eq!(state.ws_frames.lock().unwrap().len(), 2);
    });
}

#[test]
fn oversized_fragmented_messages_close_without_forwarding_a_prefix() {
    rt().block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            let state = state_with(Plugins::new());
            let plan = plan_for(&state, "ws.test enable://websocket\n");
            let script = "ctx.handleSendToServerFrame = function(buf) { return buf; };";
            let mut wire = spawn_tunnel(&state, plan, Some(script.into()));
            write_frame(
                &mut wire.client,
                false,
                OPCODE_BINARY,
                &vec![0; MAX_FRAME_BYTES],
                true,
            )
            .await
            .unwrap();
            write_frame(&mut wire.client, true, OPCODE_CONTINUATION, &[1], true)
                .await
                .unwrap();
            wire.tunnel.await.unwrap();
            assert!(read_frame(&mut wire.server).await.unwrap().is_none());
            assert!(state.ws_pause.lock().unwrap().is_empty());
            assert!(state.ws_write.lock().unwrap().is_empty());
            assert!(state.ws_frames.lock().unwrap().is_empty());
        })
        .await
        .expect("oversized reassembly must terminate");
    });
}

#[test]
fn eof_during_reassembly_drops_buffer_and_stale_writers() {
    rt().block_on(async {
        let state = state_with(Plugins::new());
        let plan = plan_for(&state, "ws.test enable://websocket\n");
        let script = "ctx.handleSendToServerFrame = function(buf) { return buf; };";
        let mut wire = spawn_tunnel(&state, plan, Some(script.into()));
        write_frame(&mut wire.client, false, OPCODE_TEXT, b"partial", true)
            .await
            .unwrap();
        until("registered", || {
            state.ws_write.lock().unwrap().contains_key(&7)
        })
        .await;
        let writer = state.ws_write.lock().unwrap()[&7].clone();
        drop(wire.client);
        tokio::time::timeout(Duration::from_secs(3), wire.tunnel)
            .await
            .unwrap()
            .unwrap();
        assert!(read_frame(&mut wire.server).await.unwrap().is_none());
        assert!(!writer.live());
        assert!(!writer.send("send", b"late").await);
        assert!(state.ws_write.lock().unwrap().is_empty());
    });
}

#[test]
fn plugins_see_one_script_result_not_unhandled_fragments() {
    rt().block_on(async {
        let state = state_hooking("whole", |f| {
            assert!(f.fin);
            assert_eq!(f.opcode, OPCODE_TEXT);
            assert_eq!(&f.payload[..], b"S:abc");
            Verdict::Replace(Bytes::from_static(b"plugin:S:abc"))
        });
        let plan = plan_for(&state, "ws.test pipe://whole\n");
        let script = "ctx.handleSendToServerFrame = function(buf) { return 'S:' + buf; };";
        let mut wire = spawn_tunnel(&state, plan, Some(script.into()));
        write_frame(&mut wire.client, false, OPCODE_TEXT, b"a", true)
            .await
            .unwrap();
        write_frame(&mut wire.client, true, OPCODE_CONTINUATION, b"bc", true)
            .await
            .unwrap();
        next(&mut wire.server, OPCODE_TEXT, b"plugin:S:abc").await;
        finish(wire).await;
        let frames = state.ws_frames.lock().unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].preview, "plugin:S:abc");
    });
}
