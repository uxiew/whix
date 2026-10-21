//! Offline HAR ingestion: validate the whole batch before assigning any IDs.
//! Captured bodies are already decoded entities, never decompress them again.
use super::*;
use serde_json::Value;

pub(super) async fn sessions_import(
    state: &Arc<AppState>,
    req: Request<Incoming>,
) -> Response<DynBody> {
    let bytes = match read_body(req).await {
        Ok(bytes) => bytes,
        Err(response) => return *response,
    };
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    let har: Value = match serde_json::from_slice(bytes) {
        Ok(har) => har,
        Err(_) => return json_error("invalid HAR JSON"),
    };
    let (sessions, warnings) = match parse_har(&har, state.config.body_preview_cap) {
        Ok(result) => result,
        Err(error) => return json_error(&error),
    };
    let imported = sessions.len();
    let (ids, retained, evicted) = state.import_sessions(sessions);
    state.flush_history().await;
    json_value(
        &serde_json::json!({"ok":true,"imported":imported,"retained":retained,"evicted":evicted,"ids":ids,"warnings":warnings}),
    )
}

fn headers(value: &Value) -> Result<Vec<(String, String)>, String> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    value
        .as_array()
        .ok_or("headers must be an array")?
        .iter()
        .map(|h| {
            let name = h["name"].as_str().ok_or("header name must be text")?;
            let value = h["value"].as_str().ok_or("header value must be text")?;
            // HAR can include HTTP/2 pseudo-headers. Preserve them for viewing;
            // replay rebuilds the request line from method/URL instead.
            if !matches!(
                name,
                ":authority" | ":method" | ":scheme" | ":path" | ":status" | ":protocol"
            ) {
                hyper::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| "invalid header name")?;
            }
            hyper::header::HeaderValue::from_str(value).map_err(|_| "invalid header value")?;
            Ok((name.into(), value.into()))
        })
        .collect()
}

fn known_size(value: &Value) -> Option<usize> {
    value.as_u64().and_then(|n| usize::try_from(n).ok())
}

fn parameter<'a>(mime: &'a str, name: &str) -> Option<&'a str> {
    mime.split(';')
        .skip(1)
        .filter_map(|p| p.trim().split_once('='))
        .find(|(key, _)| key.trim().eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim().trim_matches('"'))
}

fn body(
    field: &Value,
    message: &Value,
    headers: &mut Vec<(String, String)>,
    request: bool,
    cap: usize,
    warnings: &mut Vec<String>,
) -> Result<Option<Capture>, String> {
    let mime = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.clone())
        .or_else(|| field["mimeType"].as_str().map(str::to_string));
    if let Some(mime) = &mime
        && !headers
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("content-type"))
    {
        hyper::header::HeaderValue::from_str(mime).map_err(|_| "invalid mimeType")?;
        headers.push(("content-type".into(), mime.clone()));
    }
    let has_bytes = field.get("base64").is_some() || field.get("text").is_some();
    let explicit_base64 = field.get("base64");
    let encoding = match field.get("encoding") {
        None | Some(Value::Null) => "",
        Some(v) => v.as_str().ok_or("body encoding must be text")?,
    };
    let bytes = if let Some(encoded) = explicit_base64 {
        base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            encoded.as_str().ok_or("base64 must be text")?,
        )
        .map_err(|_| "invalid base64 body")?
    } else if encoding == "base64" {
        base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            field["text"]
                .as_str()
                .ok_or("base64 body text is required")?,
        )
        .map_err(|_| "invalid base64 body")?
    } else if encoding.is_empty() {
        match field.get("text") {
            None => Vec::new(),
            Some(text) => text
                .as_str()
                .ok_or("body text must be a string")?
                .as_bytes()
                .to_vec(),
        }
    } else {
        return Err("unsupported body encoding (expected base64 or decoded text)".into());
    };
    let compressed = headers.iter().any(|(n, v)| {
        n.eq_ignore_ascii_case("content-encoding")
            && !v.is_empty()
            && !v.eq_ignore_ascii_case("identity")
    });
    let wire = known_size(&message["bodySize"]).or_else(|| {
        headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.parse().ok())
    });
    let expected = if request {
        if compressed { None } else { wire }
    } else {
        known_size(&field["size"])
    };
    let params_only = !has_bytes && field["params"].as_array().is_some_and(|p| !p.is_empty());
    let missing = !has_bytes
        && (wire.is_some_and(|n| n > 0)
            || expected.is_some_and(|n| n > 0)
            || params_only
            || message["bodySize"].as_i64() == Some(-1));
    let non_utf8_text = !bytes.is_empty()
        && explicit_base64.is_none()
        && encoding.is_empty()
        && mime
            .as_deref()
            .and_then(|s| parameter(s, "charset"))
            .is_some_and(|s| {
                !s.eq_ignore_ascii_case("utf-8")
                    && !s.eq_ignore_ascii_case("utf8")
                    && !(s.eq_ignore_ascii_case("us-ascii") && bytes.is_ascii())
            });
    let missing_boundary = request
        && !bytes.is_empty()
        && mime.as_deref().is_some_and(|m| {
            m.to_ascii_lowercase().starts_with("multipart/")
                && parameter(m, "boundary").is_none_or(str::is_empty)
        });
    let truncated = missing
        || field["_truncated"].as_bool().unwrap_or(false)
        || bytes.len() > cap
        || expected.is_some_and(|n| n > bytes.len())
        || non_utf8_text
        || missing_boundary;
    let total = wire
        .or(expected)
        .unwrap_or(bytes.len())
        .max(if missing { 1 } else { 0 });
    if truncated {
        warnings.push(if params_only { "multipart params lack the original body/boundary; not reconstructed" } else { "body is missing, truncated, over the preview limit, or not byte-exact; do not replay it as complete" }.into());
    }
    if !has_bytes && !missing {
        return Ok(None);
    }
    Ok(Some(Capture::restored(
        &bytes[..bytes.len().min(cap)],
        mime,
        total,
        truncated,
        non_utf8_text || missing_boundary,
    )))
}

