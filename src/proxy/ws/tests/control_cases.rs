use super::*;

#[test]
fn live_modes_pause_ignore_and_resume_each_direction_without_reordering() {
    rt().block_on(async {
        for dir in [Dir::Send, Dir::Receive] {
            let state = state_with(Plugins::new());
            let mut wire = spawn_tunnel(&state, FramePlan::default(), None);
            let pause = gate_of(&state).await;
            let gate = pause.of(dir);
            let (source, destination) = if dir == Dir::Send {
                (&mut wire.client, &mut wire.server)
            } else {
                (&mut wire.server, &mut wire.client)
            };
            gate.set_mode(DirMode::Pause).unwrap();
            write_frame(source, true, OPCODE_BINARY, b"held", dir == Dir::Send)
                .await
                .unwrap();
            until("held", || gate.held() == 1).await;
            assert!(
                tokio::time::timeout(Duration::from_millis(20), read_frame(destination))
                    .await
                    .is_err()
            );
            gate.set_mode(DirMode::Ignore).unwrap();
            until("queue discarded", || gate.held() == 0).await;
            assert!(state.ws_frames.lock().unwrap()[0].ignored);
            write_frame(source, true, OPCODE_BINARY, b"ignored", dir == Dir::Send)
                .await
                .unwrap();
            until("ignored frame captured", || {
                state.ws_frames.lock().unwrap().len() == 2
            })
            .await;
            gate.set_mode(DirMode::Pass).unwrap();
            write_frame(source, true, OPCODE_BINARY, b"live", dir == Dir::Send)
                .await
                .unwrap();
            assert_eq!(
                read_frame(destination).await.unwrap().unwrap().payload,
                b"live"
            );
            finish(wire).await;
        }
    });
}

#[test]
fn binary_injection_bypasses_pause_and_is_masked_only_toward_the_server() {
    rt().block_on(async {
        let state = state_with(Plugins::new());
        let mut wire = spawn_tunnel(&state, FramePlan::default(), None);
        let gate = gate_of(&state).await;
        gate.send.set_mode(DirMode::Pause).unwrap();
        gate.receive.set_mode(DirMode::Ignore).unwrap();
        let writers = state.ws_write.lock().unwrap()[&7].clone();
        let payload = [0, 255, 128];
        for (dir, peer, masked) in [
            ("send", &mut wire.server, true),
            ("receive", &mut wire.client, false),
        ] {
            assert!(writers.send_binary(dir, &payload).await);
            let mut header = [0; 2];
            peer.read_exact(&mut header).await.unwrap();
            assert_eq!(header, [0x82, if masked { 0x83 } else { 3 }]);
            let mut key = [0; 4];
            if masked {
                peer.read_exact(&mut key).await.unwrap();
            }
            let mut bytes = [0; 3];
            peer.read_exact(&mut bytes).await.unwrap();
            for (i, byte) in bytes.iter_mut().enumerate() {
                *byte ^= key[i % 4];
            }
            assert_eq!(bytes, payload);
        }
        finish(wire).await;
    });
}

#[test]
fn short_writes_and_injected_frames_never_interleave_or_lose_the_lock_wakeup() {
    rt().block_on(async {
        let (server, mut receiver) = tokio::io::duplex(7);
        let (client, _unused) = tokio::io::duplex(7);
        let writers = Arc::new(SessionWriters::new(server, client));
        let mut relay = writers.to_server();
        let forwarding = tokio::spawn(async move {
            write_frame(&mut relay, true, OPCODE_BINARY, &vec![65; 16384], true)
                .await
                .unwrap();
        });
        // Observe a partial write before injecting, rather than relying on
        // scheduler order. Put that byte back for the full-frame assertions.
        let first = receiver.read_u8().await.unwrap();
        let mut receiver = std::io::Cursor::new([first]).chain(receiver);
        let injecting = tokio::spawn(async move {
            assert!(writers.send("send", b"injected").await);
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            assert_eq!(
                read_frame(&mut receiver).await.unwrap().unwrap().payload,
                vec![65; 16384]
            );
            assert_eq!(
                read_frame(&mut receiver).await.unwrap().unwrap().payload,
                b"injected"
            );
            forwarding.await.unwrap();
            injecting.await.unwrap();
        })
        .await
        .expect("atomic frames, no lost mutex waiter");
    });
}

