//! WS/WSS visibility from real peers and the public APIs, without UI filtering.
mod common;

use common::{Instance, get, request, scratch, start};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

trait Io: Read + Write {}
impl<T: Read + Write> Io for T {}
type Peer = Box<dyn Io + Send>;

fn headers(peer: &mut dyn Io) -> Vec<u8> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        peer.read_exact(&mut byte).unwrap();
        head.push(byte[0]);
    }
    head
}

// Independent small-frame codec: the fixture only sends payloads under 126 bytes.
fn send(peer: &mut dyn Io, fin: bool, opcode: u8, payload: &[u8], masked: bool) {
    assert!(payload.len() < 126);
    let mut frame = vec![
        (if fin { 0x80 } else { 0 }) | opcode,
        payload.len() as u8 | if masked { 0x80 } else { 0 },
    ];
    if masked {
        let key = [1, 2, 3, 4];
        frame.extend_from_slice(&key);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
    } else {
        frame.extend_from_slice(payload);
    }
    peer.write_all(&frame).unwrap();
    peer.flush().unwrap();
}

fn expect(peer: &mut dyn Io, opcode: u8, payload: &[u8], masked: bool) {
    let mut head = [0; 2];
    peer.read_exact(&mut head).unwrap();
    assert_eq!(head[0], 0x80 | opcode);
    assert_eq!(head[1] & 0x80 != 0, masked);
    let length = (head[1] & 0x7f) as usize;
    assert!(length < 126);
    let mut key = [0; 4];
    if masked {
        peer.read_exact(&mut key).unwrap();
    }
    let mut actual = vec![0; length];
    peer.read_exact(&mut actual).unwrap();
    if masked {
        for (i, b) in actual.iter_mut().enumerate() {
            *b ^= key[i % 4];
        }
    }
    assert_eq!(actual, payload);
}

fn status(proxy: &Instance) -> Value {
    serde_json::from_slice(&get(&proxy.addr, "/api/ws/status?id=1").1).unwrap()
}

fn json_get(proxy: &Instance, path: &str) -> Value {
    serde_json::from_slice(&get(&proxy.addr, path).1).unwrap()
}

fn post(proxy: &Instance, path: &str, body: Value) -> (u16, Value) {
    let (code, body) = request(&proxy.addr, "POST", path, body.to_string().as_bytes());
    (code, serde_json::from_slice(&body).unwrap())
}

fn until(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !check() {
        assert!(Instant::now() < deadline, "condition did not become true");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn open(dir: &Path, tls: bool, flags: &str, script: Option<&str>) -> (Instance, Peer, Peer) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let origin_tls = tls.then(|| {
        let rcgen::CertifiedKey { cert, key_pair } =
            rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![cert.der().clone()],
                    rustls::pki_types::PrivatePkcs8KeyDer::from(key_pair.serialize_der()).into(),
                )
                .unwrap(),
        )
    });
    let origin = std::thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut peer: Peer = match origin_tls {
            Some(config) => Box::new(rustls::StreamOwned::new(
                rustls::ServerConnection::new(config).unwrap(),
                socket,
            )),
            None => Box::new(socket),
        };
        assert!(headers(&mut *peer).starts_with(b"GET /socket HTTP/1.1"));
        peer.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n").unwrap();
        peer.flush().unwrap();
        peer
    });
    let proxy = start(dir, &["--insecure-upstream"]);
    let mut rules = format!("localhost:{port} host://127.0.0.1:{port} {flags}");
    if let Some(script) = script {
        rules = format!("``` frame.js\n{script}\n```\n{rules} frameScript://{{frame.js}}");
    }
    assert_eq!(
        request(&proxy.addr, "POST", "/api/rules", rules.as_bytes()).0,
        200
    );
    let mut socket = TcpStream::connect(&proxy.addr).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut client: Peer = if tls {
        write!(
            socket,
            "CONNECT localhost:{port} HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"
        )
        .unwrap();
        assert!(headers(&mut socket).starts_with(b"HTTP/1.1 200"));
        let pem = get(&proxy.addr, "/rootCA.crt").1;
        let mut roots = rustls::RootCertStore::empty();
        for cert in rustls_pemfile::certs(&mut pem.as_slice()) {
            roots.add(cert.unwrap()).unwrap();
        }
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connection =
            rustls::ClientConnection::new(Arc::new(config), "localhost".try_into().unwrap())
                .unwrap();
        Box::new(rustls::StreamOwned::new(connection, socket))
    } else {
        Box::new(socket)
    };
    let target = if tls {
        "/socket".to_string()
    } else {
        format!("http://localhost:{port}/socket")
    };
    write!(client, "GET {target} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").unwrap();
    client.flush().unwrap();
    assert!(headers(&mut *client).starts_with(b"HTTP/1.1 101"));
    (proxy, client, origin.join().unwrap())
}

fn close(client: &mut dyn Io, origin: &mut dyn Io) {
    let code = 1000_u16.to_be_bytes();
    send(client, true, 8, &code, true);
    expect(origin, 8, &code, true);
    send(origin, true, 8, &code, false);
    expect(client, 8, &code, false);
}

