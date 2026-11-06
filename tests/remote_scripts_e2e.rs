//! Remote rule/script content must reach the matching execution phase.
mod common;

use common::{Instance, get, request, scratch, start};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

struct Reply {
    status: u16,
    body: Vec<u8>,
    extra: &'static str,
}
impl Reply {
    fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            body: body.into(),
            extra: "",
        }
    }
}

trait Io: Read + Write {}
impl<T: Read + Write> Io for T {}

struct Server {
    address: String,
    scheme: &'static str,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    task: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    fn new(answer: impl Fn(&str, &str) -> Reply + Send + Sync + 'static) -> Self {
        Self::with_tls(None, answer)
    }
    fn with_tls(
        tls: Option<Arc<rustls::ServerConfig>>,
        answer: impl Fn(&str, &str) -> Reply + Send + Sync + 'static,
    ) -> Self {
        let scheme = if tls.is_some() { "https" } else { "http" };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let answer = Arc::new(answer);
        let task = std::thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((socket, _)) => {
                        let seen = seen.clone();
                        let answer = answer.clone();
                        let tls = tls.clone();
                        std::thread::spawn(move || {
                            socket.set_nonblocking(false).unwrap();
                            socket
                                .set_read_timeout(Some(Duration::from_secs(3)))
                                .unwrap();
                            let mut socket: Box<dyn Io> = match tls {
                                Some(config) => Box::new(rustls::StreamOwned::new(
                                    rustls::ServerConnection::new(config).unwrap(),
                                    socket,
                                )),
                                None => Box::new(socket),
                            };
                            let mut head = Vec::new();
                            while !head.ends_with(b"\r\n\r\n") {
                                let mut byte = [0];
                                if socket.read_exact(&mut byte).is_err() {
                                    return;
                                }
                                head.push(byte[0]);
                            }
                            let head = String::from_utf8(head).unwrap();
                            let path = head.split_whitespace().nth(1).unwrap();
                            seen.lock().unwrap().push(head.clone());
                            let reply = answer(path, &head);
                            let _ = write!(
                                socket,
                                "HTTP/1.1 {} Test\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",
                                reply.status,
                                reply.body.len(),
                                reply.extra
                            );
                            let _ = socket.write_all(&reply.body);
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1))
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            }
        });
        Self {
            address,
            scheme,
            requests,
            stop,
            task: Some(task),
        }
    }
    fn url(&self, path: &str) -> String {
        format!("{}://{}{path}", self.scheme, self.address)
    }
    fn paths(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|h| h.split_whitespace().nth(1).unwrap().to_string())
            .collect()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.task.take().unwrap().join().unwrap();
    }
}

fn rules(proxy: &Instance, text: &str) {
    assert_eq!(
        request(&proxy.addr, "POST", "/api/rules", text.as_bytes()).0,
        200
    );
}
fn via(proxy: &Instance, source: &Server, path: &str) -> String {
    let mut client = TcpStream::connect(&proxy.addr).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        client,
        "GET {} HTTP/1.0\r\nHost: {}\r\nCookie: fixture-only\r\nAuthorization: Bearer fixture-only\r\n\r\n",
        source.url(path),
        source.address
    )
    .unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn all_four_remote_families_execute_at_their_own_phase() {
    for (protocol, text, marker, expected_paths) in [
        (
            "reqScript",
            "rules.push('* reqHeaders://x-remote=script');",
            "x-remote: script",
            vec!["/source", "/origin"],
        ),
        (
            "reqRules",
            "* reqHeaders://x-remote=rules",
            "x-remote: rules",
            vec!["/source", "/origin"],
        ),
        (
            "resScript",
            "rules.push('* resHeaders://x-remote=script');",
            "x-remote: script",
            vec!["/origin", "/source"],
        ),
        (
            "resRules",
            "* resHeaders://x-remote=rules",
            "x-remote: rules",
            vec!["/origin", "/source"],
        ),
    ] {
        let source =
            Server::new(move |path, head| Reply::ok(if path == "/source" { text } else { head }));
        let proxy = start(&scratch(&format!("remote-{protocol}")), &["--no-persist"]);
        rules(
            &proxy,
            &format!(
                "{} {protocol}://{}",
                source.url("/origin"),
                source.url("/source")
            ),
        );
        let answer = via(&proxy, &source, "/origin");
        assert!(
            answer.to_ascii_lowercase().contains(marker),
            "{protocol}: {answer}"
        );
        assert_eq!(source.paths(), expected_paths);
        let requests = source.requests.lock().unwrap();
        let fetch = requests
            .iter()
            .find(|h| h.starts_with("GET /source "))
            .unwrap();
        assert!(!fetch.to_ascii_lowercase().contains("authorization:"));
        assert!(
            fetch
                .to_ascii_lowercase()
                .contains(&format!("host: {}", source.address))
        );
        assert!(
            !fetch.to_ascii_lowercase().contains("cookie:"),
            "source fetch is not the intercepted request"
        );
    }
}

