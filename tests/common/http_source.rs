//! A loopback HTTP(S) source with observable requests and explicit responses.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    pub extra: &'static str,
}
impl Reply {
    pub fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            body: body.into(),
            extra: "",
        }
    }
}

trait Io: Read + Write {}
impl<T: Read + Write> Io for T {}

pub struct Server {
    pub address: String,
    scheme: &'static str,
    pub requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    task: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    pub fn new(answer: impl Fn(&str, &str) -> Reply + Send + Sync + 'static) -> Self {
        Self::with_tls(None, answer)
    }
    pub fn with_tls(
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
    pub fn url(&self, path: &str) -> String {
        format!("{}://{}{path}", self.scheme, self.address)
    }
    pub fn paths(&self) -> Vec<String> {
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
