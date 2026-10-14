//! Portable PhotoCraft action sets and conservative Photoshop `.atn` migration.
//! Import validates the entire set before changing the library; it never drops events.

use crate::actions_cmds::{Action, ActionsFile};
use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};
use photocraft_psd::descriptor::{Descriptor, Value as DValue};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const IMPORT: &str = "actions.import";
const MAX_ACTIONS: usize = 10_000;
const MAX_STEPS: usize = 10_000;
const MAX_NAME_BYTES: usize = 4096;
const MAX_ERROR_BYTES: usize = 512;
/// File-size limit shared by the engine and interactive picker.
pub const MAX_FILE_BYTES: usize = photocraft_psd::atn::MAX_BYTES;

fn bad(msg: impl Into<String>) -> EngineError {
    let mut msg = msg.into();
    if msg.len() > MAX_ERROR_BYTES {
        let mut end = MAX_ERROR_BYTES - '…'.len_utf8();
        while !msg.is_char_boundary(end) {
            end -= 1;
        }
        msg.truncate(end);
        msg.push('…');
    }
    EngineError::BadParams { cmd: IMPORT.into(), msg }
}

fn always(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}

fn descriptor_keys(d: Option<&Descriptor>, allowed: &[&str]) -> Result<()> {
    if let Some(d) = d {
        let mut seen = BTreeSet::new();
        for (key, _) in &d.items {
            if !allowed.iter().any(|k| key.is(k)) || !seen.insert(key.as_bytes()) {
                let prefix = key.as_bytes().get(..key.as_bytes().len().min(128)).unwrap_or_default();
                return Err(bad(format!("unsupported or duplicate Photoshop parameter `{}`", String::from_utf8_lossy(prefix))));
            }
        }
    }
    Ok(())
}

fn number(d: Option<&Descriptor>, key: &str, default: Option<f64>, low: f64, high: f64, pixels: bool) -> Result<f64> {
    let value = match d.and_then(|d| d.get(key)) {
        Some(DValue::Integer(n)) if !pixels => f64::from(*n),
        Some(DValue::Double(n)) if !pixels => *n,
        Some(DValue::UnitFloat { unit, value }) if pixels && unit == b"#Pxl" => *value,
        None => default.ok_or_else(|| bad(format!("missing Photoshop parameter `{key}`")))?,
        _ => return Err(bad(format!("unsupported value or units for `{key}`"))),
    };
    if !value.is_finite() || !(low..=high).contains(&value) {
        return Err(bad(format!("Photoshop parameter `{key}` is out of range")));
    }
    Ok(value)
}

fn translate(step: &photocraft_psd::atn::AtnItem) -> Result<(String, Value)> {
    if step.enabled && step.with_dialog {
        return Err(bad("interactive Photoshop dialog steps are not supported"));
    }
    let d = step.descriptor.as_ref();
    let plain = match step.event.id().as_str() {
        "invert" | "Invr" => Some("image.adjustments.invert"),
        "desaturate" | "Dstt" => Some("image.adjustments.desaturate"),
        "flattenImage" | "Fltn" | "FltI" => Some("layer.flattenImage"),
        "mergeVisible" | "MrgV" => Some("layer.mergeVisible"),
        "mergeLayersNew" | "Mrg2" => Some("layer.mergeLayers"),
        _ => None,
    };
    if let Some(id) = plain {
        descriptor_keys(d, &[])?;
        return Ok((id.into(), json!({})));
    }
    let (id, params) = match step.event.id().as_str() {
        "gaussianBlur" | "GsnB" => {
            descriptor_keys(d, &["Rds "])?;
            ("filter.blur.gaussianBlur", json!({"radius": number(d, "Rds ", None, 0.1, 1000.0, true)?}))
        }
        "brightnessEvent" | "BrgC" => {
            descriptor_keys(d, &["Brgh", "Cntr", "useLegacy"])?;
            let legacy = match d.and_then(|d| d.get("useLegacy")) {
                Some(DValue::Boolean(b)) => *b,
                None => return Err(bad("brightness/contrast must explicitly specify useLegacy")),
                _ => return Err(bad("useLegacy must be a boolean")),
            };
            let (lo, hi) = if legacy { (-100.0, 100.0) } else { (-50.0, 100.0) };
            (
                "image.adjustments.brightnessContrast",
                json!({
                    "brightness": number(d, "Brgh", Some(0.0), -150.0, 150.0, false)?,
                    "contrast": number(d, "Cntr", Some(0.0), lo, hi, false)?, "legacy": legacy
                }),
            )
        }
        "exposure" | "Exps" => {
            descriptor_keys(d, &["Exps", "Ofst", "gammaCorrection"])?;
            (
                "image.adjustments.exposure",
                json!({
                    "exposure": number(d, "Exps", Some(0.0), -20.0, 20.0, false)?,
                    "offset": number(d, "Ofst", Some(0.0), -0.5, 0.5, false)?,
                    "gamma": number(d, "gammaCorrection", Some(1.0), 0.01, 9.99, false)?
                }),
            )
        }
        "posterization" | "posterize" | "Pstr" => {
            descriptor_keys(d, &["Lvls"])?;
            ("image.adjustments.posterize", json!({"levels": number(d, "Lvls", None, 2.0, 255.0, false)?}))
        }
        "threshold" | "thresholdClassEvent" | "Thrs" => {
            descriptor_keys(d, &["Lvl "])?;
            ("image.adjustments.threshold", json!({"level": number(d, "Lvl ", None, 1.0, 255.0, false)?}))
        }
        _ => return Err(bad(format!("unsupported Photoshop event `{}`", step.event.id()))),
    };
    Ok((id.into(), params))
}