#[test]
fn remote_response_hook_runs_after_loading_its_source() {
    let source = Server::new(|path, _| {
        Reply::ok(if path == "/source" {
            "ctx.res.statusCode=201; ctx.res.body='remote hook'; ctx.res.headers['x-hook']='yes';"
        } else {
            "origin"
        })
    });
    let proxy = start(&scratch("remote-response-hook"), &["--no-persist"]);
    rules(
        &proxy,
        &format!(
            "{} resScript://{}",
            source.url("/origin"),
            source.url("/source")
        ),
    );
    let answer = via(&proxy, &source, "/origin");
    assert!(
        answer.starts_with("HTTP/1.0 201") || answer.starts_with("HTTP/1.1 201"),
        "{answer}"
    );
    assert!(answer.contains("remote hook"));
    assert!(answer.to_ascii_lowercase().contains("x-hook: yes"));
    assert_eq!(source.paths(), ["/origin", "/source"]);
}

fn details(proxy: &Instance) -> Value {
    let sessions: Value = serde_json::from_slice(&get(&proxy.addr, "/sessions.json").1).unwrap();
    let id = sessions[0]["id"].as_u64().unwrap();
    serde_json::from_slice(&get(&proxy.addr, &format!("/session.json?id={id}")).1).unwrap()
}

#[test]
fn query_only_source_urls_are_valid_and_do_not_leak_into_host() {
    for (protocol, text) in [
        ("reqScript", "rules.push('* reqHeaders://x-query=yes');"),
        ("reqRules", "* reqHeaders://x-query=yes"),
        ("resScript", "rules.push('* resHeaders://x-query=yes');"),
        ("resRules", "* resHeaders://x-query=yes"),
    ] {
        let source =
            Server::new(move |path, head| Reply::ok(if path == "/?v=1" { text } else { head }));
        let proxy = start(
            &scratch(&format!("remote-query-{protocol}")),
            &["--no-persist"],
        );
        rules(
            &proxy,
            &format!(
                "{} {protocol}://{}?v=1",
                source.url("/origin"),
                source.url("")
            ),
        );
        assert!(
            via(&proxy, &source, "/origin")
                .to_ascii_lowercase()
                .contains("x-query: yes")
        );
        assert!(source.paths().contains(&"/?v=1".to_string()));
    }
}

#[test]
fn response_scripts_keep_request_phase_selection_but_use_response_context() {
    let source = Server::new(|path, _| match path {
        "/condition" => Reply::ok("rules.push('* resHeaders://x-condition='+statusCode);"),
        "/status-.rules" => Reply::ok("* resHeaders://x-template=request-phase"),
        "/created" => Reply {
            status: 201,
            ..Reply::ok("origin")
        },
        _ => Reply::ok("origin"),
    });
    let proxy = start(&scratch("remote-response-conditions"), &["--no-persist"]);
    rules(
        &proxy,
        &format!(
            "{} resScript://{} includeFilter://s:201\n{} resRules://`{}/status-${{statusCode}}.rules` includeFilter://s:201",
            source.address,
            source.url("/condition"),
            source.address,
            source.url("")
        ),
    );
    assert!(!via(&proxy, &source, "/ok").contains("x-condition"));
    assert_eq!(source.paths(), ["/ok"]);
    assert!(!via(&proxy, &source, "/created").contains("x-condition"));
    assert_eq!(source.paths(), ["/ok", "/created"]);
    source.requests.lock().unwrap().clear();
    rules(
        &proxy,
        &format!(
            "{} resScript://{}\n{} resRules://`{}/status-${{statusCode}}.rules`",
            source.address,
            source.url("/condition"),
            source.address,
            source.url("")
        ),
    );
    let created = via(&proxy, &source, "/created").to_ascii_lowercase();
    assert!(created.contains("x-condition: 201"), "{created}");
    assert!(created.contains("x-template: request-phase"), "{created}");
    assert_eq!(source.paths(), ["/created", "/condition", "/status-.rules"]);
}

