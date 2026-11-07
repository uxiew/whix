//! ctx.destroy is a connection abort, judged at both peers and the control API.
mod common;

use common::frame_peer::*;
use common::{Instance, get, request, scratch, start};
use serde_json::{Value, json};
use std::io::{ErrorKind, Write};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
enum Transport {
    Ws,
    Wss,
    Tcp,
    Raw,
}

struct Connection {
    proxy: Instance,
    client: Peer,
    origin: Peer,
    framed: bool,
}
impl Connection {
    fn open(name: &str, transport: Transport, script: &str, flags: &str) -> Self {
        let listener = listener();
        let port = listener.local_addr().unwrap().port();
        let proxy = start(&scratch(name), &["--no-persist", "--insecure-upstream"]);
        let (pattern, route, inspect) = match transport {
            Transport::Tcp => (
                format!("127.0.0.1:{port}"),
                String::new(),
                "enable://inspect",
            ),
            Transport::Raw => (
                format!("localhost:{port}"),
                format!("host://127.0.0.1:{port}"),
                "enable://inspect",
            ),
            _ => (
                format!("localhost:{port}"),
                format!("host://127.0.0.1:{port}"),
                "",
            ),
        };
        let rules = format!(
            "``` destroy.js\n{script}\n```\n{pattern} {route} {inspect} {flags} frameScript://{{destroy.js}}"
        );
        assert_eq!(
            request(&proxy.addr, "POST", "/api/rules", rules.as_bytes()).0,
            200
        );
        let (client, origin): (Peer, Peer) = match transport {
            Transport::Tcp => {
                let client = connect(&proxy, &pattern, true);
                (Box::new(client), Box::new(accept(&listener)))
            }
            _ => websocket(
                &proxy,
                listener,
                matches!(transport, Transport::Wss),
                if matches!(transport, Transport::Raw) {
                    "fixture"
                } else {
                    "websocket"
                },
            ),
        };
        Self {
            proxy,
            client,
            origin,
            framed: matches!(transport, Transport::Ws | Transport::Wss),
        }
    }
    fn send(&mut self, to_server: bool, data: &[u8]) {
        let peer = if to_server {
            &mut self.client
        } else {
            &mut self.origin
        };
        if self.framed {
            send(&mut **peer, true, 1, data, to_server);
        } else {
            peer.write_all(data).unwrap();
            peer.flush().unwrap();
        }
    }
    fn expect_at(&mut self, to_server: bool, data: &[u8]) {
        let peer = if to_server {
            &mut self.origin
        } else {
            &mut self.client
        };
        if self.framed {
            expect(&mut **peer, 1, data, to_server);
        } else {
            let mut got = vec![0; data.len()];
            peer.read_exact(&mut got).unwrap();
            assert_eq!(got, data);
        }
    }
    fn api(&self, path: &str) -> Value {
        serde_json::from_slice(&get(&self.proxy.addr, path).1).unwrap()
    }
    fn status(&self) -> Value {
        serde_json::from_slice(&get(&self.proxy.addr, "/api/ws/status?id=1").1).unwrap()
    }
    fn assert_closed(&mut self) {
        ended(&mut *self.client);
        ended(&mut *self.origin);
        until(|| self.status()["live"] == false);
        let (status, _) = request(
            &self.proxy.addr,
            "POST",
            "/api/ws/send",
            br#"{"id":1,"dir":"send","data":"after"}"#,
        );
        assert_ne!(status, 200);
        assert_eq!(get(&self.proxy.addr, "/api/status").0, 200);
    }
}

