//! Real HTTP requests, persisted state and restarts: ordering is behavior, not
//! just the order of names in a JSON response. Every address/data directory is
//! owned by this test, and the rules answer locally without reaching a host.
mod common;

use common::{get, request, restart, scratch, start};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;

fn post(addr: &str, path: &str, value: &Value) -> Value {
    let (status, body) = request(addr, "POST", path, value.to_string().as_bytes());
    assert_eq!(status, 200, "{path}: {}", String::from_utf8_lossy(&body));
    let answer: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(answer["ok"], true, "{path}: {answer}");
    answer
}

fn import(addr: &str, mut input: Value) {
    let preview = post(addr, "/api/import/preview", &input);
    input["expected"] = preview["token"].clone();
    post(addr, "/api/import/apply", &input);
}

fn names(addr: &str) -> Vec<String> {
    let (_, bytes) = get(addr, "/api/rule-groups");
    serde_json::from_slice::<Vec<Value>>(&bytes)
        .unwrap()
        .iter()
        .map(|g| g["name"].as_str().unwrap().to_string())
        .collect()
}

fn snapshot(addr: &str) -> Value {
    serde_json::from_slice(&get(addr, "/api/export").1).unwrap()
}

fn refused(addr: &str, path: &str, value: &Value) {
    let (status, bytes) = request(addr, "POST", path, value.to_string().as_bytes());
    assert_eq!(status, 400, "{path}: {}", String::from_utf8_lossy(&bytes));
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["ok"],
        false
    );
}

fn answer(addr: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(b"GET http://rank.test/ HTTP/1.0\r\nHost: rank.test\r\n\r\n")
        .unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let status = text.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, text.split_once("\r\n\r\n").unwrap().1.to_string())
}

#[test]
fn imported_order_rename_reorder_and_restart_keep_the_real_winner() {
    let dir = scratch("config-migration-order");
    let proxy = start(&dir, &[]);
    import(
        &proxy.addr,
        json!({ "format": "whistle-values", "data": serde_json::from_str::<Value>(include_str!("data/whistle/values.json")).unwrap() }),
    );
    // Deliberately not object-key order. Numeric keys and folder headings are
    // present in Whistle's actual export format.
    import(
        &proxy.addr,
        json!({ "format": "whistle-rules", "enable_new": true, "data": serde_json::from_str::<Value>(include_str!("data/whistle/rules.json")).unwrap() }),
    );
    assert_eq!(names(&proxy.addr), ["default", "10", "2"]);
    assert_eq!(
        answer(&proxy.addr),
        (201, "first-value\r\nsecond\r\nfallback".into())
    );
    post(
        &proxy.addr,
        "/api/rule-group/rename",
        &json!({"name":"10", "to":"测试 / A+%"}),
    );
    assert_eq!(
        answer(&proxy.addr),
        (201, "first-value\r\nsecond\r\nfallback".into())
    );
    post(
        &proxy.addr,
        "/api/rule-groups/reorder",
        &json!({"names":["2", "测试 / A+%"]}),
    );
    assert_eq!(
        answer(&proxy.addr),
        (202, "second\r\nfirst-value\r\nfallback".into())
    );
    let (_, detail) = get(
        &proxy.addr,
        "/api/rule-group?name=%E6%B5%8B%E8%AF%95%20%2F%20A%2B%25",
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&detail).unwrap()["name"],
        "测试 / A+%"
    );
    drop(proxy);
    let proxy = restart(&dir);
    assert_eq!(names(&proxy.addr), ["default", "2", "测试 / A+%"]);
    assert_eq!(
        answer(&proxy.addr),
        (202, "second\r\nfirst-value\r\nfallback".into())
    );
}

#[test]
fn preview_is_read_only_and_stale_confirmation_cannot_overwrite_changes() {
    let dir = scratch("config-migration-stale");
    let proxy = start(&dir, &[]);
    let before = get(&proxy.addr, "/api/export").1;
    let mut input = json!({"format":"whistle-rules", "data": {
        "first": "rank.test statusCode://200 resBody://first", "": ["Default", "first"]
    }});
    let preview = post(&proxy.addr, "/api/import/preview", &input);
    assert_eq!(get(&proxy.addr, "/api/export").1, before);
    post(
        &proxy.addr,
        "/api/value",
        &json!({"name":"changed", "value":"after preview"}),
    );
    input["expected"] = preview["token"].clone();
    let (status, _) = request(
        &proxy.addr,
        "POST",
        "/api/import/apply",
        input.to_string().as_bytes(),
    );
    assert_eq!(status, 400);
    assert!(!names(&proxy.addr).iter().any(|n| n == "first"));
}

