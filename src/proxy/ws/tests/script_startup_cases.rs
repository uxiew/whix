use super::*;

#[test]
fn frame_script_helpers_are_ready_before_user_code_and_startup_frames_are_preserved() {
    rt().block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            let state = state_with(Plugins::new());
            let plan = plan_for(&state, "ws.test enable://websocket\n");
            // Inspect the prototype before invoking a Buffer stub. This pins
            // eager preparation without relying on a busy machine or timing.
            let script = r#"
                if (typeof Buffer.prototype.writeUInt32LE !== 'function') {
                    throw new Error('Buffer was not prepared before user code');
                }
                ctx.sendToServer(Buffer.from([0x41, 0x42]), { binary: true });
                ctx.sendToServer({ k: 'v' });
                ctx.sendToClient('to-client');
                ctx.sendToServer('');
            "#;
            let mut wire = spawn_tunnel(&state, plan, Some(script.to_string()));
            write_frame(&mut wire.client, true, OPCODE_TEXT, b"a", true)
                .await
                .unwrap();
            for (opcode, payload) in [
                (OPCODE_BINARY, &b"AB"[..]),
                (OPCODE_TEXT, &b"{\"k\":\"v\"}"[..]),
                (OPCODE_TEXT, &b"a"[..]),
            ] {
                let got = read_frame(&mut wire.server).await.unwrap().unwrap();
                assert_eq!((got.opcode, got.payload.as_slice()), (opcode, payload));
            }
            let got = read_frame(&mut wire.client).await.unwrap().unwrap();
            assert_eq!(got.opcode, OPCODE_TEXT);
            assert_eq!(got.payload, b"to-client");
            finish(wire).await;
            assert!(state.ws_write.lock().unwrap().is_empty());
        })
        .await
        .expect("script startup and tunnel cleanup must finish");
    });
}

#[test]
fn frame_script_startup_still_bounds_user_code_and_discards_its_pending_sends() {
    rt().block_on(async {
        tokio::time::timeout(
            crate::proxy::script::TIME_LIMIT + Duration::from_secs(3),
            async {
                let state = state_with(Plugins::new());
                let plan = plan_for(&state, "ws.test enable://websocket\n");
                // Calling a loop repeatedly defeats the per-call loop cap; the
                // user's top-level clock must still stop it after helpers load.
                let script = r#"
                ctx.sendToClient('discard on timeout');
                function f() { for (var i = 0; i < 2000000; i++) {} }
                for (var j = 0; j < 2000000; j++) f();
            "#;
                let mut wire = spawn_tunnel(&state, plan, Some(script.to_string()));
                let seen = through(&mut wire, &[(OPCODE_TEXT, b"a")]).await;
                assert_eq!(seen, [(OPCODE_TEXT, b"a".to_vec())]);
                assert!(stays_silent(&mut wire.client).await);
                finish(wire).await;
            },
        )
        .await
        .expect("preparing helpers must not remove the user's time limit");
    });
}
