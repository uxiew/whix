//! Preview a merge, then apply only against the configuration that was reviewed.
//! File contents and credentials stay local; the token is a state fingerprint,
//! not authorization (the console's existing write-access checks still apply).
use super::*;
use crate::rules::storage::try_save_values as write_values;
use crate::rules::{RuleGroup, RuleManager};
use std::collections::{BTreeMap, HashMap, HashSet};
mod format;
use format::{Input, Parsed};

fn token(mgr: &RuleManager, values: &HashMap<String, String>, input: &Input) -> String {
    let groups: Vec<_> = mgr
        .groups()
        .iter()
        .map(|g| (&g.name, g.enabled, &g.text))
        .collect();
    let sorted: BTreeMap<_, _> = values.iter().collect();
    let bytes = serde_json::to_vec(&(
        groups,
        sorted,
        &input.format,
        &input.data,
        &input.name,
        input.enable_new,
    ))
    .unwrap();
    ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

struct Plan {
    groups: Vec<RuleGroup>,
    values: HashMap<String, String>,
    summary: serde_json::Value,
    has_groups: bool,
    has_values: bool,
}

fn plan(
    mgr: &RuleManager,
    values: &HashMap<String, String>,
    input: &Input,
    parsed: Parsed,
) -> Plan {
    let imported: HashSet<&str> = parsed.groups.iter().map(|g| g.name.as_str()).collect();
    let old_named: Vec<_> = mgr
        .groups()
        .iter()
        .filter(|g| g.name != "default")
        .collect();
    // Replace imported groups as one ordered block at their first old position.
    // Unimported groups keep their relative order; preview shows the full change.
    let anchor = old_named
        .iter()
        .position(|g| imported.contains(g.name.as_str()))
        .unwrap_or(old_named.len());
    let mut groups = Vec::new();
    let built: Vec<_> = parsed
        .groups
        .iter()
        .map(|g| {
            let old = mgr.groups().iter().find(|old| old.name == g.name);
            RuleGroup::new(
                &g.name,
                &g.text,
                g.enabled
                    .or_else(|| old.map(|g| g.enabled))
                    .unwrap_or(input.enable_new),
            )
        })
        .collect();
    if let Some(default) = built
        .iter()
        .find(|g| g.name == "default")
        .or_else(|| mgr.groups().iter().find(|g| g.name == "default"))
    {
        groups.push(default.clone());
    }
    for (i, old) in old_named.iter().enumerate() {
        if i == anchor {
            groups.extend(built.iter().filter(|g| g.name != "default").cloned());
        }
        if !imported.contains(old.name.as_str()) {
            groups.push((*old).clone());
        }
    }
    if anchor == old_named.len() {
        groups.extend(built.iter().filter(|g| g.name != "default").cloned());
    }
    let summary = serde_json::json!({
        "ok": true, "token": token(mgr, values, input),
        "groups": built.iter().map(|g| serde_json::json!({"name":g.name,"enabled":g.enabled,"replace":mgr.groups().iter().any(|old|old.name==g.name)})).collect::<Vec<_>>(),
        "values": parsed.values.iter().map(|(name,_)| serde_json::json!({"name":name,"replace":values.contains_key(name)})).collect::<Vec<_>>(),
        "before": mgr.groups().iter().map(|g|&g.name).collect::<Vec<_>>(),
        "after": groups.iter().map(|g|&g.name).collect::<Vec<_>>(),
        "warnings": parsed.warnings,
    });
    let mut merged_values = values.clone();
    for (name, value) in &parsed.values {
        merged_values.insert(name.clone(), value.clone());
    }
    Plan {
        groups,
        values: merged_values,
        summary,
        has_groups: !parsed.groups.is_empty(),
        has_values: !parsed.values.is_empty(),
    }
}

pub(super) async fn migration(
    state: &Arc<AppState>,
    req: Request<Incoming>,
    apply: bool,
) -> Response<DynBody> {
    let raw = match read_json_body(req).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let input: Input = match serde_json::from_value(raw) {
        Ok(input) => input,
        Err(_) => return json_error("expected {format, data, enable_new?, name?, expected?}"),
    };
    let parsed = match format::parse(&input) {
        Ok(parsed) => parsed,
        Err(error) => return json_error(&error),
    };
    let mut mgr = state.rules.write().unwrap();
    let mut values = state.values.write().unwrap();
    let plan = plan(&mgr, &values, &input, parsed);
    if !apply {
        return json_response(&plan.summary);
    }
    if input.expected.as_deref() != plan.summary["token"].as_str() {
        return json_error(
            "configuration changed or preview missing; preview again before applying",
        );
    }
    if state.config.persist_edits {
        let dir = state.config.data_dir();
        if plan.has_values
            && let Err(error) = write_values(dir, &plan.values)
        {
            return json_error(&format!("could not save imported values: {error}"));
        }
        if plan.has_groups
            && let Err(error) =
                crate::rules::storage::try_save_groups(&dir.join("rules"), &plan.groups)
        {
            if plan.has_values
                && let Err(rollback) = write_values(dir, &values)
            {
                return json_error(&format!(
                    "import failed: {error}; restoring values also failed: {rollback}; restore your backup"
                ));
            }
            return json_error(&format!("could not save imported groups: {error}"));
        }
    }
    if plan.has_groups {
        mgr.replace_groups(plan.groups);
    }
    if plan.has_values {
        *values = plan.values;
    }
    drop(values);
    drop(mgr);
    if plan.has_groups {
        fetch_new_includes(state);
    }
    json_response(
        &serde_json::json!({"ok":true, "groups":plan.summary["groups"].as_array().unwrap().len(), "values":plan.summary["values"].as_array().unwrap().len()}),
    )
}

fn json_response(value: &serde_json::Value) -> Response<DynBody> {
    Response::builder()
        .status(StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(body::full(Bytes::from(value.to_string())))
        .unwrap()
}