#[test]
fn selected_entries_are_fetched_once_and_the_shadowed_script_is_not_requested() {
    let source = Server::new(|path, head| match path {
        "/shared" => Reply::ok("* resHeaders://x-shared=loaded"),
        "/first" => Reply::ok("rules.push('* reqHeaders://x-first=yes');"),
        "/never" => panic!("a shadowed script must not be fetched"),
        _ => Reply::ok(head),
    });
    let proxy = start(&scratch("remote-source-selection"), &["--no-persist"]);
    let pattern = source.url("/origin");
    rules(
        &proxy,
        &format!(
            "{pattern} reqScript://{}\n{pattern} reqScript://{}\n{pattern} resScript://{}\n{pattern} resRules://{}\n{pattern} resScript://{}",
            source.url("/first"),
            source.url("/never"),
            source.url("/shared"),
            source.url("/shared"),
            source.url("/never")
        ),
    );
    for _ in 0..2 {
        let answer = via(&proxy, &source, "/origin").to_ascii_lowercase();
        assert!(answer.contains("x-first: yes"));
        assert!(answer.contains("x-shared: loaded"));
    }
    assert_eq!(
        source.paths(),
        [
            "/first", "/origin", "/shared", "/first", "/origin", "/shared"
        ]
    );
}

#[test]
fn a_request_source_can_produce_a_remote_response_rule_without_loading_it_early() {
    let source = Server::new(|path, _| match path {
        "/request" => {
            Reply::ok("rules.push(url+' resRules://'+url.replace('/origin','/response'));")
        }
        "/response" => Reply::ok("* resHeaders://x-generated=yes"),
        _ => Reply::ok("origin"),
    });
    let proxy = start(&scratch("remote-generated-response"), &["--no-persist"]);
    rules(
        &proxy,
        &format!(
            "{} reqScript://{}",
            source.url("/origin"),
            source.url("/request")
        ),
    );
    assert!(
        via(&proxy, &source, "/origin")
            .to_ascii_lowercase()
            .contains("x-generated: yes")
    );
    assert_eq!(source.paths(), ["/request", "/origin", "/response"]);
}

#[test]
fn failed_sources_are_visible_never_execute_their_body_and_retry_only_on_a_new_request() {
    for protocol in ["reqScript", "reqRules", "resScript", "resRules"] {
        let response_phase = protocol.starts_with("res");
        let operator = if response_phase {
            "resHeaders"
        } else {
            "reqHeaders"
        };
        let content = |marker| {
            let rule = format!("* {operator}://{marker}=yes");
            if protocol.ends_with("Script") {
                format!("rules.push('{rule}');")
            } else {
                rule
            }
        };
        for (name, code) in [("missing", 404), ("redirect", 302), ("unavailable", 503)] {
            let recovering = Arc::new(AtomicBool::new(false));
            let recovered = recovering.clone();
            let success = content("x-recovered");
            let failure = content("x-should-not-run");
            let source = Server::new(move |path, head| {
                if path.starts_with("/source") {
                    if recovered.load(Ordering::Acquire) {
                        Reply::ok(success.as_str())
                    } else {
                        Reply {
                            status: code,
                            body: failure.as_bytes().to_vec(),
                            extra: "Location: /never\r\n",
                        }
                    }
                } else {
                    Reply::ok(head)
                }
            });
            let proxy = start(
                &scratch(&format!("remote-failure-{protocol}-{name}")),
                &["--no-persist"],
            );
            rules(
                &proxy,
                &format!(
                    "{} {protocol}://{}?token=SOURCE_QUERY_SECRET",
                    source.url("/origin"),
                    source.url("/source")
                ),
            );
            let answer = via(&proxy, &source, "/origin");
            assert!(answer.starts_with("HTTP/1.0 200") || answer.starts_with("HTTP/1.1 200"));
            assert!(!answer.contains("x-should-not-run"));
            let session = details(&proxy);
            let reasons = session["unapplied"].as_array().unwrap();
            assert_eq!(reasons.len(), 1, "{session}");
            assert_eq!(reasons[0]["kind"], "rule-source-failed");
            let reason = reasons[0]["reason"].as_str().unwrap();
            assert!(reason.contains(&format!("HTTP {code}")));
            assert!(!reason.contains("SOURCE_QUERY_SECRET"));
            assert!(
                !proxy
                    .log
                    .lock()
                    .unwrap()
                    .join("\n")
                    .contains("SOURCE_QUERY_SECRET")
            );
            recovering.store(true, Ordering::Release);
            assert!(
                via(&proxy, &source, "/origin")
                    .to_ascii_lowercase()
                    .contains("x-recovered: yes")
            );
            let pair = if response_phase {
                ["/origin", "/source?token=SOURCE_QUERY_SECRET"]
            } else {
                ["/source?token=SOURCE_QUERY_SECRET", "/origin"]
            };
            assert_eq!(source.paths(), pair.repeat(2));
        }
    }
}

