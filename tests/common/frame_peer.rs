//! Independent short-frame peers for real proxy tests; never uses the product codec.
use super::{Instance, get};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub trait Io: Read + Write {}
impl<T: Read + Write> Io for T {}
pub type Peer = Box<dyn Io + Send>;

pub fn headers(peer: &mut dyn Io) -> String {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        peer.read_exact(&mut byte).unwrap();
        head.push(byte[0]);
    }
    String::from_utf8(head).unwrap()
}
pub fn socket(address: &str) -> TcpStream {
    let socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    socket
}
pub fn listener() -> TcpListener {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    listener
}
pub fn accept(listener: &TcpListener) -> TcpStream {
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        match listener.accept() {
            Ok((socket, _)) => {
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                return socket;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < end, "origin was not connected");
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("accept: {error}"),
        }
    }
}
pub fn send(peer: &mut dyn Io, fin: bool, opcode: u8, data: &[u8], masked: bool) {
    assert!(data.len() < 126);
    let mut frame = vec![
        (if fin { 0x80 } else { 0 }) | opcode,
        data.len() as u8 | if masked { 0x80 } else { 0 },
    ];
    if masked {
        let key = [1, 2, 3, 4];
        frame.extend_from_slice(&key);
        frame.extend(data.iter().enumerate().map(|(i, byte)| byte ^ key[i % 4]));
    } else {
        frame.extend_from_slice(data);
    }
    peer.write_all(&frame).unwrap();
    peer.flush().unwrap();
}
pub fn expect(peer: &mut dyn Io, opcode: u8, data: &[u8], masked: bool) {
    let mut head = [0; 2];
    peer.read_exact(&mut head).unwrap();
    assert_eq!(head[0], 0x80 | opcode);
    assert_eq!(head[1] & 0x80 != 0, masked);
    let len = (head[1] & 0x7f) as usize;
    assert!(len < 126);
    let mut key = [0; 4];
    if masked {
        peer.read_exact(&mut key).unwrap();
    }
    let mut actual = vec![0; len];
    peer.read_exact(&mut actual).unwrap();
    if masked {
        for (i, byte) in actual.iter_mut().enumerate() {
            *byte ^= key[i % 4];
        }
    }
    assert_eq!(actual, data);
}
pub fn close(client: &mut dyn Io, origin: &mut dyn Io) {
    let code = 1000_u16.to_be_bytes();
    send(client, true, 8, &code, true);
    expect(origin, 8, &code, true);
    send(origin, true, 8, &code, false);
    expect(client, 8, &code, false);
}
pub fn websocket(
    proxy: &Instance,
    listener: TcpListener,
    tls: bool,
    upgrade: &'static str,
) -> (Peer, Peer) {
    let port = listener.local_addr().unwrap().port();
    let origin = std::thread::spawn(move || {
        let socket = accept(&listener);
        let mut peer: Peer = if tls {
            let rcgen::CertifiedKey { cert, key_pair } =
                rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
            let config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![cert.der().clone()],
                    rustls::pki_types::PrivatePkcs8KeyDer::from(key_pair.serialize_der()).into(),
                )
                .unwrap();
            Box::new(rustls::StreamOwned::new(
                rustls::ServerConnection::new(Arc::new(config)).unwrap(),
                socket,
            ))
        } else {
            Box::new(socket)
        };
        assert!(headers(&mut *peer).starts_with("GET /socket HTTP/1.1"));
        write!(peer, "HTTP/1.1 101 Switching Protocols\r\nUpgrade: {upgrade}\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n").unwrap();
        peer.flush().unwrap();
        peer
    });
    let mut tcp = socket(&proxy.addr);
    let mut client: Peer = if tls {
        write!(
            tcp,
            "CONNECT localhost:{port} HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"
        )
        .unwrap();
        assert!(headers(&mut tcp).starts_with("HTTP/1.1 200"));
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
        Box::new(rustls::StreamOwned::new(connection, tcp))
    } else {
        Box::new(tcp)
    };
    let target = if tls {
        "/socket".to_string()
    } else {
        format!("http://localhost:{port}/socket")
    };
    write!(client, "GET {target} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: Upgrade\r\nUpgrade: {upgrade}\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nCookie: fixture-only\r\nAuthorization: Bearer fixture-only\r\n\r\n").unwrap();
    client.flush().unwrap();
    assert!(headers(&mut *client).starts_with("HTTP/1.1 101"));
    (client, origin.join().unwrap())
}
pub fn connect(proxy: &Instance, address: &str, policy: bool) -> TcpStream {
    let mut client = socket(&proxy.addr);
    let policy = if policy {
        "x-whistle-policy: tunnel\r\n"
    } else {
        ""
    };
    write!(
        client,
        "CONNECT {address} HTTP/1.1\r\nHost: {address}\r\n{policy}\r\n"
    )
    .unwrap();
    assert!(headers(&mut client).starts_with("HTTP/1.1 200"));
    client
}
pub fn line(peer: &mut dyn Io) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\n") {
        let mut byte = [0];
        peer.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
    }
    String::from_utf8(bytes).unwrap()
}
pub fn finish_tcp(client: &mut TcpStream, peer: &mut TcpStream) {
    client.shutdown(Shutdown::Write).unwrap();
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    peer.shutdown(Shutdown::Write).unwrap();
    assert_eq!(client.read(&mut [0]).unwrap(), 0);
}