#[test]
fn abort_cancels_partial_read_and_parent_cancellation_cleans_registrations() {
    rt().block_on(async {
        for parent_cancel in [false, true] {
            let state = state_with(Plugins::new());
            let mut wire = spawn_tunnel(&state, FramePlan::default(), None);
            gate_of(&state).await;
            wire.client.write_all(&[0x81, 0xfe, 0]).await.unwrap();
            let writers = state.ws_write.lock().unwrap()[&7].clone();
            if parent_cancel {
                wire.tunnel.abort();
            } else {
                writers.abort();
            }
            let _ = tokio::time::timeout(Duration::from_secs(2), &mut wire.tunnel)
                .await
                .unwrap();
            assert!(state.ws_pause.lock().unwrap().is_empty());
            assert!(state.ws_write.lock().unwrap().is_empty());
            for peer in [&mut wire.client, &mut wire.server] {
                let mut byte = [0];
                assert_eq!(
                    tokio::time::timeout(Duration::from_secs(2), peer.read(&mut byte))
                        .await
                        .unwrap()
                        .unwrap(),
                    0
                );
            }
            assert!(!writers.send("send", b"closed").await);
        }
    });
}

#[test]
fn controls_do_not_inject_into_or_discard_half_a_fragmented_message() {
    rt().block_on(async {
        let state = state_with(Plugins::new());
        let mut wire = spawn_tunnel(&state, FramePlan::default(), None);
        let pause = gate_of(&state).await;
        write_frame(&mut wire.client, false, OPCODE_BINARY, b"first", true)
            .await
            .unwrap();
        read_frame(&mut wire.server).await.unwrap().unwrap();
        assert!(pause.send.set_mode(DirMode::Ignore).is_err());
        let writers = state.ws_write.lock().unwrap()[&7].clone();
        assert!(!writers.send_binary("send", b"would interrupt").await);
        assert!(writers.live());
        pause.send.set_mode(DirMode::Pause).unwrap();
        write_frame(&mut wire.client, true, OPCODE_CONTINUATION, b"last", true)
            .await
            .unwrap();
        until("last fragment held", || pause.send.held() == 1).await;
        assert!(pause.send.set_mode(DirMode::Ignore).is_err());
        pause.send.release();
        assert_eq!(
            read_frame(&mut wire.server).await.unwrap().unwrap().payload,
            b"last"
        );
        until("fragment ended", || !pause.send.fragmented()).await;
        pause.send.set_mode(DirMode::Ignore).unwrap();
        finish(wire).await;
    });
}