fn ended(peer: &mut dyn Io) {
    let mut byte = [0];
    match peer.read(&mut byte) {
        Ok(0) => {}
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::ConnectionReset
                    | ErrorKind::ConnectionAborted
                    | ErrorKind::UnexpectedEof
            ) => {}
        result => {
            panic!("expected transport end, not data or a Close frame: {result:?}, byte={byte:?}")
        }
    }
}
fn until(mut predicate: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(Instant::now() < end, "condition did not become true");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn top_level_destroy_closes_both_peers_without_a_close_frame() {
    for transport in [
        Transport::Ws,
        Transport::Wss,
        Transport::Tcp,
        Transport::Raw,
    ] {
        for (name, script) in [
            ("plain", "ctx.destroy();"),
            (
                "pending",
                "ctx.sendToServer('pending');ctx.sendToClient('pending');ctx.destroy();ctx.sendToClient('later');",
            ),
            (
                "throw",
                "ctx.destroy();ctx.destroy();throw new Error('after destroy');",
            ),
        ] {
            let mut wire = Connection::open(
                &format!("destroy-top-{transport:?}-{name}"),
                transport,
                script,
                "",
            );
            wire.assert_closed();
        }
    }
}

#[test]
fn handler_destroy_drops_the_trigger_and_terminates_both_directions() {
    for transport in [
        Transport::Ws,
        Transport::Wss,
        Transport::Tcp,
        Transport::Raw,
    ] {
        for to_server in [false, true] {
            for (name, after) in [
                ("return", "return 'MUST NOT PASS';"),
                ("repeat", "c.destroy();return buf;"),
                ("throw", "throw new Error('MUST NOT PASS');"),
            ] {
                let handler = if to_server {
                    "handleSendToServerFrame"
                } else {
                    "handleSendToClientFrame"
                };
                let script = format!("var c=ctx;c.{handler}=function(buf){{c.destroy();{after}}};");
                let mut wire = Connection::open(
                    &format!("destroy-handler-{transport:?}-{to_server}-{name}"),
                    transport,
                    &script,
                    "",
                );
                wire.send(to_server, b"trigger");
                wire.assert_closed();
            }
        }
    }
}

#[test]
fn writable_preceding_injections_arrive_but_later_injections_and_returned_data_do_not() {
    for transport in [
        Transport::Ws,
        Transport::Wss,
        Transport::Tcp,
        Transport::Raw,
    ] {
        for to_server in [false, true] {
            let handler = if to_server {
                "handleSendToServerFrame"
            } else {
                "handleSendToClientFrame"
            };
            let script = format!(
                "var c=ctx;c.{handler}=function(buf,opts){{if(opts.frameScript)return buf;c.sendToServer('beforeS');c.sendToClient('beforeC');c.destroy();c.sendToServer('AFTER');c.sendToClient('AFTER');throw new Error('AFTER');}};"
            );
            let mut wire = Connection::open(
                &format!("destroy-injections-{transport:?}-{to_server}"),
                transport,
                &script,
                "",
            );
            wire.send(to_server, b"trigger");
            wire.expect_at(true, b"beforeS");
            wire.expect_at(false, b"beforeC");
            wire.assert_closed();
        }
    }
}

#[test]
fn destroy_closes_hidden_connections_without_exposing_capture_or_controls() {
    for transport in [
        Transport::Ws,
        Transport::Wss,
        Transport::Tcp,
        Transport::Raw,
    ] {
        let mut wire = Connection::open(
            &format!("destroy-hidden-{transport:?}"),
            transport,
            "ctx.handleSendToServerFrame=function(){ctx.destroy();};",
            "enable://hide",
        );
        wire.send(true, b"hidden");
        wire.assert_closed();
        assert_eq!(wire.api("/sessions.json"), json!([]));
        assert_eq!(wire.api("/frames.json"), json!([]));
    }
}

#[test]
fn destroy_discards_the_other_directions_held_or_ignored_data() {
    for transport in [Transport::Ws, Transport::Tcp] {
        for mode in ["pauseReceive", "ignoreReceive"] {
            let mut wire = Connection::open(
                &format!("destroy-{mode}-{transport:?}"),
                transport,
                "ctx.handleSendToServerFrame=function(){ctx.destroy();};",
                &format!("enable://{mode}"),
            );
            wire.send(false, b"held-or-ignored");
            until(|| !wire.api("/frames.json?id=1").as_array().unwrap().is_empty());
            if mode == "pauseReceive" {
                assert_eq!(wire.status()["receive"]["held"], 1);
            }
            wire.send(true, b"destroy");
            wire.assert_closed();
            assert_eq!(wire.status()["receive"]["held"], 0);
        }
    }
}

#[test]
fn fragmented_and_binary_messages_can_destroy_without_forwarding_any_prefix() {
    for binary in [false, true] {
        let mut wire = Connection::open(
            &format!("destroy-fragments-{binary}"),
            Transport::Ws,
            "ctx.handleSendToServerFrame=function(){ctx.destroy();};",
            "",
        );
        send(
            &mut *wire.client,
            false,
            if binary { 2 } else { 1 },
            b"first",
            true,
        );
        send(&mut *wire.client, true, 9, b"ping", true);
        expect(&mut *wire.origin, 9, b"ping", true);
        send(&mut *wire.origin, true, 10, b"pong", false);
        expect(&mut *wire.client, 10, b"pong", false);
        send(&mut *wire.client, true, 0, b"last", true);
        wire.assert_closed();
    }
}

#[test]
fn destroy_from_an_injected_handler_or_serialization_callback_is_not_lost() {
    for (name, script) in [
        (
            "injected",
            "var c=ctx;c.handleSendToClientFrame=function(){c.destroy();};c.handleSendToServerFrame=function(buf){c.sendToClient('reply');return buf;};",
        ),
        (
            "serialized",
            "var c=ctx;c.handleSendToServerFrame=function(buf){c.sendToClient({toJSON:function(){c.destroy();return 'AFTER';}});return buf;};",
        ),
        ("legacy", "if(ctx.frame.data==='stop')ctx.destroy();"),
    ] {
        let mut wire = Connection::open(
            &format!("destroy-callback-{name}"),
            Transport::Ws,
            script,
            "",
        );
        wire.send(true, b"stop");
        wire.assert_closed();
    }
}

#[test]
fn destroying_one_connection_does_not_stop_another_or_the_next_connection() {
    let code = "ctx.handleSendToServerFrame=function(buf){if(String(buf)==='stop')ctx.destroy();else return buf;};";
    let mut first = Connection::open("destroy-isolation", Transport::Ws, code, "");
    for stopping_first in [true, false] {
        let listener = listener();
        let port = listener.local_addr().unwrap().port();
        let rules = format!(
            "``` s
{code}
```
localhost:{port} host://127.0.0.1:{port} frameScript://{{s}}"
        );
        assert_eq!(
            request(&first.proxy.addr, "POST", "/api/rules", rules.as_bytes()).0,
            200
        );
        let (mut client, mut origin) = websocket(&first.proxy, listener, false, "websocket");
        if stopping_first {
            first.send(true, b"stop");
            first.assert_closed();
        }
        send(&mut *client, true, 1, b"still alive", true);
        expect(&mut *origin, 1, b"still alive", true);
        send(&mut *origin, true, 1, b"reply", false);
        expect(&mut *client, 1, b"reply", false);
        close(&mut *client, &mut *origin);
    }
}

#[test]
fn a_remote_frame_script_can_destroy_its_connection() {
    let source = common::http_source::Server::new(|_, _| {
        common::http_source::Reply::ok("ctx.handleSendToServerFrame=function(){ctx.destroy();};")
    });
    let listener = listener();
    let port = listener.local_addr().unwrap().port();
    let proxy = start(&scratch("destroy-remote"), &["--no-persist"]);
    let rules = format!(
        "localhost:{port} host://127.0.0.1:{port} frameScript://{}",
        source.url("/script.js")
    );
    assert_eq!(
        request(&proxy.addr, "POST", "/api/rules", rules.as_bytes()).0,
        200
    );
    let (client, origin) = websocket(&proxy, listener, false, "websocket");
    let mut wire = Connection {
        proxy,
        client,
        origin,
        framed: true,
    };
    wire.send(true, b"stop");
    wire.assert_closed();
    assert_eq!(source.paths(), ["/script.js"]);
}

#[test]
fn mentioning_destroy_without_calling_it_leaves_normal_forwarding_and_close_intact() {
    for (name, script) in [
        ("literal", "var text='ctx.destroy()';"),
        ("condition", "if(false)ctx.destroy();"),
        ("reference", "var unused=ctx.destroy;"),
    ] {
        let mut wire = Connection::open(
            &format!("destroy-control-{name}"),
            Transport::Ws,
            script,
            "",
        );
        wire.send(true, b"unmodified");
        wire.expect_at(true, b"unmodified");
        wire.send(false, b"response");
        wire.expect_at(false, b"response");
        close(&mut *wire.client, &mut *wire.origin);
    }
}
