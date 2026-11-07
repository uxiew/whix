use super::*;
use crate::proxy::ws::tests::destroy_cases::{Recorded, destroy, destroy_script};
use crate::proxy::ws::tests::{rt, state_with};
use std::task::Poll;

#[test]
fn waiting_tcp_write_cannot_resume_after_destroy() {
    rt().block_on(async {
        let script = destroy_script().await;
        let bytes = Recorded::default();
        let pipe = TunnelPipe {
            to_server: tokio::sync::Mutex::new(tokio::io::sink()),
            to_client: tokio::sync::Mutex::new(bytes.clone()),
            script: Some(script.clone()),
            signal: tokio::sync::watch::channel(false).0,
            pause: Arc::new(SessionPause::new(FrameFlow::default())),
            state: state_with(Plugins::new()),
            session: Some(7),
        };
        let hold = pipe.to_client.lock().await;
        let write = pipe.write_bytes(false, b"queued");
        tokio::pin!(write);
        assert!(std::future::poll_fn(|cx| Poll::Ready(write.as_mut().poll(cx).is_pending())).await);
        destroy(&script).await;
        drop(hold);
        assert!(write.await.is_err());
        assert!(bytes.0.lock().unwrap().is_empty());
    });
}

#[test]
fn tcp_destroy_cancels_backpressure_and_a_blocked_preceding_injection() {
    rt().block_on(async {
        let state = state_with(Plugins::new());
        let (mut client, client_io) = tokio::io::duplex(64);
        let (mut server, origin_io) = tokio::io::duplex(16 * 1024);
        let info = crate::proxy::apply::build_req_info("CONNECT", "tunnel", "fixture.test", 1234, "/", &hyper::HeaderMap::new(), None);
        let spec = FrameScriptSpec::for_request(
            "ctx.handleSendToServerFrame=function(){ctx.sendToClient('queued injection');ctx.destroy();};".into(), &info, "fixture.test", &Default::default());
        let task = tokio::spawn(inspected_relay(client_io, origin_io, spec, state.clone(), Some(7)));
        server.write_all(&[42; 8192]).await.unwrap();
        // Consume exactly the first full buffer, then let it fill again. This
        // proves a real write is in progress without relying on a fixed sleep.
        let mut first = [0; 64];
        client.read_exact(&mut first).await.unwrap();
        assert_eq!(first, [42; 64]);
        client.write_all(b"destroy").await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), task).await.expect("abort must not wait for the other writer").unwrap().unwrap();
        assert_eq!(server.read(&mut [0]).await.unwrap(), 0);
        let mut rest = Vec::new(); client.read_to_end(&mut rest).await.unwrap();
        assert!(rest.len() <= 64);
        assert!(state.tcp_closed.lock().unwrap().is_empty());
        assert!(state.ws_pause.lock().unwrap().is_empty());
    });
}
