//! Parse a file before touching any live state. Whistle's empty key is ordering
//! metadata, not a Value or rule group; only exact `Default` means the fallback.
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Deserialize)]
pub(super) struct Input {
    pub format: String,
    pub data: Value,
    pub name: Option<String>,
    #[serde(default)]
    pub enable_new: bool,
    pub expected: Option<String>,
}

pub(super) struct Group {
    pub name: String,
    pub text: String,
    pub enabled: Option<bool>,
}

#[derive(Default)]
pub(super) struct Parsed {
    pub groups: Vec<Group>,
    pub values: Vec<(String, String)>,
    pub warnings: Vec<String>,
}

fn name_valid(name: &str) -> Result<(), String> {
    if name.is_empty() || name.trim() != name || name.chars().any(char::is_control) {
        return Err("names must be nonempty, unpadded and contain no control characters".into());
    }
    Ok(())
}

fn text(value: &Value) -> Result<String, String> {
    value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "every imported item must contain text, not an object or array".into())
}

/// JavaScript enumerates canonical array-index keys before other object keys.
/// The explicit list wins; unlisted keys follow JS's order, as getKeys does.
fn keys(map: &serde_json::Map<String, Value>) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    if let Some(meta) = map.get("") {
        let list = meta
            .as_array()
            .or_else(|| meta.get("list").and_then(Value::as_array))
            .ok_or("the empty-key ordering metadata must be an array or {list: [...]} ")?;
        for name in list {
            result.push(
                name.as_str()
                    .ok_or("ordering metadata must contain only names")?
                    .to_string(),
            );
        }
    }
    let mut numeric = Vec::new();
    let mut rest = Vec::new();
    for name in map.keys().filter(|n| !n.is_empty()) {
        match name.parse::<u32>() {
            Ok(n) if n != u32::MAX && n.to_string() == *name => numeric.push((n, name.clone())),
            _ => rest.push(name.clone()),
        }
    }
    numeric.sort_by_key(|(n, _)| *n);
    result.extend(numeric.into_iter().map(|(_, name)| name));
    result.extend(rest);
    let mut seen = HashSet::new();
    result.retain(|name| !name.is_empty() && map.contains_key(name) && seen.insert(name.clone()));
    Ok(result)
}

pub(super) fn parse(input: &Input) -> Result<Parsed, String> {
    let mut out = Parsed::default();
    match input.format.as_str() {
        "whistle-rules" | "whistle-values" => {
            let map = input
                .data
                .as_object()
                .ok_or("expected a Whistle export object")?;
            let rules = input.format == "whistle-rules";
            let mut folders = 0;
            for name in keys(map)? {
                if name.starts_with('\r') {
                    folders += 1;
                    continue;
                }
                name_valid(&name)?;
                let text = text(&map[&name])?;
                if rules {
                    if name == "default" {
                        return Err("Whistle's named file 'default' conflicts with whix's reserved name; rename it in Whistle before exporting".into());
                    }
                    out.groups.push(Group {
                        name: if name == "Default" {
                            "default".into()
                        } else {
                            name
                        },
                        text,
                        enabled: None,
                    });
                } else {
                    out.values.push((name, text));
                }
            }
            if folders > 0 {
                out.warnings.push(format!(
                    "Ignored {folders} folder headings; contained files keep their order."
                ));
            }
            if rules {
                out.warnings.push("Whistle exports omit enabled states and modes (including backRulesFirst). Existing switches are kept; new groups follow the explicit import option. Only normal-order priority can be preserved from this file.".into());
            }
        }
        "whix" => {
            let map = input.data.as_object().ok_or("expected a whix bundle")?;
            if !map.contains_key("whix") {
                return Err("not a whix bundle".into());
            }
            let mut names = HashSet::new();
            if let Some(groups) = map.get("rules") {
                for group in groups.as_array().ok_or("rules must be an array")? {
                    let name = group["name"].as_str().ok_or("group name is required")?;
                    name_valid(name)?;
                    if !names.insert(name) {
                        return Err("duplicate group name in bundle".into());
                    }
                    let enabled = match group.get("enabled") {
                        Some(value) => Some(value.as_bool().ok_or("enabled must be a boolean")?),
                        None => None,
                    };
                    out.groups.push(Group {
                        name: name.into(),
                        text: text(&group["text"])?,
                        enabled,
                    });
                }
            }
            if let Some(values) = map.get("values") {
                for (name, value) in values.as_object().ok_or("values must be an object")? {
                    name_valid(name)?;
                    out.values.push((name.clone(), text(value)?));
                }
            }
        }
        "rules-text" | "value-text" => {
            let name = input
                .name
                .as_deref()
                .ok_or("a filename is required for a plain-text import")?;
            name_valid(name)?;
            let text = text(&input.data)?;
            if input.format == "rules-text" {
                out.groups.push(Group {
                    name: name.into(),
                    text,
                    enabled: None,
                });
            } else {
                out.values.push((name.into(), text));
            }
        }
        _ => return Err("unsupported import format".into()),
    }
    Ok(out)
}