#[test]
fn merge_reorders_the_imported_block_without_deleting_unrelated_data_and_is_repeatable() {
    let dir = scratch("config-migration-merge");
    let proxy = start(&dir, &[]);
    for name in ["keep", "B", "middle", "A", "tail"] {
        post(
            &proxy.addr,
            "/api/rule-groups",
            &json!({"name":name,"text":format!("# {name}"),"enabled":name!="B"}),
        );
    }
    post(
        &proxy.addr,
        "/api/value",
        &json!({"name":"keep-value","value":"untouched"}),
    );
    let source = json!({"format":"whistle-rules", "data": {"B":"# new B", "A":"# new A", "new":"# new", "":{"list":["Default","A","A","absent","B","new"]}}});
    let preview = post(&proxy.addr, "/api/import/preview", &source);
    assert_eq!(
        preview["before"],
        json!(["default", "keep", "B", "middle", "A", "tail"])
    );
    assert_eq!(
        preview["after"],
        json!(["default", "keep", "A", "B", "new", "middle", "tail"])
    );
    assert_eq!(
        preview["groups"][1]["enabled"], false,
        "existing switch kept"
    );
    assert_eq!(
        preview["groups"][2]["enabled"], false,
        "new groups default off"
    );
    import(&proxy.addr, source.clone());
    let once = snapshot(&proxy.addr);
    import(&proxy.addr, source);
    assert_eq!(snapshot(&proxy.addr), once);
    assert_eq!(once["values"]["keep-value"], "untouched");
    assert_eq!(once["rules"][1]["text"], "# keep");
    drop(proxy);
    assert_eq!(snapshot(&restart(&dir).addr), once);
}

#[test]
fn invalid_imports_renames_and_permutations_leave_the_configuration_untouched() {
    let dir = scratch("config-migration-invalid");
    let proxy = start(&dir, &[]);
    post(
        &proxy.addr,
        "/api/rule-groups",
        &json!({"name":"A","text":"# A"}),
    );
    post(
        &proxy.addr,
        "/api/rule-groups",
        &json!({"name":"B","text":"# B"}),
    );
    let before = snapshot(&proxy.addr);
    for source in [
        json!({"format":"whistle-rules","data":{"good":"# valid","bad":{},"":["good","bad"]}}),
        json!({"format":"whistle-rules","data":{"default":"# cannot rename Default silently","":[]}}),
        json!({"format":"whistle-values","data":{"value":"v","":"not an order list"}}),
        json!({"format":"whix","data":{"whix":"test","rules":[{"name":"A","text":"a"},{"name":"A","text":"b"}]}}),
    ] {
        refused(&proxy.addr, "/api/import/preview", &source);
        refused(&proxy.addr, "/api/import/apply", &source);
        assert_eq!(snapshot(&proxy.addr), before);
    }
    for names in [
        json!(["A", "A"]),
        json!(["A"]),
        json!(["A", "absent"]),
        json!(["default", "A", "B"]),
    ] {
        refused(
            &proxy.addr,
            "/api/rule-groups/reorder",
            &json!({"names":names}),
        );
    }
    for (from, to) in [
        ("A", "B"),
        ("A", "default"),
        ("default", "C"),
        ("A", ""),
        ("A", " bad "),
        ("A", "a\nb"),
        ("absent", "C"),
    ] {
        refused(
            &proxy.addr,
            "/api/rule-group/rename",
            &json!({"name":from,"to":to}),
        );
    }
    post(
        &proxy.addr,
        "/api/rule-group/rename",
        &json!({"name":"A","to":"A"}),
    );
    post(
        &proxy.addr,
        "/api/rule-groups/reorder",
        &json!({"names":["A","B"]}),
    );
    assert_eq!(snapshot(&proxy.addr), before);
}

#[test]
fn preview_token_also_binds_the_input_and_activation_choice() {
    let dir = scratch("config-migration-input-token");
    let proxy = start(&dir, &[]);
    let mut source = json!({"format":"whistle-rules","data":{"A":"# reviewed","":["A"]}});
    source["expected"] = post(&proxy.addr, "/api/import/preview", &source)["token"].clone();
    source["data"]["A"] = json!("# not reviewed");
    refused(&proxy.addr, "/api/import/apply", &source);
    source["data"]["A"] = json!("# reviewed");
    source["enable_new"] = json!(true);
    refused(&proxy.addr, "/api/import/apply", &source);
    assert_eq!(names(&proxy.addr), ["default"]);
}