#[test]
fn inline_and_file_hooks_keep_working_and_loaded_content_is_never_reopened_as_a_file() {
    let dir = scratch("remote-content-not-path");
    let source = Server::new(|path, _| {
        Reply::ok(if path == "/hook" {
            "ctx.res.body='from-source';"
        } else {
            "origin"
        })
    });
    std::fs::write(
        dir.join("ctx.res.body='from-source';"),
        "ctx.res.body='WRONG FILE';",
    )
    .unwrap();
    std::fs::write(dir.join("local.js"), "ctx.res.body='local file';").unwrap();
    let mut command = common::command(&dir, &["--no-persist"]);
    command.current_dir(&dir);
    let proxy = common::try_start(command, &dir).unwrap();
    for (operator, expected) in [
        (
            format!("resScript://{}", source.url("/hook")),
            "from-source",
        ),
        (
            format!("resScript://{}", dir.join("local.js").display()),
            "local file",
        ),
        ("resScript://{inline}".into(), "inline value"),
    ] {
        rules(
            &proxy,
            &format!(
                "``` inline\nctx.res.body='inline value';\n```\n{} {operator}",
                source.url("/origin")
            ),
        );
        let answer = via(&proxy, &source, "/origin");
        assert_eq!(answer.split_once("\r\n\r\n").unwrap().1, expected);
    }
    assert_eq!(source.paths().iter().filter(|p| *p == "/hook").count(), 1);
}

#[test]
fn downloaded_response_rules_can_filter_on_the_response_status() {
    for (protocol, content) in [
        (
            "resRules",
            "* resHeaders://x-filtered=yes includeFilter://s:201",
        ),
        (
            "resScript",
            "rules.push('* resHeaders://x-filtered=yes includeFilter://s:201');",
        ),
    ] {
        let source = Server::new(move |path, _| match path {
            "/source" => Reply::ok(content),
            "/created" => Reply {
                status: 201,
                ..Reply::ok("origin")
            },
            _ => Reply::ok("origin"),
        });
        let proxy = start(
            &scratch(&format!("remote-inner-filter-{protocol}")),
            &["--no-persist"],
        );
        rules(
            &proxy,
            &format!("{} {protocol}://{}", source.address, source.url("/source")),
        );
        assert!(!via(&proxy, &source, "/ok").contains("x-filtered"));
        assert!(
            via(&proxy, &source, "/created")
                .to_ascii_lowercase()
                .contains("x-filtered: yes")
        );
        assert_eq!(source.paths(), ["/ok", "/source", "/created", "/source"]);
    }
}

