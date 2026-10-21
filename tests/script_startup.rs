//! Trusted initialization must finish before a user's execution budget starts.
use std::collections::HashMap;
use std::sync::{Arc, Barrier, Mutex};
use whix::proxy::script::{
    RulesScriptCtx, RulesScriptRes, eval_pac, produce_rules, run_res_script,
};

#[derive(Clone, Copy, Debug)]
enum Kind {
    Request,
    ResponseRules,
    ResponseBody,
    Pac,
}

fn rules(src: &str, response: bool) -> Result<String, String> {
    let values = HashMap::new();
    let data = Mutex::new(serde_json::Value::Null);
    let env = whix::rules::ScriptEnv::default();
    let ctx = RulesScriptCtx {
        method: "GET",
        full_url: "http://script.test/",
        headers: &[],
        body: "",
        client_ip: None,
        client_port: None,
        res: response.then_some(RulesScriptRes {
            status: 201,
            server_ip: None,
            headers: &[],
        }),
        values: &values,
        script_data: &data,
        pattern: "script.test",
        env: &env,
    };
    produce_rules(src, &ctx)
        .map(|r| r.rules)
        .map_err(|e| e.to_string())
}

const PAC: &str = r#"
    var ready = typeof isInNet === 'function' && typeof shExpMatch === 'function'
        && typeof dnsDomainIs === 'function' && typeof Buffer === 'undefined';
    function FindProxyForURL(url, host) {
        if (!ready || !isInNet('127.0.0.1', '127.0.0.0', '255.0.0.0')
            || !shExpMatch(url, 'http://script.test/*') || !dnsDomainIs(host, '.test')) {
            throw new Error('PAC helpers not ready');
        }
        return 'PROXY 127.0.0.1:9';
    }
"#;

fn run(kind: Kind, inspect_readiness: bool) -> Result<(), String> {
    if matches!(kind, Kind::Pac) {
        let answer =
            eval_pac(PAC, "http://script.test/", "script.test").map_err(|e| e.to_string())?;
        return if answer == "PROXY 127.0.0.1:9" {
            Ok(())
        } else {
            Err(answer)
        };
    }
    // A deterministic guard, separate from the contention experiment. Reading
    // the prototype must not require a prior call to a lazy Buffer stub.
    let guard = if inspect_readiness {
        "if (typeof Buffer.prototype.writeUInt32LE !== 'function') throw new Error('helpers not ready');"
    } else {
        ""
    };
    let (actual, expected) = match kind {
        Kind::Request => (
            rules(
                &format!(
                    "{guard} rules.push('* reqHeaders://x-probe=' + Buffer.from('AB').toString());"
                ),
                false,
            )?,
            "* reqHeaders://x-probe=AB",
        ),
        Kind::ResponseRules => (
            rules(
                &format!(
                    "{guard} rules.push('* resHeaders://x-probe=' + Buffer.from('AB').toString() + ':' + statusCode);"
                ),
                true,
            )?,
            "* resHeaders://x-probe=AB:201",
        ),
        Kind::ResponseBody => {
            let r = run_res_script(&format!("{guard} ctx.res.body = Buffer.from('AB').toString(); ctx.res.statusCode = 201; ctx.res.headers['x-probe'] = 'ready';"), "GET", "http://script.test/", 200, &[], "original").map_err(|e| e.to_string())?;
            (
                format!(
                    "{}:{}:{}",
                    r.status.unwrap_or(0),
                    r.body.unwrap_or_default(),
                    r.headers
                        .iter()
                        .any(|(k, v)| k == "x-probe" && v == "ready")
                ),
                "201:AB:true",
            )
        }
        Kind::Pac => unreachable!(),
    };
    if actual == expected {
        Ok(())
    } else {
        Err(format!("expected {expected}, got {actual}"))
    }
}

#[test]
fn request_helpers_are_ready_before_user_code() {
    run(Kind::Request, true).unwrap();
}
#[test]
fn response_rules_helpers_are_ready_before_user_code() {
    run(Kind::ResponseRules, true).unwrap();
}
#[test]
fn response_body_helpers_are_ready_before_user_code() {
    run(Kind::ResponseBody, true).unwrap();
}
#[test]
fn pac_helpers_are_ready_without_a_node_environment() {
    run(Kind::Pac, true).unwrap();
}

// A broken engine deadline must fail the test, not keep the test process in
// the runaway JavaScript. Results cross the thread boundary as owned text.
fn error_with_watchdog(f: impl FnOnce() -> String + Send + 'static) -> String {
    let (send, recv) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _ = send.send(f());
    });
    let result = recv
        .recv_timeout(std::time::Duration::from_secs(4))
        .expect("script must report its timeout within the watchdog");
    worker.join().expect("execution thread");
    result
}

