//! Real CONNECT peers: controls are judged by bytes at both ends, not capture alone.
mod common;

use common::{Instance, get, request, scratch, start};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::time::{Duration, Instant};

fn post(proxy: &Instance, path: &str, body: Value) -> Value {
    let (status, bytes) = request(&proxy.addr, "POST", path, body.to_string().as_bytes());
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    serde_json::from_slice(&bytes).unwrap()
}

fn status(proxy: &Instance, id: u64) -> Value {
    serde_json::from_slice(&get(&proxy.addr, &format!("/api/ws/status?id={id}")).1).unwrap()
}

fn until(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(Instant::now() < end, "condition did not become true");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn tunnel(name: &str, flags: &str) -> (Instance, TcpStream, TcpStream, u64) {
    let origin = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = origin.local_addr().unwrap();
    let proxy = start(&scratch(name), &["--no-persist"]);
    let (code, _) = request(
        &proxy.addr,
        "POST",
        "/api/rules",
        format!("{address} enable://{flags}").as_bytes(),
    );
    assert_eq!(code, 200);
    let mut client = TcpStream::connect(&proxy.addr).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    write!(
        client,
        "CONNECT {address} HTTP/1.1\r\nHost: {address}\r\nx-whistle-policy: tunnel\r\n\r\n"
    )
    .unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        client.read_exact(&mut byte).unwrap();
        head.push(byte[0]);
    }
    assert!(head.starts_with(b"HTTP/1.1 200"));
    let (server, _) = origin.accept().unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut id = None;
    until(|| {
        let sessions: Value =
            serde_json::from_slice(&get(&proxy.addr, "/sessions.json").1).unwrap();
        id = sessions
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["method"] == "CONNECT")
            .and_then(|s| s["id"].as_u64());
        id.is_some()
    });
    (proxy, client, server, id.unwrap())
}

fn bytes(peer: &mut TcpStream, expected: &[u8]) {
    let mut actual = vec![0; expected.len()];
    peer.read_exact(&mut actual).unwrap();
    assert_eq!(actual, expected);
}

fn quiet(peer: &mut TcpStream) {
    peer.set_read_timeout(Some(Duration::from_millis(80)))
        .unwrap();
    let error = peer
        .read(&mut [0])
        .expect_err("paused/ignored bytes must not reach the peer");
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
    peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
}

#[test]
fn rule_pause_holds_tcp_bytes_until_release_and_keeps_the_reverse_half_open() {
    let (proxy, mut client, mut server, id) = tunnel("tcp-rule-pause", "pauseSend");
    client.write_all(b"held\0\xff").unwrap();
    until(|| status(&proxy, id)["send"]["held"] == 1);
    assert_eq!(status(&proxy, id)["send"]["mode"], "pause");
    quiet(&mut server);
    post(&proxy, "/api/ws/release", json!({"id":id,"dir":"send"}));
    bytes(&mut server, b"held\0\xff");
    client.shutdown(Shutdown::Write).unwrap();
    assert_eq!(server.read(&mut [0]).unwrap(), 0);
    assert_eq!(status(&proxy, id)["live"], true);
    server.write_all(b"response after EOF").unwrap();
    bytes(&mut client, b"response after EOF");
    server.shutdown(Shutdown::Write).unwrap();
    assert_eq!(client.read(&mut [0]).unwrap(), 0);
    until(|| status(&proxy, id)["live"] == false);
}

#[test]
fn rule_ignore_in_either_direction_discards_bytes_then_runtime_pass_resumes() {
    for (name, dir, flag) in [
        ("tcp-ignore-send", "send", "ignoreSend"),
        ("tcp-ignore-receive", "receive", "ignoreReceive"),
    ] {
        let (proxy, mut client, mut server, id) = tunnel(name, flag);
        let (sender, receiver) = if dir == "send" {
            (&mut client, &mut server)
        } else {
            (&mut server, &mut client)
        };
        assert_eq!(status(&proxy, id)[dir]["mode"], "ignore");
        sender.write_all(b"discard").unwrap();
        until(|| {
            let frames: Value =
                serde_json::from_slice(&get(&proxy.addr, &format!("/frames.json?id={id}")).1)
                    .unwrap();
            frames
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["dir"] == dir && f["ignored"] == true)
        });
        quiet(receiver);
        post(
            &proxy,
            "/api/ws/control",
            json!({"id":id,"dir":dir,"mode":"pass"}),
        );
        sender.write_all(b"next\0\xff").unwrap();
        bytes(receiver, b"next\0\xff");
        post(&proxy, "/api/ws/abort", json!({"id":id}));
    }
}