#[test]
fn https_sources_use_the_existing_tls_verification_policy() {
    let rcgen::CertifiedKey { cert, key_pair } =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(key_pair.serialize_der()).into(),
        )
        .unwrap();
    let source = Server::with_tls(Some(Arc::new(config)), |_, _| {
        Reply::ok("rules.push('* reqHeaders://x-tls-source=yes');")
    });
    let origin = Server::new(|_, head| Reply::ok(head));
    for (name, extra, allowed) in [
        ("remote-https-verified", vec!["--no-persist"], false),
        (
            "remote-https-fixture",
            vec!["--no-persist", "--insecure-upstream"],
            true,
        ),
    ] {
        let proxy = start(&scratch(name), &extra);
        rules(
            &proxy,
            &format!(
                "{} reqScript://{}",
                origin.url("/origin"),
                source.url("/source")
            ),
        );
        let response = via(&proxy, &origin, "/origin").to_ascii_lowercase();
        assert_eq!(response.contains("x-tls-source: yes"), allowed);
        let detail = details(&proxy);
        let failed = detail["unapplied"]
            .as_array()
            .is_some_and(|entries| entries.iter().any(|e| e["kind"] == "rule-source-failed"));
        assert_eq!(failed, !allowed);
    }
    assert_eq!(source.paths(), ["/source"]);
}

#[test]
fn concurrent_requests_do_not_share_loaded_sources_or_hold_store_locks() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let waiting = gate.clone();
    let (reached, arrivals) = std::sync::mpsc::channel();
    let source = Server::new(move |path, head| {
        if path == "/source" {
            reached.send(()).unwrap();
            // Only API completion releases the sources; a timeout must fail
            // the clients, not silently turn a held store lock into a pass.
            let (released, _) = waiting
                .1
                .wait_timeout_while(waiting.0.lock().unwrap(), Duration::from_secs(3), |open| {
                    !*open
                })
                .unwrap();
            if !*released {
                return Reply {
                    status: 503,
                    ..Reply::ok("gate timed out")
                };
            }
            Reply::ok("rules.push(url+' reqHeaders://x-request='+url.split('/').pop());")
        } else {
            Reply::ok(head)
        }
    });
    let proxy = start(&scratch("remote-concurrent"), &["--no-persist"]);
    let rule = format!("{} reqScript://{}", source.address, source.url("/source"));
    rules(&proxy, &rule);
    std::thread::scope(|scope| {
        let clients: Vec<_> = (0..4)
            .map(|n| {
                scope.spawn({
                    let source = &source;
                    let proxy = &proxy;
                    move || via(proxy, source, &format!("/origin-{n}"))
                })
            })
            .collect();
        for _ in 0..4 {
            arrivals.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        assert_eq!(get(&proxy.addr, "/api/status").0, 200);
        assert_eq!(request(&proxy.addr, "POST", "/api/values", b"{}").0, 200);
        rules(&proxy, &rule);
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        for (n, client) in clients.into_iter().enumerate() {
            assert!(
                client
                    .join()
                    .unwrap()
                    .to_ascii_lowercase()
                    .contains(&format!("x-request: origin-{n}"))
            );
        }
    });
    assert_eq!(source.paths().iter().filter(|p| *p == "/source").count(), 4);
}

#[test]
fn source_failure_diagnostics_survive_persistence_and_restart_without_refetching() {
    let dir = scratch("remote-failure-history");
    let source = Server::new(|path, _| {
        if path == "/source" {
            Reply {
                status: 404,
                ..Reply::ok("* resHeaders://x-not-run=yes")
            }
        } else {
            Reply::ok("origin")
        }
    });
    let proxy = start(&dir, &[]);
    rules(
        &proxy,
        &format!(
            "{} resRules://{}",
            source.url("/origin"),
            source.url("/source")
        ),
    );
    assert!(via(&proxy, &source, "/origin").ends_with("origin"));
    let expected = details(&proxy)["unapplied"].clone();
    assert_eq!(expected[0]["kind"], "rule-source-failed");
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let stored = whix::proxy::persist::SessionStore::load(&dir.join("sessions"), 1, 7);
        if let Some(session) = stored.first() {
            assert_eq!(serde_json::to_value(&session.unapplied).unwrap(), expected);
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "failure was not persisted"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(proxy);
    let proxy = common::restart(&dir);
    assert_eq!(details(&proxy)["unapplied"], expected);
    assert_eq!(source.paths(), ["/origin", "/source"]);
}
