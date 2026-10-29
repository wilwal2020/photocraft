use super::convert::{Imported, import_any};
use super::convert::{UniqueNames, export};
use super::{Action, DEFAULT_SET, EngineError, Result, Session, Value, bad, json};

fn unique(name: &str, taken: &[String]) -> String {
    if !taken.iter().any(|t| t == name) {
        return name.to_string();
    }
    (2..).map(|i| format!("{name} {i}")).find(|n| !taken.contains(n)).unwrap_or_else(|| name.to_string())
}

pub(super) fn load(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "actions.load";
    if s.actions.recording.is_some() || s.actions.playing > 0 {
        return Err(bad(cmd, "stop recording or playing before loading actions"));
    }
    let (bytes, _) = crate::preset_import_cmds::file_bytes(p, cmd, ".atn file or Actions palette")?;
    let mut sets = import_any(&bytes).map_err(|e| bad(cmd, e))?;
    match p.get("set") {
        None | Some(Value::Null) => {}
        Some(Value::String(n)) if !n.trim().is_empty() => {
            let [imp] = sets.as_mut_slice() else {
                return Err(bad(cmd, "`set` renames a single set; an Actions palette keeps its sets' names"));
            };
            imp.set = n.trim().to_string();
            for a in &mut imp.actions {
                a.set = Some(imp.set.clone());
            }
            for u in &mut imp.unsupported {
                u.set = imp.set.clone();
            }
        }
        Some(_) => return Err(bad(cmd, "`set` must be a non-empty string")),
    }
    let replace = match p.get("replace") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(bad(cmd, "`replace` must be true or false")),
    };
    let palette = sets.len() > 1;
    let mut loaded: Vec<Value> = Vec::with_capacity(sets.len());
    let mut seen: Vec<(String, Vec<Action>)> = Vec::new();
    for mut imp in sets {
        // Photoshop's panel can hold the same set twice (an installer run four times): an exact
        // copy loads once, a different set of the same name gets a numbered name.
        if seen.iter().any(|(n, a)| *n == imp.set && *a == imp.actions) {
            continue;
        }
        if seen.iter().any(|(n, _)| *n == imp.set) {
            let taken: Vec<String> = seen.iter().map(|(n, _)| n.clone()).collect();
            imp.set = unique(&imp.set, &taken);
            for a in &mut imp.actions {
                a.set = Some(imp.set.clone());
            }
            for u in &mut imp.unsupported {
                u.set = imp.set.clone();
            }
        }
        seen.push((imp.set.clone(), imp.actions.clone()));
        loaded.push(load_set(s, imp, replace));
    }
    s.actions.touch();
    if !palette && let Some(one) = loaded.pop() {
        return Ok(one);
    }
    // A palette: every set's report, and the totals in the single-set fields.
    let all = |k: &str| loaded.iter().flat_map(|v| v[k].as_array().cloned().unwrap_or_default()).collect::<Vec<_>>();
    let steps: u64 = loaded.iter().filter_map(|v| v["steps"].as_u64()).sum();
    Ok(json!({
        "sets": loaded.iter().map(|v| v["set"].clone()).collect::<Vec<_>>(),
        "set": loaded.first().map_or(Value::Null, |v| v["set"].clone()),
        "actions": all("actions"),
        "steps": steps,
        "unsupported": all("unsupported"),
        "functionKeys": all("functionKeys"),
        "warnings": all("warnings"),
        "bySet": loaded,
    }))
}

