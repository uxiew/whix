use super::*;

#[test]
fn hiding_keeps_script_and_plugin_execution_without_capturing_their_output() {
    rt().block_on(async {
        let state = state_hooking("upper", |frame| {
            Verdict::Replace(Bytes::from(
                frame
                    .payload
                    .iter()
                    .map(u8::to_ascii_uppercase)
                    .collect::<Vec<_>>(),
            ))
        });
        let plan = plan_for(&state, "ws.test enable://hide plugin://upper");
        assert!(!plan.is_empty(), "the plugin must still be selected");
        let script = "ctx.handleSendToServerFrame=function(buf){return 'script:'+buf;};";
        let mut wire = spawn_tunnel(&state, plan, Some(script.into()));
        write_frame(&mut wire.client, true, OPCODE_TEXT, b"secret", true)
            .await
            .unwrap();
        let received = read_frame(&mut wire.server).await.unwrap().unwrap();
        assert_eq!(received.payload, b"SCRIPT:SECRET");
        write_frame(&mut wire.server, true, OPCODE_TEXT, b"reply", false)
            .await
            .unwrap();
        assert_eq!(
            read_frame(&mut wire.client).await.unwrap().unwrap().payload,
            b"REPLY"
        );
        assert!(state.ws_frames.lock().unwrap().is_empty());
        assert!(state.ws_write.lock().unwrap().is_empty());
        assert!(state.ws_pause.lock().unwrap().is_empty());
        finish(wire).await;
        assert!(state.ws_frames.lock().unwrap().is_empty());
    });
}

#[test]
fn hidden_ignore_still_drops_data_and_passes_controls_without_recording_either() {
    rt().block_on(async {
        let state = state_with(Plugins::new());
        let rule = "ws.test enable://hide|ignoreSend";
        let mut wire = spawn_tunnel_with(&state, plan_for(&state, rule), None, flow_for(rule));
        write_frame(&mut wire.client, true, OPCODE_TEXT, b"discard", true)
            .await
            .unwrap();
        write_frame(&mut wire.client, true, OPCODE_PING, b"barrier", true)
            .await
            .unwrap();
        let received = read_frame(&mut wire.server).await.unwrap().unwrap();
        assert_eq!(received.opcode, OPCODE_PING);
        assert_eq!(received.payload, b"barrier");
        assert!(state.ws_frames.lock().unwrap().is_empty());
        assert!(state.ws_write.lock().unwrap().is_empty());
        assert!(state.ws_pause.lock().unwrap().is_empty());
        finish(wire).await;
    });
}

#[test]
fn hidden_pause_keeps_its_hold_without_exposing_it_and_eof_still_tears_down() {
    rt().block_on(async {
        let state = state_with(Plugins::new());
        let rule = "ws.test enable://hide|pauseReceive";
        let mut wire = spawn_tunnel_with(&state, plan_for(&state, rule), None, flow_for(rule));
        write_frame(&mut wire.server, true, OPCODE_BINARY, &[0, 255], false)
            .await
            .unwrap();
        assert!(stays_silent(&mut wire.client).await);
        write_frame(
            &mut wire.client,
            true,
            OPCODE_TEXT,
            b"opposite direction",
            true,
        )
        .await
        .unwrap();
        assert_eq!(
            read_frame(&mut wire.server).await.unwrap().unwrap().payload,
            b"opposite direction"
        );
        assert!(state.ws_frames.lock().unwrap().is_empty());
        assert!(state.ws_write.lock().unwrap().is_empty());
        assert!(state.ws_pause.lock().unwrap().is_empty());
        drop(wire.server);
        tokio::time::timeout(Duration::from_secs(3), wire.tunnel)
            .await
            .unwrap()
            .unwrap();
        assert!(read_frame(&mut wire.client).await.unwrap().is_none());
        assert!(state.ws_frames.lock().unwrap().is_empty());
    });
}
