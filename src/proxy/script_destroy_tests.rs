use super::*;
use std::future::Future;
use std::task::Poll;

#[test]
fn destroy_answers_before_remaining_javascript_returns() {
    let (reply, mut answer) = tokio::sync::oneshot::channel();
    let (entered, waiting) = std::sync::mpsc::channel();
    let (release, continue_running) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut ctx = script_engine().unwrap();
        Rc::new(FrameControl {
            destroyed: Arc::new(AtomicBool::new(false)),
            reply: RefCell::new(Some(reply)),
        })
        .install(&mut ctx)
        .unwrap();
        // SAFETY: only Rust channels are captured, no GC-managed JS values.
        let wait = unsafe {
            NativeFunction::from_closure(move |_, _, _| {
                entered.send(()).unwrap();
                continue_running
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
                Ok(JsValue::undefined())
            })
        };
        ctx.register_global_callable(js_string!("waitForTest"), 0, wait)
            .unwrap();
        ctx.eval(Source::from_bytes(FRAME_GLUE.as_bytes())).unwrap();
        ctx.eval(Source::from_bytes(
            b"ctx.destroy();waitForTest();throw new Error('after');",
        ))
        .unwrap_err();
    });
    // No latency guess: user code is observably blocked after destroy returned.
    waiting.recv_timeout(Duration::from_secs(3)).unwrap();
    let outcome = answer.try_recv();
    release.send(()).unwrap();
    worker.join().unwrap();
    let outcome = outcome.expect("destroy must answer before the rest of JS finishes");
    assert!(matches!(outcome, ControlFlow::Break(sent) if sent.is_empty()));
}

#[tokio::test]
async fn a_queued_job_dropped_after_destroy_does_not_fall_back_to_forwarding() {
    let (jobs, inbox) = std::sync::mpsc::channel();
    let destroyed = Arc::new(AtomicBool::new(false));
    let script = FrameScript {
        jobs,
        handles: Arc::new(std::sync::atomic::AtomicU8::new(3)),
        destroyed: destroyed.clone(),
    };
    let reply = script.relay_controlled(ScriptFrame {
        to_server: false,
        data: b"queued".to_vec(),
        binary: false,
    });
    tokio::pin!(reply);
    assert!(std::future::poll_fn(|cx| Poll::Ready(reply.as_mut().poll(cx).is_pending())).await);
    let queued = inbox.try_recv().unwrap();
    destroyed.store(true, Ordering::Release);
    drop(queued);
    let ControlFlow::Continue(outcome) = reply.await else {
        panic!("only the destroying job owns termination");
    };
    assert!(outcome.frame.is_none());
    assert!(outcome.sent.is_empty());
}

#[tokio::test]
async fn output_only_entry_points_preserve_their_result_shapes() {
    let info = crate::proxy::apply::build_req_info(
        "GET",
        "ws",
        "fixture.test",
        80,
        "/",
        &hyper::HeaderMap::new(),
        None,
    );
    let spec = |text: &str| {
        FrameScriptSpec::for_request(text.into(), &info, "fixture.test", &Default::default())
            .unwrap()
    };
    // Exhaustive destructuring also guards the existing public struct shapes.
    let FrameScriptStart { script, sent } = FrameScript::start(spec(
        "ctx.handleSendToServerFrame=function(){ctx.destroy();};",
    ))
    .await;
    assert!(sent.is_empty());
    let FrameOutcome { sent, frame } = script
        .unwrap()
        .relay(ScriptFrame {
            to_server: true,
            data: b"drop".to_vec(),
            binary: false,
        })
        .await;
    assert!(sent.is_empty());
    assert!(frame.is_none());
    let FrameScriptStart { script, sent } = FrameScript::start(spec("ctx.destroy();")).await;
    assert!(script.is_none());
    assert!(sent.is_empty());
}