fn validate(actions: &[Action], existing: &[Action]) -> Result<()> {
    if actions.is_empty() || actions.len().saturating_add(existing.len()) > MAX_ACTIONS {
        return Err(bad("an action library must contain 1..10000 actions"));
    }
    let mut names: BTreeSet<&str> = existing.iter().map(|a| a.name.as_str()).collect();
    let mut count = existing.iter().fold(0usize, |count, action| count.saturating_add(action.steps.len()));
    for action in actions {
        if action.set.as_ref().is_some_and(|set| set.len() > MAX_NAME_BYTES) {
            return Err(bad("action set names exceed 4096 UTF-8 bytes"));
        }
        if action.name.len() > MAX_NAME_BYTES {
            return Err(bad("action names exceed 4096 UTF-8 bytes"));
        }
        if action.name.trim().is_empty() || !names.insert(&action.name) {
            return Err(bad(format!("empty or duplicate action name `{}`; rename the existing action before loading", action.name)));
        }
        count = count.saturating_add(action.steps.len());
        if count > MAX_STEPS {
            return Err(bad("action library exceeds 10000 steps"));
        }
        if action.disabled.iter().chain(&action.dialogs).any(|i| *i >= action.steps.len()) {
            return Err(bad(format!("invalid disabled or dialog step in `{}`", action.name)));
        }
        for (id, params) in &action.steps {
            if id.len() > 256 {
                return Err(bad("action command ids exceed 256 bytes"));
            }
            let replayable = crate::actions_cmds::replayable(id)
                || matches!(id.as_str(), crate::atn_cmds::PHOTOSHOP_STEP | crate::atn_cmds::STOP_MESSAGE | "actions.conditional");
            if !replayable || !(params.is_object() || params.is_null()) {
                return Err(bad(format!("unsupported action command or params `{id}`")));
            }
            if id == "actions.conditional"
                && !params.get("if").and_then(Value::as_str).is_some_and(|condition| crate::actions_edit_cmds::CONDITIONS.contains(&condition))
            {
                return Err(bad("conditional action needs a supported `if` condition"));
            }
        }
    }
    Ok(())
}

// Indices in an on-disk set refer to that set, never to the receiving library. Persist
// names so appending the set beside existing actions cannot redirect nested playback.
fn name_calls(actions: &mut [Action]) -> Result<()> {
    let names: Vec<String> = actions.iter().map(|a| a.name.clone()).collect();
    let known: BTreeSet<&str> = names.iter().map(String::as_str).collect();
    for action in actions {
        for (id, params) in &mut action.steps {
            if id == "actions.conditional" {
                for key in ["then", "else"] {
                    match params.get(key) {
                        None | Some(Value::Null) => {}
                        Some(Value::String(name)) if name.trim().is_empty() || known.contains(name.as_str()) => {}
                        _ => return Err(bad("conditional action target must belong to the imported set")),
                    }
                }
                continue;
            }
            if id != "actions.play" {
                continue;
            }
            let target = params.get("action").ok_or_else(|| bad("nested action call is missing its target"))?;
            let name = match target {
                Value::String(name) if known.contains(name.as_str()) => name.clone(),
                Value::Number(n) => {
                    let index = n.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad("nested action index must be a non-negative integer"))?;
                    names.get(index).cloned().ok_or_else(|| bad("nested action index is outside the imported set"))?
                }
                _ => return Err(bad("nested action target must belong to the imported set")),
            };
            let object = params.as_object_mut().ok_or_else(|| bad("nested action params must be an object"))?;
            object.insert("action".into(), json!(name));
        }
    }
    Ok(())
}