#[test]
fn live_tcp_controls_discard_held_chunks_reject_ws_injection_and_disconnect() {
    let (proxy, mut client, mut server, id) = tunnel("tcp-runtime-control", "inspect");
    until(|| status(&proxy, id)["live"] == true);
    assert_eq!(status(&proxy, id)["can_send"], false);
    for dir in ["send", "receive"] {
        let (sender, receiver) = if dir == "send" {
            (&mut client, &mut server)
        } else {
            (&mut server, &mut client)
        };
        post(
            &proxy,
            "/api/ws/control",
            json!({"id":id,"dir":dir,"mode":"pause"}),
        );
        sender.write_all(b"held").unwrap();
        until(|| status(&proxy, id)[dir]["held"] == 1);
        quiet(receiver);
        post(
            &proxy,
            "/api/ws/control",
            json!({"id":id,"dir":dir,"mode":"ignore"}),
        );
        post(
            &proxy,
            "/api/ws/control",
            json!({"id":id,"dir":dir,"mode":"pass"}),
        );
        sender.write_all(b"kept").unwrap();
        bytes(receiver, b"kept");
    }
    for payload in [
        json!({"id":id,"dir":"other","mode":"pause"}),
        json!({"id":id,"dir":"send","mode":"invalid"}),
    ] {
        assert_eq!(
            request(
                &proxy.addr,
                "POST",
                "/api/ws/control",
                payload.to_string().as_bytes()
            )
            .0,
            400
        );
    }
    assert_eq!(
        request(
            &proxy.addr,
            "POST",
            "/api/ws/send",
            json!({"id":id,"dir":"send","data":"not a WS connection"})
                .to_string()
                .as_bytes()
        )
        .0,
        400
    );
    quiet(&mut server);
    post(
        &proxy,
        "/api/ws/control",
        json!({"id":id,"dir":"send","mode":"pause"}),
    );
    client.write_all(b"pending").unwrap();
    until(|| status(&proxy, id)["send"]["held"] == 1);
    assert_eq!(
        post(&proxy, "/api/ws/abort", json!({"id":id}))["closed"],
        true
    );
    assert_eq!(server.read(&mut [0]).unwrap(), 0);
    assert_eq!(client.read(&mut [0]).unwrap(), 0);
    until(|| status(&proxy, id)["live"] == false);
    assert_eq!(
        post(&proxy, "/api/ws/abort", json!({"id":id}))["closed"],
        false
    );
    assert_eq!(
        request(
            &proxy.addr,
            "POST",
            "/api/ws/control",
            json!({"id":id,"dir":"send","mode":"pass"})
                .to_string()
                .as_bytes()
        )
        .0,
        400
    );
}

#[test]
fn hidden_raw_upgrade_runs_scripts_but_exposes_neither_frames_nor_controls() {
    for (name, flags, visible) in [
        ("tcp-hidden-upgrade", "hide|inspect", false),
        ("tcp-shown-upgrade", "hide|inspect|show", true),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let origin = std::thread::spawn(move || {
            let (mut server, _) = listener.accept().unwrap();
            server
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                server.read_exact(&mut byte).unwrap();
                head.push(byte[0]);
            }
            server.write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: raw\r\n\r\n").unwrap();
            bytes(&mut server, b"S:hidden");
            server.write_all(b"reply").unwrap();
            server
        });
        let proxy = start(&scratch(name), &["--no-persist"]);
        let rules = format!(
            "``` script.js\nctx.handleSendToServerFrame = function(buf) {{ return 'S:' + buf; }};\n```\n{address} enable://{flags} frameScript://{{script.js}}"
        );
        assert_eq!(
            request(&proxy.addr, "POST", "/api/rules", rules.as_bytes()).0,
            200
        );
        let mut client = TcpStream::connect(&proxy.addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        write!(client, "GET http://{address}/raw HTTP/1.1\r\nHost: {address}\r\nConnection: Upgrade\r\nUpgrade: raw\r\n\r\n").unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            client.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        assert!(head.starts_with(b"HTTP/1.1 101"));
        client.write_all(b"hidden").unwrap();
        bytes(&mut client, b"reply");
        let _server = origin.join().unwrap();
        let sessions: Value =
            serde_json::from_slice(&get(&proxy.addr, "/sessions.json").1).unwrap();
        let frames: Value = serde_json::from_slice(&get(&proxy.addr, "/frames.json").1).unwrap();
        assert_eq!(sessions.as_array().unwrap().len(), usize::from(visible));
        assert_eq!(
            frames.as_array().unwrap().len(),
            if visible { 2 } else { 0 }
        );
        // Each isolated instance allocates its first captured/hidden request id 1.
        assert_eq!(status(&proxy, 1)["live"], visible);
    }
}