fn assert_hidden(proxy: &Instance) {
    assert_eq!(json_get(proxy, "/sessions.json"), json!([]));
    assert_eq!(json_get(proxy, "/frames.json"), json!([]));
    assert_eq!(json_get(proxy, "/frames.json?id=1"), json!([]));
    assert_eq!(json_get(proxy, "/session.json?id=1"), Value::Null);
    assert_eq!(
        status(proxy),
        json!({"live":false,"can_send":false,
        "send":{"mode":"pass","paused":false,"held":0},
        "receive":{"mode":"pass","paused":false,"held":0}})
    );
}

#[test]
fn hidden_ws_and_wss_keep_the_wire_but_expose_no_frames_or_controls() {
    for (name, tls, flags) in [
        ("ws-hidden", false, "enable://hide"),
        ("wss-hidden", true, "enable://hide"),
        ("ws-disable-show", false, "disable://show"),
    ] {
        let dir = scratch(name);
        let (proxy, mut client, mut origin) = open(&dir, tls, flags, None);
        send(&mut *client, true, 2, &[0, 255, 128], true);
        expect(&mut *origin, 2, &[0, 255, 128], true);
        send(&mut *origin, true, 1, b"private reply", false);
        expect(&mut *client, 1, b"private reply", false);
        assert_hidden(&proxy);
        for (path, body) in [
            (
                "/api/ws/send",
                json!({"id":1,"dir":"send","data":"must not inject"}),
            ),
            (
                "/api/ws/control",
                json!({"id":1,"dir":"send","mode":"ignore"}),
            ),
            ("/api/ws/release", json!({"id":1,"dir":"send"})),
        ] {
            assert_eq!(post(&proxy, path, body).0, 400);
        }
        assert_eq!(
            post(&proxy, "/api/ws/abort", json!({"id":1})),
            (200, json!({"ok":true,"closed":false}))
        );
        // Visibility is fixed with the connection's rules, not recalculated
        // from later edits or from whichever sessions remain in the list.
        assert_eq!(request(&proxy.addr, "POST", "/api/rules", b"").0, 200);
        send(&mut *client, true, 1, b"still connected", true);
        expect(&mut *origin, 1, b"still connected", true);
        close(&mut *client, &mut *origin);
        assert_hidden(&proxy);
        drop(client);
        drop(origin);
        drop(proxy);
        let restarted = common::restart(&dir);
        assert_hidden(&restarted);
    }
}

#[test]
fn hidden_scripts_injections_fragments_and_control_frames_are_not_recorded() {
    let script = "var c=ctx; c.sendToServer('server boot'); c.sendToClient('client boot');\nc.handleSendToServerFrame=function(buf,opts){if(opts.frameScript)return buf;c.sendToClient('ack');return 'S:'+buf;};\nc.handleSendToClientFrame=function(buf,opts){return opts.frameScript?buf:'C:'+buf;};";
    let (proxy, mut client, mut origin) = open(
        &scratch("ws-hidden-script"),
        false,
        "enable://hide",
        Some(script),
    );
    expect(&mut *origin, 1, b"server boot", true);
    expect(&mut *client, 1, b"client boot", false);
    send(&mut *client, false, 1, b"sec", true);
    send(&mut *client, true, 0, b"ret", true);
    expect(&mut *client, 1, b"ack", false);
    expect(&mut *origin, 1, b"S:secret", true);
    send(&mut *origin, true, 1, b"response", false);
    expect(&mut *client, 1, b"C:response", false);
    send(&mut *client, true, 9, b"ping", true);
    expect(&mut *origin, 9, b"ping", true);
    send(&mut *origin, true, 10, b"ping", false);
    expect(&mut *client, 10, b"ping", false);
    assert_hidden(&proxy);
    close(&mut *client, &mut *origin);
    assert_hidden(&proxy);
}

#[test]
fn show_overrides_and_visible_connections_remain_controllable_after_clearing_capture() {
    for (name, flags) in [
        ("ws-show", "enable://hide|show"),
        ("ws-disable-hide", "enable://hide disable://hide"),
    ] {
        let (proxy, mut client, mut origin) = open(&scratch(name), false, flags, None);
        send(&mut *client, true, 1, b"visible", true);
        expect(&mut *origin, 1, b"visible", true);
        assert_eq!(
            json_get(&proxy, "/sessions.json").as_array().unwrap().len(),
            1
        );
        assert_eq!(status(&proxy)["live"], true);
        assert_eq!(
            json_get(&proxy, "/frames.json").as_array().unwrap().len(),
            1
        );
        assert_eq!(post(&proxy, "/api/sessions/clear", json!({})).0, 200);
        assert_eq!(status(&proxy)["live"], true);
        send(&mut *client, true, 1, b"after clear", true);
        expect(&mut *origin, 1, b"after clear", true);
        until(|| json_get(&proxy, "/frames.json").as_array().unwrap().len() == 1);
        assert_eq!(
            post(
                &proxy,
                "/api/ws/send",
                json!({"id":1,"dir":"receive","data":"manual"})
            )
            .0,
            200
        );
        expect(&mut *client, 1, b"manual", false);
        close(&mut *client, &mut *origin);
        until(|| status(&proxy)["live"] == false);
    }
}