#[test]
fn unicode_punctuation_case_and_pathlike_names_do_not_collide_on_disk() {
    let dir = scratch("config-migration-filenames");
    let proxy = start(&dir, &[]);
    let input = json!({"format":"whistle-rules", "data": {
        "中文/甲":"# slash", "中文?甲":"# question", "Case":"# upper", "case":"# lower",
        "../outside":"# path stays a label", "": ["中文/甲","中文?甲","Case","case","../outside"]
    }});
    import(&proxy.addr, input);
    let before = snapshot(&proxy.addr);
    assert!(!dir.join("outside.rules").exists());
    drop(proxy);
    let proxy = restart(&dir);
    assert_eq!(snapshot(&proxy.addr), before);
    for name in ["中文/甲", "中文?甲", "Case", "case", "../outside"] {
        let encoded = whix::rules::replace::encode_uri_component(name);
        let (status, bytes) = get(&proxy.addr, &format!("/api/rule-group?name={encoded}"));
        assert_eq!(status, 200);
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["name"],
            name
        );
    }
}

#[test]
fn a_failed_group_commit_restores_values_and_keeps_runtime_and_restart_consistent() {
    let dir = scratch("config-migration-write-failure");
    let proxy = start(&dir, &[]);
    post(
        &proxy.addr,
        "/api/value",
        &json!({"name":"original","value":"kept"}),
    );
    let before = snapshot(&proxy.addr);
    // A directory at a would-be group file is a deterministic write failure,
    // even for a privileged test runner; no chmod assumptions.
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    digest.update(&7_u64.to_be_bytes());
    digest.update(b"blocked# blocked");
    let hex: String = digest
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    std::fs::create_dir_all(dir.join("rules").join(format!("rule-{hex}.rules"))).unwrap();
    let files_before = common::snapshot(&dir.join("rules"));
    let mut input = json!({"format":"whix","data":{"whix":"test","rules":[{"name":"blocked","text":"# blocked","enabled":true}],"values":{"original":"changed","new":"new"}}});
    input["expected"] = post(&proxy.addr, "/api/import/preview", &input)["token"].clone();
    refused(&proxy.addr, "/api/import/apply", &input);
    assert_eq!(snapshot(&proxy.addr), before);
    assert_eq!(
        common::snapshot(&dir.join("rules")),
        files_before,
        "failed imports leave no unpublished group files"
    );
    drop(proxy);
    assert_eq!(snapshot(&restart(&dir).addr), before);
}

#[test]
fn native_bundle_default_switch_and_source_order_survive_restart() {
    let dir = scratch("config-migration-native");
    let proxy = start(&dir, &[]);
    import(
        &proxy.addr,
        json!({"format":"whix","data":{"whix":"0.1.0","rules":[
        {"name":"default","text":"rank.test statusCode://203","enabled":false},
        {"name":"B","text":"rank.test statusCode://202","enabled":true},
        {"name":"A","text":"rank.test statusCode://201","enabled":true}
    ],"values":{"v":"kept"}}}),
    );
    let before = snapshot(&proxy.addr);
    assert_eq!(before["rules"][0]["enabled"], false);
    assert_eq!(answer(&proxy.addr).0, 202);
    drop(proxy);
    let proxy = restart(&dir);
    assert_eq!(snapshot(&proxy.addr), before);
    assert_eq!(answer(&proxy.addr).0, 202);
}

#[test]
fn full_values_replacements_and_imports_serialize_disk_with_memory() {
    let dir = scratch("config-migration-concurrent-values");
    let proxy = start(&dir, &[]);
    let addr = proxy.addr.clone();
    let replace = std::thread::spawn(move || {
        for i in 0..24 {
            post(&addr, "/api/values", &json!({"replacement":i.to_string()}));
        }
    });
    for i in 0..24 {
        let mut input = json!({"format":"whistle-values","data":{"imported":i.to_string()}});
        input["expected"] = post(&proxy.addr, "/api/import/preview", &input)["token"].clone();
        let (status, body) = request(
            &proxy.addr,
            "POST",
            "/api/import/apply",
            input.to_string().as_bytes(),
        );
        assert!(
            status == 200 || status == 400,
            "{}",
            String::from_utf8_lossy(&body)
        );
        if status == 400 {
            assert!(String::from_utf8_lossy(&body).contains("configuration changed"));
        }
    }
    replace.join().unwrap();
    let before = snapshot(&proxy.addr);
    drop(proxy);
    assert_eq!(snapshot(&restart(&dir).addr), before);
}