#[test]
fn advertised_huge_frame_is_rejected_before_allocating_its_payload() {
    rt().block_on(async {
        let mut bytes = &[0x82, 127, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff][..];
        assert_eq!(
            read_frame(&mut bytes).await.err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
    });
}

#[test]
fn rapid_ignore_then_pass_cannot_resurrect_a_held_frame() {
    rt().block_on(async {
        let state = state_with(Plugins::new());
        let mut wire = spawn_tunnel(&state, FramePlan::default(), None);
        let pause = gate_of(&state).await;
        pause.send.set_mode(DirMode::Pause).unwrap();
        write_frame(&mut wire.client, true, OPCODE_BINARY, b"discard", true)
            .await
            .unwrap();
        until("held", || pause.send.held() == 1).await;
        // No yield: Notify may merge these, but their effects must not merge.
        pause.send.set_mode(DirMode::Ignore).unwrap();
        pause.send.set_mode(DirMode::Pass).unwrap();
        write_frame(&mut wire.client, true, OPCODE_BINARY, b"kept", true)
            .await
            .unwrap();
        assert_eq!(
            read_frame(&mut wire.server).await.unwrap().unwrap().payload,
            b"kept"
        );
        assert!(state.ws_frames.lock().unwrap()[0].ignored);
        finish(wire).await;
    });
}

#[test]
fn normal_close_waits_for_the_reply_in_both_directions_including_a_held_close() {
    rt().block_on(async {
        for dir in [Dir::Send, Dir::Receive] {
            for paused in [false, true] {
                let state = state_with(Plugins::new());
                let mut wire = spawn_tunnel(&state, FramePlan::default(), None);
                let pause = gate_of(&state).await;
                if paused {
                    pause.of(dir).set_mode(DirMode::Pause).unwrap();
                }
                let (sender, receiver) = if dir == Dir::Send {
                    (&mut wire.client, &mut wire.server)
                } else {
                    (&mut wire.server, &mut wire.client)
                };
                write_frame(sender, true, OPCODE_CLOSE, &[3, 232], dir == Dir::Send)
                    .await
                    .unwrap();
                if paused {
                    until("close held", || pause.of(dir).held() == 1).await;
                    pause.of(dir).set_mode(DirMode::Pass).unwrap();
                }
                assert_eq!(
                    read_frame(receiver).await.unwrap().unwrap().opcode,
                    OPCODE_CLOSE
                );
                assert!(
                    !wire.tunnel.is_finished(),
                    "a Close frame is not a completed handshake"
                );
                let reply_dir = if dir == Dir::Send {
                    Dir::Receive
                } else {
                    Dir::Send
                };
                if paused {
                    pause.of(reply_dir).set_mode(DirMode::Pause).unwrap();
                }
                write_frame(receiver, true, OPCODE_CLOSE, &[3, 232], dir == Dir::Receive)
                    .await
                    .unwrap();
                if paused {
                    until("reply held", || pause.of(reply_dir).held() == 1).await;
                    pause.of(reply_dir).set_mode(DirMode::Pass).unwrap();
                }
                let reply = tokio::time::timeout(Duration::from_secs(1), read_frame(sender))
                    .await
                    .unwrap()
                    .unwrap()
                    .expect("close reply");
                assert_eq!(reply.opcode, OPCODE_CLOSE);
                assert_eq!(reply.payload, [3, 232]);
                tokio::time::timeout(Duration::from_secs(1), &mut wire.tunnel)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(state.ws_write.lock().unwrap().is_empty());
                assert!(state.ws_pause.lock().unwrap().is_empty());
            }
        }
    });
}

#[test]
fn closing_rejects_new_injections_but_abort_and_eof_still_cancel_immediately() {
    rt().block_on(async {
        for abort in [false, true] {
            let state = state_with(Plugins::new());
            let mut wire = spawn_tunnel(&state, FramePlan::default(), None);
            gate_of(&state).await;
            let writers = state.ws_write.lock().unwrap()[&7].clone();
            write_frame(&mut wire.client, true, OPCODE_CLOSE, &[3, 232], true)
                .await
                .unwrap();
            assert_eq!(
                read_frame(&mut wire.server).await.unwrap().unwrap().opcode,
                OPCODE_CLOSE
            );
            until("closing", || writers.closing.load(Ordering::Acquire)).await;
            assert!(
                writers.live(),
                "Release and Disconnect remain available while closing"
            );
            assert!(!writers.send("receive", b"after Close").await);
            assert!(!wire.tunnel.is_finished());
            if abort {
                writers.abort();
            } else {
                wire.server.shutdown().await.unwrap();
            }
            tokio::time::timeout(Duration::from_secs(1), &mut wire.tunnel)
                .await
                .unwrap()
                .unwrap();
            assert!(state.ws_write.lock().unwrap().is_empty());
            assert!(state.ws_pause.lock().unwrap().is_empty());
        }
    });
}

#[test]
fn an_unanswered_close_has_a_bounded_grace_period() {
    rt().block_on(async {
        let state = state_with(Plugins::new());
        let mut wire = spawn_tunnel(&state, FramePlan::default(), None);
        gate_of(&state).await;
        write_frame(&mut wire.client, true, OPCODE_CLOSE, &[3, 232], true)
            .await
            .unwrap();
        assert_eq!(
            read_frame(&mut wire.server).await.unwrap().unwrap().opcode,
            OPCODE_CLOSE
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut wire.tunnel)
                .await
                .is_err()
        );
        tokio::time::timeout(CLOSE_GRACE + Duration::from_secs(1), &mut wire.tunnel)
            .await
            .unwrap()
            .unwrap();
        assert!(state.ws_write.lock().unwrap().is_empty());
        assert!(state.ws_pause.lock().unwrap().is_empty());
        assert_eq!(
            wire.client.read_u8().await.unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(
            wire.server.read_u8().await.unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    });
}