/// Add one imported set to the Actions panel (replacing the actions of a set of the same name
/// when `replace`), and report it as `actions.load` does.
fn load_set(s: &mut Session, mut imp: Imported, replace: bool) -> Value {
    if replace {
        s.actions.list.retain(|a| a.set.as_deref() != Some(imp.set.as_str()));
    }
    let mut taken = UniqueNames::new(s.actions.list.iter().map(|a| a.name.clone()));
    let mut names = Vec::with_capacity(imp.actions.len());
    // The file's name of each action that had to be renamed, and its new name.
    let mut renamed: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut steps = 0usize;
    let mut added = Vec::with_capacity(imp.actions.len());
    for mut a in imp.actions {
        let name = taken.claim(&a.name);
        if name != a.name {
            renamed.entry(a.name.clone()).or_insert_with(|| name.clone());
        }
        a.name = name.clone();
        steps = steps.saturating_add(a.steps.len());
        names.push(name);
        added.push(a);
    }
    // Play and Conditional steps call actions by name: follow the renames in the actions just
    // loaded, each target looked up once by the name the file gave it.
    for a in &mut added {
        for (id, params) in &mut a.steps {
            let keys: &[&str] = match id.as_str() {
                "actions.play" => &["action"],
                "actions.conditional" => &["then", "else"],
                _ => &[],
            };
            for key in keys {
                if let Some(to) = params.get(*key).and_then(Value::as_str).and_then(|n| renamed.get(n)) {
                    params[*key] = json!(to);
                }
            }
        }
    }
    s.actions.list.extend(added);
    let keys: Vec<(String, String)> = imp.function_keys.into_iter().map(|(n, k)| (renamed.get(&n).cloned().unwrap_or(n), k)).collect();
    if !keys.is_empty() {
        s.edit_prefs(|prefs| {
            for (name, key) in &keys {
                // A function key belongs to one action.
                prefs.shortcuts.retain(|k, v| !(k.starts_with(crate::actions_cmds::PLAY_SHORTCUT_PREFIX) && v == key));
                prefs.shortcuts.insert(format!("{}{name}", crate::actions_cmds::PLAY_SHORTCUT_PREFIX), key.clone());
            }
        });
    }
    // Unsupported steps name the action as loaded (a rename on a clash included).
    for u in &mut imp.unsupported {
        if let Some(to) = renamed.get(&u.action) {
            u.action = to.clone();
        }
    }
    json!({
        "set": imp.set,
        "actions": names,
        "steps": steps,
        "unsupported": imp.unsupported,
        "functionKeys": keys.iter().map(|(n, k)| json!({"action": n, "key": k})).collect::<Vec<_>>(),
        "warnings": imp.warnings,
    })
}

pub(super) fn save(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "actions.save";
    crate::actions_cmds::flush_recording(s)?;
    let skip = match p.get("skipUnsupported") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(bad(cmd, "`skipUnsupported` must be true or false")),
    };
    let set_param = match p.get("set") {
        None | Some(Value::Null) => None,
        Some(Value::String(n)) if !n.trim().is_empty() => Some(n.trim().to_string()),
        Some(_) => return Err(bad(cmd, "`set` must be a non-empty string")),
    };
    let (set, actions): (String, Vec<&Action>) = if p.get("action").is_some() {
        let idx = crate::actions_cmds::resolve(&s.actions.list, p, cmd)?;
        let a = &s.actions.list[idx];
        (set_param.or_else(|| a.set.clone()).unwrap_or_else(|| a.name.clone()), vec![a])
    } else if let Some(set) = set_param {
        let list: Vec<&Action> = s.actions.list.iter().filter(|a| a.set.as_deref() == Some(set.as_str())).collect();
        if list.is_empty() {
            return Err(bad(cmd, format!("no actions in a set named `{set}`")));
        }
        (set, list)
    } else {
        (DEFAULT_SET.to_string(), s.actions.list.iter().collect())
    };
    if actions.is_empty() {
        return Err(bad(cmd, "there are no actions to save"));
    }
    let names: Vec<String> = actions.iter().map(|a| a.name.clone()).collect();
    let keys: Vec<(String, String)> =
        s.prefs().shortcuts.iter().filter_map(|(k, v)| k.strip_prefix(crate::actions_cmds::PLAY_SHORTCUT_PREFIX).map(|n| (n.to_string(), v.clone()))).collect();
    let (bytes, skipped) = export(&set, &actions, &keys, skip).map_err(|e| bad(cmd, e))?;
    let steps: usize = actions.iter().map(|a| a.steps.len()).sum::<usize>().saturating_sub(skipped.len());
    let mut out = json!({"set": set, "actions": names, "steps": steps, "bytes": bytes.len(), "skipped": skipped});
    match p.get("path") {
        None | Some(Value::Null) => {
            out["data"] = json!(photocraft_paint::tile::b64_encode(&bytes));
        }
        Some(Value::String(path)) if !path.trim().is_empty() => {
            write_file(path, &bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
            out["path"] = json!(path);
        }
        Some(_) => return Err(bad(cmd, "`path` must be a non-empty string")),
    }
    Ok(out)
}

#[cfg(not(target_arch = "wasm32"))]
fn write_file(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    // Atomic replacement (temporary file + rename): a failed write keeps the previous ATN (#7290).
    photocraft_format::atomic_write(std::path::Path::new(path), bytes)
}

#[cfg(target_arch = "wasm32")]
fn write_file(path: &str, _bytes: &[u8]) -> std::io::Result<()> {
    Err(std::io::Error::other(format!("cannot write paths on the web ({path}); omit `path` to get the bytes back as `data`")))
}