pub(super) fn parse_har(har: &Value, cap: usize) -> Result<(Vec<Session>, Vec<String>), String> {
    let log = har
        .get("log")
        .and_then(Value::as_object)
        .ok_or("expected HAR log object")?;
    if log.get("version").is_some_and(|v| v != "1.2") {
        return Err("only HAR 1.2 is supported".into());
    }
    let entries = log
        .get("entries")
        .and_then(Value::as_array)
        .ok_or("expected log.entries array")?;
    if entries.len() > 2000 {
        return Err("a HAR import is limited to 2000 entries".into());
    }
    let mut warnings = Vec::new();
    let sessions = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let mut notes = Vec::new();
            let result =
                parse_entry(entry, cap, &mut notes).map_err(|e| format!("entries[{i}]: {e}"));
            warnings.extend(
                notes
                    .into_iter()
                    .map(|note| format!("entries[{i}]: {note}")),
            );
            result
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((sessions, warnings))
}

fn parse_entry(entry: &Value, cap: usize, notes: &mut Vec<String>) -> Result<Session, String> {
    let req = &entry["request"];
    let res = &entry["response"];
    if !req.is_object() || !res.is_object() {
        return Err("request and response are required".into());
    }
    let url = req["url"].as_str().ok_or("request URL is required")?;
    let uri: hyper::Uri = url.parse().map_err(|_| "invalid request URL")?;
    if uri.authority().is_none()
        || !matches!(uri.scheme_str(), Some("http" | "https" | "ws" | "wss"))
    {
        return Err("request URL must be absolute HTTP(S) or WS(S)".into());
    }
    let method = req["method"].as_str().ok_or("request method is required")?;
    hyper::Method::from_bytes(method.as_bytes()).map_err(|_| "invalid request method")?;
    let status = res["status"]
        .as_u64()
        .filter(|n| *n == 0 || (100..=599).contains(n))
        .ok_or("invalid response status")? as u16;
    let date = entry["startedDateTime"]
        .as_str()
        .ok_or("startedDateTime is required")?;
    let time = time::OffsetDateTime::parse(date, &time::format_description::well_known::Rfc3339)
        .map_err(|_| "invalid RFC3339 startedDateTime")?
        .unix_timestamp_nanos();
    if time < 0 {
        return Err("timestamps before the Unix epoch are not supported".into());
    }
    let duration = entry["time"]
        .as_f64()
        .filter(|v| v.is_finite() && *v >= 0.0 && *v <= u64::MAX as f64)
        .ok_or("invalid duration")?;
    let mut req_headers = headers(&req["headers"])?;
    let mut res_headers = headers(&res["headers"])?;
    let req_body = body(&req["postData"], req, &mut req_headers, true, cap, notes)?;
    let res_body = body(&res["content"], res, &mut res_headers, false, cap, notes)?;
    if entry.get("_error").is_some_and(|e| !e.is_null()) {
        notes.push("source _error is not assigned a guessed live failure phase".into());
    }
    Ok(Session {
        id: 0,
        time_ms: (time / 1_000_000) as u128,
        duration_ms: duration.round() as u128,
        method: method.into(),
        url: url.into(),
        status,
        target: "imported HAR".into(),
        req_headers,
        res_headers,
        req_body,
        res_body,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry() -> Value {
        json!({
            "startedDateTime":"2026-10-19T12:00:00+09:00", "time":2.5,
            "request":{"method":"POST","url":"http://fixture.test/upload","bodySize":0,"headers":[]},
            "response":{"status":200,"bodySize":0,"headers":[],"content":{"size":0}}
        })
    }

    #[test]
    fn accepts_standard_and_whistle_exact_bytes_without_decoding_twice() {
        for field in [
            json!({"text":"AP+A","encoding":"base64"}),
            json!({"text":"display only","base64":"AP+A"}),
        ] {
            let mut e = entry();
            e["request"]["bodySize"] = json!(3);
            e["request"]["headers"] = json!([
                {"name":":authority","value":"fixture.test"},
                {"name":":method","value":"POST"}
            ]);
            e["request"]["postData"] = field.clone();
            e["response"]["content"] = field;
            e["response"]["headers"] = json!([{"name":":status","value":"200"},{"name":"Content-Encoding","value":"gzip"}]);
            e["response"]["bodySize"] = json!(23);
            let mut warnings = Vec::new();
            let s = parse_entry(&e, 100, &mut warnings).unwrap();
            assert_eq!(
                s.req_headers[0],
                (":authority".into(), "fixture.test".into())
            );
            for body in [s.req_body, s.res_body] {
                let body = body.unwrap();
                assert_eq!(body.preview_bytes().bytes, &[0, 255, 128][..]);
                assert!(matches!(body.replay_body(), ReplayBody::Whole(_)));
            }
            assert!(warnings.is_empty());
        }
    }

    #[test]
    fn missing_params_only_and_capped_bodies_are_not_complete() {
        for (field, cap) in [
            (
                json!({"mimeType":"multipart/form-data","params":[{"name":"file","fileName":"x.bin"}]}),
                100,
            ),
            (json!({"text":"abcdef"}), 3),
            (json!({"text":"abc","_truncated":true}), 100),
            (
                json!({"text":"abc","mimeType":"text/plain; CHARSET = latin1"}),
                100,
            ),
            (
                json!({"text":"abc","mimeType":"multipart/form-data; boundary=\"\""}),
                100,
            ),
            (Value::Null, 100),
        ] {
            let mut e = entry();
            e["request"]["bodySize"] = json!(6);
            e["request"]["postData"] = field;
            let mut warnings = Vec::new();
            let s = parse_entry(&e, cap, &mut warnings).unwrap();
            assert!(!warnings.is_empty());
            assert!(!matches!(
                s.req_body.unwrap().replay_body(),
                ReplayBody::Whole(_) | ReplayBody::Empty
            ));
        }
    }

    #[test]
    fn multipart_boundary_and_charset_parameters_allow_spaces_and_quotes() {
        let mut e = entry();
        e["request"]["bodySize"] = json!(3);
        e["request"]["postData"] = json!({"text":"abc","mimeType":"multipart/form-data; boundary = \"abc\"; charset = \"UTF-8\""});
        let mut warnings = Vec::new();
        let s = parse_entry(&e, 100, &mut warnings).unwrap();
        assert!(matches!(
            s.req_body.unwrap().replay_body(),
            ReplayBody::Whole(_)
        ));
        assert!(warnings.is_empty());
    }

    #[test]
    fn malformed_entries_reject_the_whole_batch() {
        for field in [
            json!({"text":"%","encoding":"base64"}),
            json!({"text":7,"encoding":"base64"}),
            json!({"encoding":"base64"}),
            json!({"text":"abc","encoding":7}),
            json!({"text":7}),
            json!({"text":"abc","base64":"?"}),
        ] {
            let mut bad = entry();
            bad["request"]["postData"] = field;
            let har = json!({"log":{"version":"1.2","entries":[entry(),bad]}});
            assert!(
                parse_har(&har, 100)
                    .err()
                    .unwrap()
                    .starts_with("entries[1]:")
            );
        }
    }

    #[test]
    fn validates_dates_urls_headers_and_entry_limit() {
        let mut invalid = vec![];
        for (path, value) in [
            ("/startedDateTime", json!("2026-02-30T00:00:00Z")),
            ("/request/url", json!("file:///etc/hosts")),
            ("/request/method", json!("bad method")),
            (
                "/request/headers",
                json!([{"name":"x-test","value":"line\r\nbreak"}]),
            ),
            ("/response/status", json!(999)),
            ("/time", json!(-1)),
        ] {
            let mut e = entry();
            *e.pointer_mut(path).unwrap() = value;
            invalid.push(e);
        }
        for e in invalid {
            assert!(parse_entry(&e, 100, &mut Vec::new()).is_err());
        }
        assert!(parse_har(&json!({"log":{"entries":vec![entry(); 2001]}}), 100).is_err());
    }
    #[test]
    fn append_keeps_order_assigns_fresh_ids_obeys_capacity_and_skips_observers() {
        let config = crate::config::Config {
            storage_dir: std::env::temp_dir()
                .join(format!("whix-har-append-{}", std::process::id())),
            persist_sessions: false,
            req_cache_size: 2,
            ..Default::default()
        };
        let ca = crate::ca::CertAuthority::load_or_create(&config).unwrap();
        let state = AppState::new(config, crate::rules::RuleManager::new(), ca);
        state.observe(|_| panic!("offline import must not notify live observers"));
        let make = || parse_entry(&entry(), 100, &mut Vec::new()).unwrap();
        let (first, _, _) = state.import_sessions(vec![make()]);
        let (ids, retained, evicted) = state.import_sessions(vec![make(), make(), make()]);
        assert_eq!((retained, evicted), (2, 2));
        assert!(ids[0] > first[0] && ids.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(
            state
                .sessions
                .lock()
                .unwrap()
                .iter()
                .map(|s| s.id)
                .collect::<Vec<_>>(),
            ids[1..]
        );
        assert_eq!(state.import_sessions(Vec::new()), (Vec::new(), 0, 0));
    }
}