#[test]
fn pac_file_and_call_timeouts_are_errors_not_direct_routes() {
    let loops = "function spin() { for (var i=0; i<2000000; i++) {} } for (var j=0; j<2000000; j++) spin();";
    for src in [
        format!("{loops} function FindProxyForURL() {{ return 'DIRECT'; }}"),
        format!("function FindProxyForURL() {{ {loops} return 'DIRECT'; }}"),
    ] {
        let error = error_with_watchdog(move || {
            eval_pac(&src, "http://script.test/", "script.test")
                .unwrap_err()
                .to_string()
        });
        assert!(error.contains("still running"), "{error}");
    }
}

#[test]
fn node_scripts_still_timeout_without_returning_partial_changes() {
    let loops = "function spin() { for (var i=0; i<2000000; i++) {} } for (var j=0; j<2000000; j++) spin();";
    for response in [false, true] {
        let error = error_with_watchdog(move || {
            rules(
                &format!("rules.push('* reqHeaders://x-partial=yes'); {loops}"),
                response,
            )
            .unwrap_err()
        });
        assert!(error.contains("still running"), "{error}");
    }
    let error = error_with_watchdog(move || {
        run_res_script(
            &format!("ctx.res.body = 'partial'; {loops}"),
            "GET",
            "http://script.test/",
            200,
            &[],
            "original",
        )
        .err()
        .expect("must time out")
        .to_string()
    });
    assert!(error.contains("still running"), "{error}");
}

fn bounded_env(name: &str, default: usize, max: usize) -> usize {
    let n = std::env::var(name)
        .map(|s| s.parse::<usize>().expect("integer parameter"))
        .unwrap_or(default);
    assert!((1..=max).contains(&n), "{name} must be in 1..={max}");
    n
}

#[test]
#[ignore = "bounded CPU contention probe; run separately from other tests"]
fn concurrent_script_startup_keeps_results() {
    let threads = bounded_env("SCRIPT_STARTUP_THREADS", 128, 128);
    let rounds = bounded_env("SCRIPT_STARTUP_ROUNDS", 5, 10);
    let mut failures = Vec::new();
    for kind in [
        Kind::Request,
        Kind::ResponseRules,
        Kind::ResponseBody,
        Kind::Pac,
    ] {
        let mut errors = Vec::new();
        for _ in 0..rounds {
            let barrier = Arc::new(Barrier::new(threads));
            let workers: Vec<_> = (0..threads)
                .map(|_| {
                    let barrier = barrier.clone();
                    std::thread::Builder::new()
                        .stack_size(8 * 1024 * 1024)
                        .spawn(move || {
                            barrier.wait();
                            run(kind, false)
                        })
                        .expect("worker")
                })
                .collect();
            for worker in workers {
                if let Err(error) = worker.join().expect("worker panic") {
                    errors.push(error);
                }
            }
        }
        println!(
            "{kind:?}: runs={} failed={} timeouts={} first={:?}",
            threads * rounds,
            errors.len(),
            errors
                .iter()
                .filter(|e| e.contains("still running"))
                .count(),
            errors.first()
        );
        if !errors.is_empty() {
            failures.push(format!("{kind:?}: {} failures", errors.len()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join(", "));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn response_body_hook_applies_prepared_helpers_on_the_wire() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let origin = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            head.push(byte[0]);
        }
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 8\r\nConnection: close\r\n\r\noriginal").await.unwrap();
    });
    let dir = std::env::temp_dir().join(format!(
        "whix-script-startup-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let text = format!(
        r#"body.test host://{addr}
body.test resScript://{{body.js}}
``` body.js
if (typeof Buffer.prototype.writeUInt32LE !== 'function') throw new Error('not prepared');
ctx.res.body = Buffer.from('rewritten').toString();
ctx.res.statusCode = 201;
ctx.res.headers['x-helper'] = 'ready';
```
"#
    );
    let proxy = whix::embed::Proxy::builder()
        .port(0)
        .persist_sessions(false)
        .storage_dir(dir.clone())
        .rules(text)
        .start()
        .await
        .unwrap();
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut socket = TcpStream::connect(proxy.addr()).await?;
        socket
            .write_all(
                b"GET http://body.test/ HTTP/1.1\r\nHost: body.test\r\nConnection: close\r\n\r\n",
            )
            .await?;
        let mut answer = String::new();
        socket.read_to_string(&mut answer).await?;
        Ok::<_, std::io::Error>(answer)
    })
    .await;
    proxy.shutdown().await;
    origin.abort();
    let _ = origin.await;
    std::fs::remove_dir_all(dir).unwrap();
    let response = response.expect("request must finish").unwrap();
    assert!(response.starts_with("HTTP/1.1 201"), "{response}");
    assert!(
        response.to_ascii_lowercase().contains("x-helper: ready"),
        "{response}"
    );
    assert!(response.ends_with("rewritten"), "{response}");
}
