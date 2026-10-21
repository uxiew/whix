mod common;

use base64::Engine;
use common::{request, scratch, start};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

fn origin() -> (String, mpsc::Receiver<(String, Vec<u8>)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut b = [0];
            stream.read_exact(&mut b).unwrap();
            head.push(b[0]);
        }
        let head = String::from_utf8(head).unwrap();
        let len: usize = head
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse().unwrap())
            })
            .unwrap_or(0);
        let mut body = vec![0; len];
        stream.read_exact(&mut body).unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .unwrap();
        let _ = tx.send((head, body));
    });
    (format!("http://{addr}/upload"), rx)
}

fn post(addr: &str, path: &str, data: Value) -> Value {
    let (status, body) = request(addr, "POST", path, data.to_string().as_bytes());
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    serde_json::from_slice(&body).unwrap()
}

#[test]
fn composer_preserves_binary_multipart_bytes_and_boundary() {
    let proxy = start(&scratch("traffic-composer"), &[]);
    let (url, captured) = origin();
    let bytes = b"--raw-boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"x.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n\x00\xff\x80\r\n--raw-boundary--\r\n";
    let result = post(
        &proxy.addr,
        "/api/composer",
        json!({
            "method":"POST", "url":url, "headers":"Content-Type: multipart/form-data; boundary=raw-boundary\nContent-Length: 1",
            "body":base64::engine::general_purpose::STANDARD.encode(bytes), "body_encoding":"base64"
        }),
    );
    assert_eq!(result["sent"], bytes.len());
    let (headers, actual) = captured.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(headers.contains("multipart/form-data; boundary=raw-boundary"));
    assert_eq!(actual, bytes);
}

#[test]
fn har_import_is_offline_and_binary_replay_uses_new_session_ids() {
    let proxy = start(&scratch("traffic-har"), &[]);
    let (url, captured) = origin();
    let bytes = [0, 255, 128, 13, 10];
    let source = json!({"log":{"version":"1.2","entries":[{
        "id":999, "startedDateTime":"2026-10-19T12:00:00.250+09:00", "time":1.2,
        "request":{"method":"POST","url":url,"headers":[{"name":"Content-Type","value":"application/octet-stream"}],"bodySize":5,
        "postData":{"mimeType":"application/octet-stream","encoding":"base64","text":base64::engine::general_purpose::STANDARD.encode(bytes)}},
        "response":{"status":200,"headers":[],"content":{"size":2,"text":"ok"}}
    }]}});
    let imported = post(&proxy.addr, "/api/sessions/import", source.clone());
    let id = imported["ids"][0].as_u64().unwrap();
    assert_ne!(id, 999);
    assert!(captured.try_recv().is_err(), "import must not replay");
    let again = post(&proxy.addr, "/api/sessions/import", source);
    assert!(again["ids"][0].as_u64().unwrap() > id);
    let detail: Value =
        serde_json::from_slice(&common::get(&proxy.addr, &format!("/session.json?id={id}")).1)
            .unwrap();
    assert_eq!(detail["imported"], true);
    assert_eq!(detail["time_ms"], 1792378800250_u64);
    post(&proxy.addr, "/api/replay", json!({"ids":[id]}));
    assert_eq!(
        captured.recv_timeout(Duration::from_secs(5)).unwrap().1,
        bytes
    );
}

#[test]
fn rejected_import_is_atomic_and_partial_import_survives_restart() {
    let dir = scratch("traffic-har-partial");
    let proxy = start(&dir, &[]);
    let entry = json!({
        "startedDateTime":"2026-10-19T00:00:00Z","time":0,
        "request":{"method":"POST","url":"http://127.0.0.1:1/never-send","bodySize":8,"headers":[],
            "postData":{"text":"abc","mimeType":"application/octet-stream"}},
        "response":{"status":0,"headers":[],"content":{"size":0}}
    });
    let mut bad = entry.clone();
    bad["request"]["postData"] = json!({"text":"!","encoding":"base64"});
    let (status, _) = request(
        &proxy.addr,
        "POST",
        "/api/sessions/import",
        json!({"log":{"entries":[entry.clone(),bad]}})
            .to_string()
            .as_bytes(),
    );
    assert_eq!(status, 400);
    let sessions: Value =
        serde_json::from_slice(&common::get(&proxy.addr, "/sessions.json").1).unwrap();
    assert_eq!(sessions, json!([]), "no valid prefix may be imported");
    let result = post(
        &proxy.addr,
        "/api/sessions/import",
        json!({"log":{"entries":[entry]}}),
    );
    assert_eq!(result["imported"], 1);
    assert!(!result["warnings"].as_array().unwrap().is_empty());
    let id = result["ids"][0].as_u64().unwrap();
    drop(proxy);
    let proxy = common::restart(&dir);
    let detail: Value =
        serde_json::from_slice(&common::get(&proxy.addr, &format!("/session.json?id={id}")).1)
            .unwrap();
    assert_eq!(detail["imported"], true);
    assert_eq!(detail["req_body"]["truncated"], true);
    assert_eq!(
        common::get(&proxy.addr, &format!("/body.bin?id={id}&side=req")).1,
        b"abc"
    );
    let (status, body) = request(
        &proxy.addr,
        "POST",
        "/api/replay",
        json!({"id":id}).to_string().as_bytes(),
    );
    assert_eq!(status, 400, "{}", String::from_utf8_lossy(&body));
    assert!(String::from_utf8_lossy(&body).contains("incomplete"));
}

#[test]
fn composer_invalid_binary_and_unconfirmed_partial_are_rejected_before_sending() {
    let proxy = start(&scratch("traffic-invalid"), &[]);
    for fields in [
        json!({"body":"bad!","body_encoding":"base64"}),
        json!({"body":"abc","body_incomplete":true}),
        json!({"body":"abc","body_encoding":"hex"}),
    ] {
        let mut input = fields;
        input["url"] = json!("http://127.0.0.1:1/must-not-connect");
        let (status, _) = request(
            &proxy.addr,
            "POST",
            "/api/composer",
            input.to_string().as_bytes(),
        );
        assert_eq!(status, 400);
    }
    let (url, captured) = origin();
    let result = post(
        &proxy.addr,
        "/api/composer",
        json!({"method":"POST","url":url,"body":"abc","body_incomplete":true,"allow_partial":true}),
    );
    assert_eq!(result["sent"], 3);
    assert_eq!(
        captured.recv_timeout(Duration::from_secs(5)).unwrap().1,
        b"abc"
    );
}
