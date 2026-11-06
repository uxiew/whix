//! Remote frame scripts are loaded once during connection setup, not per frame.
mod common;

use common::frame_peer::*;
use common::http_source::{Reply, Server};
use common::{Instance, get, request, scratch, start};
use serde_json::Value;
use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const SCRIPT: &str = "var n=0;ctx.handleSendToServerFrame=function(buf){return 'S'+(++n)+':'+buf;};ctx.handleSendToClientFrame=function(buf){return 'C:'+buf;};";

// No TCP message boundaries are assumed by these transforms.
const TCP_SCRIPT: &str = "var handle=function(buf){return buf.toString().toUpperCase();};ctx.handleSendToServerFrame=handle;ctx.handleSendToClientFrame=handle;";

fn rules(proxy: &Instance, text: &str) {
    assert_eq!(
        request(&proxy.addr, "POST", "/api/rules", text.as_bytes()).0,
        200
    );
}
fn json(proxy: &Instance, path: &str) -> Value {
    serde_json::from_slice(&get(&proxy.addr, path).1).unwrap()
}

fn until(mut check: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(3);
    while !check() {
        assert!(Instant::now() < end, "condition did not become true");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn remote_frame_script_runs_once_per_websocket_connection_with_fresh_state() {
    let source = Server::new(|_, _| Reply::ok(SCRIPT));
    let proxy = start(&scratch("remote-frame-ws"), &["--no-persist"]);
    for _ in 0..2 {
        let origin = listener();
        let port = origin.local_addr().unwrap().port();
        rules(
            &proxy,
            &format!(
                "localhost:{port} host://127.0.0.1:{port} frameScript://{}",
                source.url("/frame.js")
            ),
        );
        let (mut client, mut peer) = websocket(&proxy, origin, false, "websocket");
        for (number, data) in [(1, "a"), (2, "b")] {
            send(&mut *client, true, 1, data.as_bytes(), true);
            expect(&mut *peer, 1, format!("S{number}:{data}").as_bytes(), true);
            send(&mut *peer, true, 1, b"reply", false);
            expect(&mut *client, 1, b"C:reply", false);
        }
        close(&mut *client, &mut *peer);
    }
    assert_eq!(source.paths(), ["/frame.js", "/frame.js"]);
    for head in source.requests.lock().unwrap().iter() {
        assert!(!head.to_ascii_lowercase().contains("cookie:"));
        assert!(!head.to_ascii_lowercase().contains("authorization:"));
    }
}

#[test]
fn remote_frame_script_runs_on_inspected_connect() {
    let source = Server::new(|_, _| Reply::ok(SCRIPT));
    let proxy = start(&scratch("remote-frame-tcp"), &["--no-persist"]);
    for _ in 0..2 {
        let origin = listener();
        let address = origin.local_addr().unwrap().to_string();
        rules(
            &proxy,
            &format!(
                "{address} enable://inspect frameScript://{}",
                source.url("/frame.js")
            ),
        );
        let mut client = connect(&proxy, &address, true);
        let mut peer = accept(&origin);
        // One byte cannot split into multiple handler calls. Output reads may
        // split freely; read_exact checks bytes rather than TCP chunk counts.
        for (index, &byte) in b"ab".iter().enumerate() {
            client.write_all(&[byte]).unwrap();
            let mut data = [0; 4];
            peer.read_exact(&mut data).unwrap();
            assert_eq!(data, [b'S', b'1' + index as u8, b':', byte]);
            peer.write_all(b"r").unwrap();
            let mut reply = [0; 3];
            client.read_exact(&mut reply).unwrap();
            assert_eq!(&reply, b"C:r");
        }
        finish_tcp(&mut client, &mut peer);
    }
    assert_eq!(source.paths(), ["/frame.js", "/frame.js"]);
}

#[test]
fn wss_keeps_fragment_binary_and_control_frame_semantics() {
    let source = Server::new(|_, _| {
        Reply::ok(
            "ctx.sendToClient('ready');ctx.handleSendToServerFrame=function(buf,opts){return opts.opcode===2?buf:'S:'+buf;};ctx.handleSendToClientFrame=function(buf,opts){return opts.opcode===2?buf:'C:'+buf;};",
        )
    });
    let proxy = start(
        &scratch("remote-frame-wss"),
        &["--no-persist", "--insecure-upstream"],
    );
    let origin = listener();
    let port = origin.local_addr().unwrap().port();
    rules(
        &proxy,
        &format!(
            "localhost:{port} host://127.0.0.1:{port} frameScript://{}",
            source.url("/frame.js")
        ),
    );
    let (mut client, mut peer) = websocket(&proxy, origin, true, "websocket");
    expect(&mut *client, 1, b"C:ready", false);
    send(&mut *client, false, 1, b"one", true);
    send(&mut *client, true, 9, b"ping", true);
    expect(&mut *peer, 9, b"ping", true);
    send(&mut *client, true, 0, b"two", true);
    expect(&mut *peer, 1, b"S:onetwo", true);
    send(&mut *client, true, 2, &[0, 255, 128], true);
    expect(&mut *peer, 2, &[0, 255, 128], true);
    send(&mut *peer, true, 2, &[128, 0, 255], false);
    expect(&mut *client, 2, &[128, 0, 255], false);
    close(&mut *client, &mut *peer);
    assert_eq!(source.paths(), ["/frame.js"]);
}

#[test]
fn sniffed_connect_and_inspected_raw_upgrade_load_before_forwarding_the_first_bytes() {
    let source = Server::new(|_, _| Reply::ok(TCP_SCRIPT));
    let proxy = start(&scratch("remote-frame-sniff"), &["--no-persist"]);
    let origin = listener();
    let address = origin.local_addr().unwrap().to_string();
    rules(
        &proxy,
        &format!(
            "{address} enable://inspect frameScript://{}",
            source.url("/frame.js")
        ),
    );
    let mut client = connect(&proxy, &address, false);
    // These non-TLS, non-HTTP bytes make the SNI path choose a raw relay.
    client.write_all(b"first\n").unwrap();
    let mut peer = accept(&origin);
    assert_eq!(line(&mut peer), "FIRST\n");
    peer.write_all(b"reply\n").unwrap();
    assert_eq!(line(&mut client), "REPLY\n");
    finish_tcp(&mut client, &mut peer);

    let origin = listener();
    let port = origin.local_addr().unwrap().port();
    rules(
        &proxy,
        &format!(
            "localhost:{port} host://127.0.0.1:{port} enable://inspect frameScript://{}",
            source.url("/frame.js")
        ),
    );
    let (mut client, mut peer) = websocket(&proxy, origin, false, "fixture");
    client.write_all(b"raw\n").unwrap();
    assert_eq!(line(&mut *peer), "RAW\n");
    peer.write_all(b"reply\n").unwrap();
    assert_eq!(line(&mut *client), "REPLY\n");
    assert_eq!(source.paths(), ["/frame.js", "/frame.js"]);
}

#[test]
fn ordinary_http_opaque_tunnels_and_rejected_upgrades_do_not_fetch_a_frame_source() {
    let source = Server::new(|_, _| Reply::ok(SCRIPT));
    let proxy = start(&scratch("remote-frame-unused"), &["--no-persist"]);
    rules(&proxy, &format!("* frameScript://{}", source.url("/never")));
    let http = Server::new(|path, _| {
        if path == "/rejected" {
            Reply {
                status: 403,
                ..Reply::ok("rejected")
            }
        } else {
            Reply::ok("ordinary")
        }
    });
    assert_eq!(
        get(&proxy.addr, &http.url("/ordinary")),
        (200, b"ordinary".to_vec())
    );
    let rejected = common::request_with(
        &proxy.addr,
        "GET",
        &http.url("/rejected"),
        &[
            ("Connection", "Upgrade"),
            ("Upgrade", "websocket"),
            ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ],
        b"",
    );
    assert_eq!(rejected, (403, b"rejected".to_vec()));
    let origin = listener();
    let address = origin.local_addr().unwrap().to_string();
    let mut client = connect(&proxy, &address, true);
    let mut peer = accept(&origin);
    client.write_all(b"opaque\n").unwrap();
    assert_eq!(line(&mut peer), "opaque\n");
    finish_tcp(&mut client, &mut peer);
    let origin = listener();
    let port = origin.local_addr().unwrap().port();
    rules(
        &proxy,
        &format!(
            "localhost:{port} host://127.0.0.1:{port} frameScript://{}",
            source.url("/never")
        ),
    );
    let (mut client, mut peer) = websocket(&proxy, origin, false, "fixture");
    client.write_all(b"raw\n").unwrap();
    assert_eq!(line(&mut *peer), "raw\n");
    assert!(source.paths().is_empty());
}

#[test]
fn handshake_precedes_source_loading_and_configuration_changes_do_not_change_its_snapshot() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let waiting = gate.clone();
    let (reached, arrived) = std::sync::mpsc::channel();
    let source = Server::new(move |_, _| {
        reached.send(()).unwrap();
        let (released, _) = waiting
            .1
            .wait_timeout_while(waiting.0.lock().unwrap(), Duration::from_secs(3), |open| {
                !*open
            })
            .unwrap();
        if !*released {
            return Reply {
                status: 503,
                ..Reply::ok("source gate timed out")
            };
        }
        Reply::ok(
            "var mark=getValue('mark');ctx.handleSendToServerFrame=function(buf){return mark+':'+buf;};",
        )
    });
    let proxy = start(&scratch("remote-frame-snapshot"), &["--no-persist"]);
    assert_eq!(
        request(&proxy.addr, "POST", "/api/values", br#"{"mark":"before"}"#).0,
        200
    );
    let origin = listener();
    let port = origin.local_addr().unwrap().port();
    rules(
        &proxy,
        &format!(
            "localhost:{port} host://127.0.0.1:{port} frameScript://{}",
            source.url("/before")
        ),
    );
    let (mut client, mut peer) = websocket(&proxy, origin, false, "websocket");
    // The fixture cannot answer the source until the client observed 101.
    // Also wait for the GET without sending a frame: initialization is eager.
    arrived.recv_timeout(Duration::from_secs(3)).unwrap();
    send(&mut *client, true, 1, b"queued", true);
    assert_eq!(json(&proxy, "/sessions.json")[0]["open"], true);
    assert_eq!(get(&proxy.addr, "/api/status").0, 200);
    assert_eq!(
        request(&proxy.addr, "POST", "/api/values", br#"{"mark":"after"}"#).0,
        200
    );
    rules(&proxy, "* frameScript://http://127.0.0.1:1/never");
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    expect(&mut *peer, 1, b"before:queued", true);
    send(&mut *client, true, 1, b"again", true);
    expect(&mut *peer, 1, b"before:again", true);
    assert_ne!(json(&proxy, "/sessions.json")[0]["open"], true);
    close(&mut *client, &mut *peer);
    assert_eq!(source.paths(), ["/before"]);
}

#[test]
fn slow_sources_initialize_concurrent_tcp_connections_without_holding_store_locks() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let waiting = gate.clone();
    let (reached, arrivals) = std::sync::mpsc::channel();
    let source = Server::new(move |_, _| {
        reached.send(()).unwrap();
        let (released, _) = waiting
            .1
            .wait_timeout_while(waiting.0.lock().unwrap(), Duration::from_secs(3), |open| {
                !*open
            })
            .unwrap();
        if !*released {
            return Reply {
                status: 503,
                ..Reply::ok("source gate timed out")
            };
        }
        Reply::ok(TCP_SCRIPT)
    });
    let proxy = start(&scratch("remote-frame-concurrent"), &["--no-persist"]);
    let origin = listener();
    let address = origin.local_addr().unwrap().to_string();
    let rule = format!(
        "{address} enable://inspect frameScript://{}",
        source.url("/shared")
    );
    rules(&proxy, &rule);
    std::thread::scope(|scope| {
        let clients: Vec<_> = (0..4)
            .map(|n| {
                let proxy = &proxy;
                let address = &address;
                scope.spawn(move || {
                    let mut client = connect(proxy, address, true);
                    client
                        .write_all(format!("client-{n}\n").as_bytes())
                        .unwrap();
                    assert_eq!(line(&mut client), format!("REPLY:CLIENT-{n}\n"));
                })
            })
            .collect();
        let peers: Vec<_> = (0..4)
            .map(|_| {
                let mut peer = accept(&origin);
                scope.spawn(move || {
                    let received = line(&mut peer);
                    assert!(received.starts_with("CLIENT-"));
                    peer.write_all(format!("reply:{received}").as_bytes())
                        .unwrap();
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
        for client in clients {
            client.join().unwrap();
        }
        for peer in peers {
            peer.join().unwrap();
        }
    });
    assert_eq!(source.paths(), ["/shared", "/shared", "/shared", "/shared"]);
}

#[test]
fn failed_sources_are_visible_persisted_once_and_survive_restart_for_ws_and_tcp() {
    for tcp in [false, true] {
        let dir = scratch(&format!("remote-frame-history-{tcp}"));
        let source = Server::new(|_, _| Reply {
            status: 404,
            ..Reply::ok(SCRIPT)
        });
        let proxy = start(&dir, &[]);
        let origin = listener();
        let address = origin.local_addr().unwrap().to_string();
        let port = origin.local_addr().unwrap().port();
        if tcp {
            rules(
                &proxy,
                &format!(
                    "{address} enable://inspect frameScript://{}",
                    source.url("/missing")
                ),
            );
            let mut client = connect(&proxy, &address, true);
            let mut peer = accept(&origin);
            client.write_all(b"unchanged\n").unwrap();
            assert_eq!(line(&mut peer), "unchanged\n");
            finish_tcp(&mut client, &mut peer);
        } else {
            rules(
                &proxy,
                &format!(
                    "localhost:{port} host://127.0.0.1:{port} frameScript://{}",
                    source.url("/missing")
                ),
            );
            let (mut client, mut peer) = websocket(&proxy, origin, false, "websocket");
            send(&mut *client, true, 1, b"unchanged", true);
            expect(&mut *peer, 1, b"unchanged", true);
            close(&mut *client, &mut *peer);
        }
        until(|| {
            json(&proxy, "/session.json?id=1")["unapplied"][0]["kind"] == "rule-source-failed"
        });
        let expected = json(&proxy, "/session.json?id=1")["unapplied"].clone();
        assert_eq!(expected.as_array().unwrap().len(), 1);
        assert!(expected[0]["reason"].as_str().unwrap().contains("HTTP 404"));
        until(|| {
            !whix::proxy::persist::SessionStore::load(&dir.join("sessions"), 10, 7).is_empty()
        });
        drop(proxy);
        let stored = whix::proxy::persist::SessionStore::load(&dir.join("sessions"), 10, 7);
        assert_eq!(
            stored.len(),
            1,
            "initialization must not append a second history row"
        );
        let proxy = common::restart(&dir);
        assert_eq!(json(&proxy, "/session.json?id=1")["unapplied"], expected);
        assert_eq!(source.paths(), ["/missing"]);
    }
}

#[test]
fn a_late_failure_does_not_resurrect_a_cleared_row_or_annotate_another_request() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let waiting = gate.clone();
    let (reached, arrived) = std::sync::mpsc::channel();
    let source = Server::new(move |_, _| {
        reached.send(false).unwrap();
        let (released, _) = waiting
            .1
            .wait_timeout_while(waiting.0.lock().unwrap(), Duration::from_secs(3), |open| {
                !*open
            })
            .unwrap();
        reached.send(*released).unwrap();
        Reply {
            status: 503,
            ..Reply::ok(SCRIPT)
        }
    });
    let proxy = start(&scratch("remote-frame-cleared"), &["--no-persist"]);
    let origin = listener();
    let port = origin.local_addr().unwrap().port();
    rules(
        &proxy,
        &format!(
            "localhost:{port} host://127.0.0.1:{port} frameScript://{}",
            source.url("/late")
        ),
    );
    let (mut client, mut peer) = websocket(&proxy, origin, false, "websocket");
    assert!(!arrived.recv_timeout(Duration::from_secs(3)).unwrap());
    assert_eq!(
        request(&proxy.addr, "POST", "/api/sessions/clear", b"{}").0,
        200
    );
    let other = Server::new(|_, _| Reply::ok("other"));
    assert_eq!(get(&proxy.addr, &other.url("/other")).0, 200);
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    assert!(
        arrived.recv_timeout(Duration::from_secs(3)).unwrap(),
        "source gate timed out before release"
    );
    send(&mut *client, true, 1, b"unchanged", true);
    expect(&mut *peer, 1, b"unchanged", true);
    let rows = json(&proxy, "/sessions.json");
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["id"], 2);
    assert!(rows[0]["unapplied"].is_null());
    close(&mut *client, &mut *peer);
}

#[test]
fn hidden_connections_load_the_source_but_publish_neither_frames_nor_failures() {
    for tcp in [false, true] {
        for status in [200, 404] {
            let source = Server::new(move |_, _| Reply {
                status,
                ..Reply::ok(if tcp { TCP_SCRIPT } else { SCRIPT })
            });
            let proxy = start(
                &scratch(&format!("remote-frame-hidden-{tcp}-{status}")),
                &["--no-persist"],
            );
            let origin = listener();
            let address = origin.local_addr().unwrap().to_string();
            let port = origin.local_addr().unwrap().port();
            if tcp {
                rules(
                    &proxy,
                    &format!(
                        "{address} enable://inspect|hide frameScript://{}",
                        source.url("/source")
                    ),
                );
                let mut client = connect(&proxy, &address, true);
                let mut peer = accept(&origin);
                client.write_all(b"a\n").unwrap();
                assert_eq!(line(&mut peer), if status == 200 { "A\n" } else { "a\n" });
                finish_tcp(&mut client, &mut peer);
            } else {
                rules(
                    &proxy,
                    &format!(
                        "localhost:{port} host://127.0.0.1:{port} enable://hide frameScript://{}",
                        source.url("/source")
                    ),
                );
                let (mut client, mut peer) = websocket(&proxy, origin, false, "websocket");
                send(&mut *client, true, 1, b"a", true);
                expect(
                    &mut *peer,
                    1,
                    if status == 200 { b"S1:a" } else { b"a" },
                    true,
                );
                close(&mut *client, &mut *peer);
            }
            assert_eq!(json(&proxy, "/sessions.json"), serde_json::json!([]));
            assert_eq!(json(&proxy, "/frames.json"), serde_json::json!([]));
            assert_eq!(json(&proxy, "/api/ws/status?id=1")["live"], false);
            assert_eq!(source.paths(), ["/source"]);
        }
    }
}

#[test]
fn file_inline_and_values_still_work_and_loaded_content_is_not_reopened_as_a_filename() {
    let dir = scratch("remote-frame-content-not-path");
    let code = "ctx.handleSendToServerFrame=function(buf){return'content-'+buf;};";
    std::fs::write(
        dir.join(code),
        "ctx.handleSendToServerFrame=function(){return'WRONG';};",
    )
    .unwrap();
    std::fs::write(
        dir.join("local.js"),
        "ctx.handleSendToServerFrame=function(buf){return'file:'+buf;};",
    )
    .unwrap();
    let source = Server::new(move |_, _| Reply::ok(code));
    let mut command = common::command(&dir, &["--no-persist"]);
    command.current_dir(&dir);
    let proxy = common::try_start(command, &dir).unwrap();
    for (value, expected) in [
        (source.url("/script"), "content-a"),
        (format!("({code})"), "content-a"),
        ("{code}".into(), "content-a"),
        (dir.join("local.js").display().to_string(), "file:a"),
        (
            "ctx.handleSendToServerFrame=function(buf){return'bare:'+buf;};".into(),
            "bare:a",
        ),
    ] {
        let origin = listener();
        let port = origin.local_addr().unwrap().port();
        rules(
            &proxy,
            &format!(
                "``` code\n{code}\n```\nlocalhost:{port} host://127.0.0.1:{port} frameScript://{value}"
            ),
        );
        let (mut client, mut peer) = websocket(&proxy, origin, false, "websocket");
        send(&mut *client, true, 1, b"a", true);
        expect(&mut *peer, 1, expected.as_bytes(), true);
        close(&mut *client, &mut *peer);
    }
    assert_eq!(source.paths(), ["/script"]);
}

#[test]
fn https_frame_sources_follow_the_existing_verification_policy() {
    let rcgen::CertifiedKey { cert, key_pair } =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(key_pair.serialize_der()).into(),
        )
        .unwrap();
    let source = Server::with_tls(Some(Arc::new(config)), |_, _| Reply::ok(SCRIPT));
    for insecure in [false, true] {
        let options = if insecure {
            vec!["--no-persist", "--insecure-upstream"]
        } else {
            vec!["--no-persist"]
        };
        let proxy = start(
            &scratch(&format!("remote-frame-https-{insecure}")),
            &options,
        );
        let origin = listener();
        let port = origin.local_addr().unwrap().port();
        rules(
            &proxy,
            &format!(
                "localhost:{port} host://127.0.0.1:{port} frameScript://{}",
                source.url("/tls.js")
            ),
        );
        let (mut client, mut peer) = websocket(&proxy, origin, false, "websocket");
        send(&mut *client, true, 1, b"a", true);
        expect(&mut *peer, 1, if insecure { b"S1:a" } else { b"a" }, true);
        assert_eq!(
            json(&proxy, "/session.json?id=1")["unapplied"][0]["kind"] == "rule-source-failed",
            !insecure
        );
        close(&mut *client, &mut *peer);
    }
    assert_eq!(source.paths(), ["/tls.js"]);
}

#[test]
fn redirect_and_oversized_frame_sources_are_not_executed() {
    for oversized in [false, true] {
        let source = Server::new(move |_, _| {
            let mut body = SCRIPT.as_bytes().to_vec();
            if oversized {
                body.resize(256 * 1024 + 1, b' ');
            }
            Reply {
                status: if oversized { 200 } else { 302 },
                body,
                extra: "Location: /never\r\n",
            }
        });
        let proxy = start(
            &scratch(&format!("remote-frame-bound-{oversized}")),
            &["--no-persist"],
        );
        let origin = listener();
        let port = origin.local_addr().unwrap().port();
        rules(
            &proxy,
            &format!(
                "localhost:{port} host://127.0.0.1:{port} frameScript://{}",
                source.url("/source")
            ),
        );
        let (mut client, mut peer) = websocket(&proxy, origin, false, "websocket");
        send(&mut *client, true, 1, b"unchanged", true);
        expect(&mut *peer, 1, b"unchanged", true);
        let notes = json(&proxy, "/session.json?id=1")["unapplied"].clone();
        assert_eq!(notes.as_array().unwrap().len(), 1);
        assert_eq!(notes[0]["kind"], "rule-source-failed");
        assert!(notes[0]["reason"].as_str().unwrap().contains(if oversized {
            "262144"
        } else {
            "HTTP 302"
        }));
        close(&mut *client, &mut *peer);
        assert_eq!(source.paths(), ["/source"]);
    }
}

#[test]
fn a_close_queued_during_source_loading_keeps_the_closing_handshake_and_cleans_controls() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let waiting = gate.clone();
    let (reached, arrived) = std::sync::mpsc::channel();
    let source = Server::new(move |_, _| {
        reached.send(false).unwrap();
        let (released, _) = waiting
            .1
            .wait_timeout_while(waiting.0.lock().unwrap(), Duration::from_secs(3), |open| {
                !*open
            })
            .unwrap();
        reached.send(*released).unwrap();
        if !*released {
            return Reply {
                status: 503,
                ..Reply::ok("source gate timed out")
            };
        }
        Reply::ok(SCRIPT)
    });
    let proxy = start(&scratch("remote-frame-close-in-setup"), &["--no-persist"]);
    let origin = listener();
    let port = origin.local_addr().unwrap().port();
    rules(
        &proxy,
        &format!(
            "localhost:{port} host://127.0.0.1:{port} frameScript://{}",
            source.url("/source")
        ),
    );
    let (mut client, mut peer) = websocket(&proxy, origin, false, "websocket");
    assert!(!arrived.recv_timeout(Duration::from_secs(3)).unwrap());
    let code = 1000_u16.to_be_bytes();
    send(&mut *client, true, 8, &code, true);
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    assert!(
        arrived.recv_timeout(Duration::from_secs(3)).unwrap(),
        "source gate timed out before release"
    );
    expect(&mut *peer, 8, &code, true);
    send(&mut *peer, true, 8, &code, false);
    expect(&mut *client, 8, &code, false);
    until(|| json(&proxy, "/api/ws/status?id=1")["live"] == false);
    assert_eq!(source.paths(), ["/source"]);
}

#[test]
fn a_truncated_frame_source_never_executes_its_valid_script_prefix() {
    let source = listener();
    let url = format!("http://{}/partial", source.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut peer = accept(&source);
        assert!(headers(&mut peer).starts_with("GET /partial "));
        write!(
            peer,
            "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n{SCRIPT}"
        )
        .unwrap();
    });
    let proxy = start(&scratch("remote-frame-truncated"), &["--no-persist"]);
    let origin = listener();
    let port = origin.local_addr().unwrap().port();
    rules(
        &proxy,
        &format!("localhost:{port} host://127.0.0.1:{port} frameScript://{url}"),
    );
    let (mut client, mut peer) = websocket(&proxy, origin, false, "websocket");
    send(&mut *client, true, 1, b"unchanged", true);
    expect(&mut *peer, 1, b"unchanged", true);
    let notes = json(&proxy, "/session.json?id=1")["unapplied"].clone();
    assert_eq!(notes.as_array().unwrap().len(), 1);
    assert_eq!(notes[0]["kind"], "rule-source-failed");
    assert!(
        notes[0]["reason"]
            .as_str()
            .unwrap()
            .contains("transport failed")
    );
    close(&mut *client, &mut *peer);
    server.join().unwrap();
}