// Count the exact portable representation before appending. Separate counting from buffering
// so an oversized library cannot allocate an unbounded export buffer.
struct PortableWriter {
    len: usize,
    bytes: Option<Vec<u8>>,
}

impl std::io::Write for PortableWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let len = self.len.saturating_add(bytes.len());
        if len > MAX_FILE_BYTES {
            return Err(std::io::Error::other("action file exceeds 32 MiB"));
        }
        if let Some(out) = &mut self.bytes {
            out.extend_from_slice(bytes);
        }
        self.len = len;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn portable_file(actions: &[Action], buffer: bool) -> Result<Vec<u8>> {
    #[derive(serde::Serialize)]
    struct File<'a> {
        version: u32,
        actions: &'a [Action],
    }
    let mut writer = PortableWriter { len: 0, bytes: buffer.then(Vec::new) };
    serde_json::to_writer_pretty(&mut writer, &File { version: 1, actions }).map_err(|e| bad(e.to_string()))?;
    Ok(writer.bytes.unwrap_or_default())
}

fn import(s: &mut Session, p: &Value) -> Result<Value> {
    if s.actions.recording.is_some() || s.actions.playing != 0 {
        return Err(bad("stop recording or playback before loading actions"));
    }
    let format = p.get("format").and_then(Value::as_str).ok_or_else(|| bad("format must be `atn` or `json`"))?;
    let data = p.get("data").and_then(Value::as_str).ok_or_else(|| bad("data must be base64 file bytes"))?;
    if data.len() > photocraft_psd::atn::MAX_BYTES / 3 * 4 + 4 {
        return Err(bad("action file exceeds 32 MiB"));
    }
    let bytes = crate::paint::tile::b64_decode(data).ok_or_else(|| bad("invalid base64 action file"))?;
    if bytes.len() > photocraft_psd::atn::MAX_BYTES {
        return Err(bad("action file exceeds 32 MiB"));
    }
    let (mut actions, warnings) = match format {
        "json" => {
            // Serde's data errors can quote an entire hostile string. Report location and
            // category without copying file contents into the status bar.
            let file: ActionsFile = serde_json::from_slice(&bytes)
                .map_err(|e| bad(format!("invalid PhotoCraft action file: {:?} at line {}, column {}", e.classify(), e.line(), e.column())))?;
            if file.version != 1 {
                return Err(bad(format!("unsupported PhotoCraft action version {}", file.version)));
            }
            (file.actions, Vec::<String>::new())
        }
        "atn" => {
            let set = photocraft_psd::atn::parse_bounded(&bytes).map_err(|e| bad(e.to_string()))?;
            let mut actions = Vec::with_capacity(set.actions.len());
            let set_name = set.name.to_string_lossy();
            for a in set.actions {
                let mut action = Action::new(a.name.to_string_lossy(), Vec::with_capacity(a.items.len()));
                if !set_name.trim().is_empty() {
                    action.set = Some(set_name.clone());
                }
                for (i, step) in a.items.iter().enumerate() {
                    let mapped = translate(step).map_err(|e| bad(format!("action `{}`, step {}: {e}", action.name, i + 1)))?;
                    if !step.enabled {
                        action.disabled.insert(i);
                    }
                    if step.with_dialog {
                        action.dialogs.insert(i);
                    }
                    action.steps.push(mapped);
                }
                actions.push(action);
            }
            (actions, vec!["Photoshop function-key bindings, button colours and expansion state are not imported.".into()])
        }
        _ => return Err(bad("format must be `atn` or `json`")),
    };
    validate(&actions, &s.actions.list)?;
    name_calls(&mut actions)?;
    let mut combined = s.actions.list.clone();
    name_calls(&mut combined)?;
    combined.extend(actions.iter().cloned());
    validate(&combined, &[])?;
    portable_file(&combined, false)?;
    let imported: Vec<String> = actions.iter().map(|a| a.name.clone()).collect();
    let index = s.actions.list.len();
    s.actions.list.extend(actions);
    s.actions.touch();
    Ok(json!({"count": imported.len(), "index": index, "imported": imported, "warnings": warnings}))
}

