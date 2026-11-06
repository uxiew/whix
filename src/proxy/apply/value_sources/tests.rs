use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn resolve(url: &str) -> (Resolved, ReqInfo) {
    let mut rules = RuleManager::new();
    rules.set_text(&format!("example.test reqScript://{url}"));
    let info = build_req_info(
        "GET",
        "http",
        "example.test",
        80,
        "/",
        &HeaderMap::new(),
        None,
    );
    (rules.resolve(&info), info)
}

async fn head(socket: &mut tokio::net::TcpStream) {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        bytes.push(socket.read_u8().await.unwrap());
    }
}

#[tokio::test]
async fn exactly_256_kib_loads_but_one_more_byte_aborts_without_waiting_for_eof() {
    for oversized in [false, true] {
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (mut resolved, info) =
            resolve(&format!("http://{}/code", server.local_addr().unwrap()));
        let serving = tokio::spawn(async move {
            let (mut socket, _) = server.accept().await.unwrap();
            head(&mut socket).await;
            let length = if oversized {
                MAX_URL_VALUE + 1024
            } else {
                MAX_URL_VALUE
            };
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let sent = MAX_URL_VALUE + usize::from(oversized);
            socket.write_all(&vec![b' '; sent]).await.unwrap();
            if oversized {
                // No EOF and no remainder: only a streaming limit can finish.
                let n =
                    tokio::time::timeout(std::time::Duration::from_secs(3), socket.read(&mut [0]))
                        .await
                        .expect("the oversized fetch must release its connection")
                        .unwrap();
                assert_eq!(n, 0);
            }
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            load_remote_scripts(&mut resolved, &info, ScriptPhase::Request),
        )
        .await
        .expect("must reject the excess before the source finishes");
        serving.await.unwrap();
        let op = resolved.get("rulesFile").unwrap();
        assert!(op.value_is_content);
        if oversized {
            assert!(op.value.is_empty());
            let noted = info.noted.lock().unwrap();
            assert_eq!(noted.len(), 1);
            assert!(noted[0].reason.contains("262144"), "{:?}", *noted);
        } else {
            assert_eq!(op.value.len(), MAX_URL_VALUE);
            assert!(info.noted.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn a_truncated_script_never_runs_and_is_not_fetched_again_by_another_consumer() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (mut resolved, info) = resolve(&format!(
        "http://{}/partial",
        listener.local_addr().unwrap()
    ));
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        head(&mut socket).await;
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\nrules.push('* statusCode://599');").await.unwrap();
    });
    load_remote_scripts(&mut resolved, &info, ScriptPhase::Request).await;
    server.await.unwrap();
    assert!(resolved.value("rulesFile").unwrap().is_empty());
    load_remote_scripts(&mut resolved, &info, ScriptPhase::Request).await;
    assert_eq!(info.noted.lock().unwrap().len(), 1);
    assert!(merge_included_rules(&mut resolved, &info, &HashMap::new(), false).is_empty());
    assert!(resolved.get("statusCode").is_none());
}

#[tokio::test]
async fn a_stalled_source_obeys_the_existing_16_second_deadline() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (mut resolved, info) = resolve(&format!(
        "http://{}/stalled",
        listener.local_addr().unwrap()
    ));
    let (ready, waiting) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        head(&mut socket).await;
        ready.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    let loading = tokio::spawn(async move {
        load_remote_scripts(&mut resolved, &info, ScriptPhase::Request).await;
        (resolved, info)
    });
    waiting.await.unwrap();
    // Use a virtual clock only after real loopback I/O reached the source.
    tokio::time::pause();
    let paused_at = tokio::time::Instant::now();
    let deadline = std::time::Duration::from_secs(16);
    tokio::time::advance(deadline).await;
    let (resolved, info) = loading.await.unwrap();
    // Awaiting a task can auto-advance a paused clock: assert elapsed time too,
    // or a longer deadline with the same error text would still pass.
    // Tokio rounds deadlines up to a millisecond tick (TimeSource::deadline_to_tick).
    let elapsed = tokio::time::Instant::now() - paused_at;
    assert!(
        elapsed <= deadline + std::time::Duration::from_millis(1),
        "{elapsed:?}"
    );
    assert!(resolved.value("rulesFile").unwrap().is_empty());
    assert!(
        info.noted.lock().unwrap()[0]
            .reason
            .contains("timed out after 16")
    );
    server.abort();
}