fn export(s: &mut Session, _p: &Value) -> Result<Value> {
    if s.actions.recording.is_some() || s.actions.playing != 0 {
        return Err(EngineError::BadParams { cmd: "actions.export".into(), msg: "stop recording or playback before saving actions".into() });
    }
    let export_error = |error| match error {
        EngineError::BadParams { msg, .. } => EngineError::BadParams { cmd: "actions.export".into(), msg },
        other => other,
    };
    validate(&s.actions.list, &[]).map_err(export_error)?;
    let mut actions = s.actions.list.clone();
    name_calls(&mut actions).map_err(export_error)?;
    let bytes = portable_file(&actions, true).map_err(export_error)?;
    Ok(json!({"filename": "Actions.pcactions", "data": crate::paint::tile::b64_encode(&bytes), "count": actions.len()}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: IMPORT,
            label: "Load Actions…",
            menu: &[],
            shortcut: None,
            params: r#"{"format":"atn|json","data":base64 file bytes} → {count, index, imported, warnings}; rejects unsupported events atomically"#,
            enabled: always,
            run: import,
            journal: false,
        },
        CommandSpec {
            id: "actions.export",
            label: "Save Actions…",
            menu: &[],
            shortcut: None,
            params: "{} → {filename, data:base64 PhotoCraft action set, count}; preserves all actions and disabled steps",
            enabled: always,
            run: export,
            journal: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(file: &ActionsFile) -> Value {
        json!({"format":"json", "data":crate::paint::tile::b64_encode(&serde_json::to_vec(file).unwrap())})
    }

    #[test]
    fn cumulative_serialized_size_is_checked_before_mutating_the_library() {
        let mut s = Session::new();
        s.actions.list.push(Action::new("Existing", vec![("layer.new.layer".into(), json!({"name":"x".repeat(MAX_FILE_BYTES - 4096)}))]));
        assert!(portable_file(&s.actions.list, false).is_ok());
        let before = s.actions.clone();
        let file = ActionsFile { version: 1, actions: vec![Action::new("Incoming", vec![("layer.new.layer".into(), json!({"name":"y".repeat(8192)}))])] };
        let error = s.execute(IMPORT, params(&file)).unwrap_err().to_string();
        assert!(error.contains("32 MiB"), "{error}");
        assert_eq!(s.actions, before);
        assert!(portable_file(&s.actions.list, false).is_ok());
        let mut writer = PortableWriter { len: MAX_FILE_BYTES - 1, bytes: Some(Vec::new()) };
        assert!(std::io::Write::write(&mut writer, &[0, 0]).is_err());
        assert_eq!(writer.len, MAX_FILE_BYTES - 1);
        assert!(writer.bytes.as_ref().unwrap().is_empty());
    }

    #[test]
    fn appending_cannot_make_the_library_exceed_export_limits() {
        let empty = Action::new("Existing", vec![]);
        let incoming = vec![Action::new("Incoming", vec![])];
        assert!(validate(&incoming, &vec![empty.clone(); MAX_ACTIONS - 1]).is_ok());
        assert!(validate(&incoming, &vec![empty; MAX_ACTIONS]).is_err());

        let step = ("image.adjustments.invert".into(), json!({}));
        let mut s = Session::new();
        s.actions.list.push(Action::new("Existing", vec![step.clone(); MAX_STEPS - 1]));
        let file = ActionsFile { version: 1, actions: vec![Action::new("Incoming", vec![step])] };
        s.execute(IMPORT, params(&file)).unwrap();
        assert!(s.execute("actions.export", json!({})).is_ok());
        let before = s.actions.clone();
        let overflow = ActionsFile { version: 1, actions: vec![Action::new("Overflow", vec![("image.adjustments.invert".into(), json!({}))])] };
        assert!(s.execute(IMPORT, params(&overflow)).is_err());
        assert_eq!(s.actions, before);
        assert!(s.execute("actions.export", json!({})).is_ok());
    }

    #[test]
    fn export_errors_identify_the_export_command() {
        let mut s = Session::new();
        assert!(matches!(s.execute("actions.export", json!({})), Err(EngineError::BadParams { cmd, .. }) if cmd == "actions.export"));
        s.actions.list.push(Action::new("Broken", vec![("actions.play".into(), json!({"action": 99}))]));
        assert!(matches!(s.execute("actions.export", json!({})), Err(EngineError::BadParams { cmd, .. }) if cmd == "actions.export"));
    }

    #[test]
    fn hostile_labels_never_expand_ui_error_messages_or_change_the_library() {
        let mut s = Session::new();
        let before = s.actions.clone();
        for action in [Action::new("x".repeat(4097), vec![]), Action::new("Named", vec![("x".repeat(257), json!({}))])] {
            let file = ActionsFile { version: 1, actions: vec![action] };
            let error = s.execute(IMPORT, params(&file)).unwrap_err().to_string();
            assert!(error.len() < 512);
            assert_eq!(s.actions, before);
        }
        let step = photocraft_psd::atn::AtnItem {
            enabled: true,
            with_dialog: false,
            event: photocraft_psd::atn::AtnEvent::Name("Invr".into()),
            descriptor: Some(Descriptor::new("Invr").with(&"x".repeat(8192), DValue::Boolean(true))),
            ..Default::default()
        };
        assert!(translate(&step).unwrap_err().to_string().len() < 512);
    }

    #[test]
    fn portable_sets_preserve_main_atn_metadata_and_kept_photoshop_steps() {
        let bytes = atn(&[("unsupportedEvent", true, None)]);
        let mut s = Session::new();
        s.execute("actions.load", json!({"data": crate::paint::tile::b64_encode(&bytes)})).unwrap();
        s.actions.list[0].dialogs.insert(0);
        s.actions.list.push(Action::new("Conditional", vec![("actions.conditional".into(), json!({"if":"always", "then":"Imported"}))]));
        assert_eq!(s.actions.list[0].set.as_deref(), Some("Studio"));
        assert_eq!(s.actions.list[0].steps[0].0, crate::atn_cmds::PHOTOSHOP_STEP);
        let result = s.execute("actions.export", json!({})).unwrap();
        let mut other = Session::new();
        other.execute(IMPORT, json!({"format":"json", "data":result["data"]})).unwrap();
        assert_eq!(s.actions.list, other.actions.list);
        let before = other.actions.clone();
        let broken =
            ActionsFile { version: 1, actions: vec![Action::new("Broken", vec![("actions.conditional".into(), json!({"if":"always", "then":"Outside"}))])] };
        assert!(other.execute(IMPORT, params(&broken)).is_err());
        assert_eq!(other.actions, before);
    }

    #[test]
    fn portable_sets_roundtrip_disabled_steps_and_nested_actions() {
        let mut s = Session::new();
        let mut child = Action::new("Child", vec![("image.adjustments.invert".into(), json!({}))]);
        child.disabled.insert(0);
        s.actions.list = vec![child, Action::new("Parent", vec![("actions.play".into(), json!({"action":"Child"}))])];
        let result = s.execute("actions.export", json!({})).unwrap();
        let mut other = Session::new();
        other.execute(IMPORT, json!({"format":"json", "data":result["data"]})).unwrap();
        assert_eq!(s.actions.list, other.actions.list);
        other.execute("file.new", json!({"width":2,"height":2})).unwrap();
        let played = other.execute("actions.play", json!({"action":"Parent"})).unwrap();
        assert!(played.get("failed").is_none(), "{played}");
    }

    #[test]
    fn invalid_or_conflicting_sets_leave_state_unchanged() {
        let mut s = Session::new();
        s.actions.list.push(Action::new("Kept", vec![]));
        let before = s.actions.clone();
        for file in [
            ActionsFile { version: 2, actions: vec![Action::new("New", vec![])] },
            ActionsFile { version: 1, actions: vec![Action::new("New", vec![]), Action::new("Kept", vec![])] },
            ActionsFile { version: 1, actions: vec![Action::new("New", vec![("unknown".into(), json!({}))])] },
            ActionsFile { version: 1, actions: vec![Action { name: "New".into(), steps: vec![], disabled: BTreeSet::from([0]), ..Default::default() }] },
        ] {
            assert!(s.execute(IMPORT, params(&file)).is_err());
            assert_eq!(s.actions, before);
        }
        for p in [Value::Null, json!({}), json!({"format":"atn","data":"!"}), json!({"format":"unknown","data":""})] {
            assert!(s.execute(IMPORT, p).is_err());
            assert_eq!(s.actions, before);
        }
        s.execute("actions.record", json!({"name":"Recording"})).unwrap();
        let before = s.actions.clone();
        let file = ActionsFile { version: 1, actions: vec![Action::new("New", vec![])] };
        assert!(s.execute(IMPORT, params(&file)).is_err());
        assert!(s.execute("actions.export", json!({})).is_err());
        assert_eq!(s.actions, before);
    }

    #[test]
    fn malformed_json_and_long_action_names_have_bounded_errors() {
        let mut s = Session::new();
        let name = "🦀".repeat(1000);
        s.actions.list.push(Action::new(name.clone(), vec![]));
        let before = s.actions.clone();
        let malformed = serde_json::to_vec(&json!({"version":"x".repeat(100_000),"actions":[]})).unwrap();
        let error = s.execute(IMPORT, json!({"format":"json","data":crate::paint::tile::b64_encode(&malformed)})).unwrap_err();
        assert!(error.to_string().contains("Data at line"));
        assert!(error.to_string().len() < 1024);
        assert!(!error.to_string().contains(&"x".repeat(100)));
        assert_eq!(s.actions, before);

        let duplicate = ActionsFile { version: 1, actions: vec![Action::new(name, vec![])] };
        let error = s.execute(IMPORT, params(&duplicate)).unwrap_err();
        assert!(matches!(error, EngineError::BadParams { ref msg, .. } if msg.len() <= MAX_ERROR_BYTES && msg.ends_with('…')));
        assert_eq!(s.actions, before);

        s.actions.list[0].disabled.insert(0);
        assert!(
            matches!(s.execute("actions.export", json!({})), Err(EngineError::BadParams { cmd, msg }) if cmd == "actions.export" && msg.len() <= MAX_ERROR_BYTES)
        );
    }

    #[test]
    fn invalid_conditional_conditions_fail_before_changing_the_library() {
        let mut s = Session::new();
        s.actions.list.push(Action::new("Kept", vec![]));
        let before = s.actions.clone();
        for condition in [json!({}), json!({"if":null}), json!({"if":true}), json!({"if":"unknown"})] {
            let file = ActionsFile { version: 1, actions: vec![Action::new("Broken", vec![("actions.conditional".into(), condition.clone())])] };
            let error = s.execute(IMPORT, params(&file)).unwrap_err().to_string();
            assert!(error.contains("supported `if` condition"), "{error}");
            assert_eq!(s.actions, before);

            let mut invalid = Session::new();
            invalid.actions.list = file.actions;
            assert!(matches!(invalid.execute("actions.export", json!({})), Err(EngineError::BadParams { cmd, .. }) if cmd == "actions.export"));
        }
        let valid = ActionsFile { version: 1, actions: vec![Action::new("Conditional", vec![("actions.conditional".into(), json!({"if":"always"}))])] };
        s.execute(IMPORT, params(&valid)).unwrap();
        assert_eq!(s.execute("actions.play", json!({"action":"Conditional"})).unwrap()["ran"], 1);
        assert!(s.execute("actions.export", json!({})).is_ok());
    }

    #[test]
    fn threshold_event_aliases_import_and_replay() {
        for event in ["threshold", "thresholdClassEvent", "Thrs"] {
            let descriptor = Descriptor::new("Thrs").with("Lvl ", DValue::Integer(100));
            let mut file = photocraft_psd::atn::parse(&atn(&[(event, true, Some(descriptor))])).unwrap();
            if event == "Thrs" {
                file.actions[0].items[0].event = photocraft_psd::atn::AtnEvent::Code(*b"Thrs");
            }
            let bytes = photocraft_psd::atn::write(&file).unwrap();
            let mut s = Session::new();
            s.execute("file.new", json!({"width":2,"height":2,"background":"black"})).unwrap();
            s.execute(IMPORT, json!({"format":"atn","data":crate::paint::tile::b64_encode(&bytes)})).unwrap();
            assert_eq!(s.actions.list[0].steps[0], ("image.adjustments.threshold".into(), json!({"level":100.0})));
            let played = s.execute("actions.play", json!({"action":"Imported"})).unwrap();
            assert_eq!(played["ran"], 1, "{played}");
            assert!(played.get("failed").is_none(), "{played}");
        }
    }

    #[test]
    fn maps_photoshop_units_and_rejects_unknown_fields_and_dialogs() {
        let mut step = photocraft_psd::atn::AtnItem {
            enabled: true,
            with_dialog: false,
            event: photocraft_psd::atn::AtnEvent::Name("GsnB".into()),
            descriptor: Some(Descriptor::new("GsnB").with("Rds ", DValue::UnitFloat { unit: *b"#Pxl", value: 2.5 })),
            ..Default::default()
        };
        assert_eq!(translate(&step).unwrap(), ("filter.blur.gaussianBlur".into(), json!({"radius":2.5})));
        step.descriptor = Some(Descriptor::new("GsnB").with("Rds ", DValue::UnitFloat { unit: *b"#Prc", value: 2.5 }));
        assert!(translate(&step).is_err());
        step.event = photocraft_psd::atn::AtnEvent::Name("Invr".into());
        step.descriptor = Some(Descriptor::new("Invr").with("unknown", DValue::Boolean(true)));
        assert!(translate(&step).is_err());
        step.descriptor = None;
        step.with_dialog = true;
        assert!(translate(&step).is_err());
        step.with_dialog = false;
        step.event = photocraft_psd::atn::AtnEvent::Name("select".into());
        assert!(translate(&step).is_err());
        step.event = photocraft_psd::atn::AtnEvent::Name("BrgC".into());
        step.descriptor = Some(Descriptor::new("BrgC").with("Cntr", DValue::Integer(25)).with("useLegacy", DValue::Boolean(false)));
        assert_eq!(translate(&step).unwrap().1, json!({"brightness":0.0,"contrast":25.0,"legacy":false}));
        step.descriptor = Some(Descriptor::new("BrgC").with("Cntr", DValue::Integer(25)));
        assert!(translate(&step).is_err(), "never guess a brightness algorithm");
        step.event = photocraft_psd::atn::AtnEvent::Name("exposure".into());
        step.descriptor = Some(Descriptor::new("exposure").with("Exps", DValue::Double(1.0)).with("gammaCorrection", DValue::Double(1.2)));
        assert_eq!(translate(&step).unwrap().1, json!({"exposure":1.0,"offset":0.0,"gamma":1.2}));
        step.descriptor = Some(Descriptor::new("exposure").with("gammaCorrection", DValue::Double(f64::NAN)));
        assert!(translate(&step).is_err());
    }

    #[test]
    fn exposure_event_aliases_import_and_replay_at_all_depths() {
        let expected = json!({"exposure":1.0,"offset":0.1,"gamma":1.2});
        for event in ["exposure", "Exps"] {
            let descriptor =
                Descriptor::new("Exps").with("Exps", DValue::Double(1.0)).with("Ofst", DValue::Double(0.1)).with("gammaCorrection", DValue::Double(1.2));
            let mut file = photocraft_psd::atn::parse(&atn(&[(event, true, Some(descriptor))])).unwrap();
            if event == "Exps" {
                file.actions[0].items[0].event = photocraft_psd::atn::AtnEvent::Code(*b"Exps");
            }
            let bytes = photocraft_psd::atn::write(&file).unwrap();
            for depth in [8, 16, 32] {
                let mut s = Session::new();
                s.execute("file.new", json!({"width":2,"height":2,"depth":depth,"background":"black"})).unwrap();
                let before = s.active().unwrap().doc.clone();
                s.execute("image.adjustments.exposure", expected.clone()).unwrap();
                let pixel = s.active().unwrap().doc.layers[0].surface().unwrap().pixel(0, 0);
                s.execute("edit.undo", json!({})).unwrap();
                s.execute(IMPORT, json!({"format":"atn","data":crate::paint::tile::b64_encode(&bytes)})).unwrap();
                assert_eq!(s.actions.list[0].steps[0], ("image.adjustments.exposure".into(), expected.clone()));
                let played = s.execute("actions.play", json!({"action":"Imported"})).unwrap();
                assert_eq!(played["ran"], 1, "{played}");
                assert!(played.get("failed").is_none(), "{played}");
                assert_eq!(s.active().unwrap().doc.layers[0].surface().unwrap().pixel(0, 0), pixel);
                s.execute("edit.undo", json!({})).unwrap();
                assert_eq!(s.active().unwrap().doc, before);
            }
        }
    }

    #[test]
    fn local_nested_indices_cannot_target_existing_actions() {
        let file = ActionsFile {
            version: 1,
            actions: vec![
                Action::new("Child", vec![("layer.new.layer".into(), json!({"name":"Imported"}))]),
                Action::new("Parent", vec![("actions.play".into(), json!({"action":0}))]),
            ],
        };
        let mut s = Session::new();
        s.actions.list.push(Action::new("Existing", vec![("layer.new.layer".into(), json!({"name":"Wrong"}))]));
        s.execute(IMPORT, params(&file)).unwrap();
        assert_eq!(s.actions.list[2].steps[0].1["action"], "Child");
        s.execute("file.new", json!({"width":2,"height":2})).unwrap();
        let played = s.execute("actions.play", json!({"action":"Parent"})).unwrap();
        assert!(played.get("failed").is_none(), "{played}");
        assert_eq!(s.active().unwrap().doc.layers.last().unwrap().name, "Imported");
        let before = s.actions.clone();
        let invalid = ActionsFile { version: 1, actions: vec![Action::new("Broken", vec![("actions.play".into(), json!({"action":100}))])] };
        assert!(s.execute(IMPORT, params(&invalid)).is_err());
        assert_eq!(s.actions, before);
    }

    fn atn(events: &[(&str, bool, Option<Descriptor>)]) -> Vec<u8> {
        let mut out = 16u32.to_be_bytes().to_vec();
        let unicode = |out: &mut Vec<u8>, name: &str| {
            let units: Vec<u16> = name.encode_utf16().collect();
            out.extend_from_slice(&(units.len() as u32).to_be_bytes());
            for u in units {
                out.extend_from_slice(&u.to_be_bytes());
            }
        };
        unicode(&mut out, "Studio");
        out.push(0);
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(&[0; 6]);
        unicode(&mut out, "Imported");
        out.push(0);
        out.extend_from_slice(&(events.len() as u32).to_be_bytes());
        for (event, enabled, descriptor) in events {
            out.extend_from_slice(&[0, u8::from(*enabled), 0, 2]);
            out.extend_from_slice(b"TEXT");
            out.extend_from_slice(&(event.len() as u32).to_be_bytes());
            out.extend_from_slice(event.as_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&if descriptor.is_some() { u32::MAX } else { 0 }.to_be_bytes());
            if let Some(d) = descriptor {
                out.extend_from_slice(&d.to_bytes());
            }
        }
        out
    }

    #[test]
    fn atn_import_replays_with_undo_at_all_depths_and_is_atomic_on_unsupported_events() {
        let bytes = atn(&[("invert", true, None), ("invert", false, None)]);
        for depth in [8, 16, 32] {
            let mut s = Session::new();
            s.execute("file.new", json!({"width":2,"height":2,"depth":depth,"background":"black"})).unwrap();
            let before = s.active().unwrap().doc.clone();
            let r = s.execute(IMPORT, json!({"format":"atn","data":crate::paint::tile::b64_encode(&bytes)})).unwrap();
            assert_eq!(r["count"], 1);
            assert_eq!(s.actions.list[0].disabled, BTreeSet::from([1]));
            let played = s.execute("actions.play", json!({"action":"Imported"})).unwrap();
            assert_eq!(played["ran"], 1, "{played}");
            assert!(played.get("failed").is_none(), "{played}");
            assert_eq!(s.active().unwrap().doc.layers[0].surface().unwrap().pixel(0, 0), vec![1.0, 1.0, 1.0, 1.0]);
            s.execute("edit.undo", json!({})).unwrap();
            assert_eq!(s.active().unwrap().doc, before);
        }
        let mut s = Session::new();
        let before = s.actions.clone();
        let broken = atn(&[("invert", true, None), ("unsupportedEvent", false, None)]);
        let error = s.execute(IMPORT, json!({"format":"atn","data":crate::paint::tile::b64_encode(&broken)})).unwrap_err().to_string();
        assert!(error.contains("step 2"), "{error}");
        assert_eq!(s.actions, before);
    }
}
