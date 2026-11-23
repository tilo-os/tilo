//! How a program of the person's (`tilo-install`, `app`) changes what this computer runs. A
//! runtime, an image, a collection or a check is the cache's published action (`packages#<id>`);
//! an app installed, updated, kept or removed is the action of the workspace's tree
//! (`workspace/apps…`), whose every device then follows it. Both are asked of the daemon over
//! its native endpoint, each file a change names put first. So a change from a terminal is the
//! same action, admitted the same way, as one from an agent or another device.

use std::path::Path;

use app_cli::device::{CHANGE_WAIT_MS, Daemon, install_file, on_app, update_offered};
use serde_json::{Map, Value, json};

use crate::{Change, Report};

/// Make `change` through this computer's daemon, putting each local file it names first.
///
/// # Errors
/// The daemon could not be reached, a file it names could not be read or put, the change is no
/// action of the cache's, or the cache refused or failed it (what it said before then included).
pub fn change(change: &Change) -> Result<Report, String> {
    let mut daemon = Daemon::reach()?;
    let mut asked = serde_json::to_value(change).map_err(|error| error.to_string())?;
    put_files(&mut daemon, &mut asked)?;
    let (action, arguments) = action_of(asked)?;
    let answer = daemon.call(
        "tilo_do",
        &json!({"path": format!("packages#{action}"), "wait_ms": CHANGE_WAIT_MS, "args": arguments}),
    )?;
    Ok(report_in(&answer).unwrap_or_else(|| Report {
        lines: vec![answer.to_string()],
        warnings: Vec::new(),
    }))
}

/// Make `asked` on this computer: an app's install, update, pin or removal is the action of
/// the workspace's tree, which every device of the workspace follows; whose packages it trusts,
/// and where it fetches from, are its own resources there too; the rest is the device cache's
/// action.
///
/// # Errors
/// The change was refused or failed, or the daemon could not be reached.
pub fn make(asked: Change) -> Result<Report, String> {
    let said = |answer: serde_json::Value| Report {
        lines: vec![answer.to_string()],
        warnings: Vec::new(),
    };
    let not_offline = |what: &str| {
        Err(format!(
            "a dry run of {what} is a change to a store this process writes (`--offline-store DIR`)"
        ))
    };
    match asked {
        Change::Install { dry_run: true, .. } => not_offline("an install"),
        Change::Install {
            file,
            trust,
            pinned,
            ..
        } => install_file(Path::new(&file.source), trust.allow_self_signed, pinned).map(said),
        Change::Update { dry_run: true, .. } => not_offline("an update"),
        Change::Update {
            file: Some(file),
            trust,
            ..
        } => install_file(Path::new(&file.source), trust.allow_self_signed, false).map(said),
        Change::Update {
            app: Some(app),
            version,
            ..
        } => update_offered(&app, version.as_deref()).map(said),
        Change::Update { app: None, .. } => {
            Err("name the app to update: a workspace updates its apps one by one".to_owned())
        }
        Change::Uninstall { dry_run: true, .. } => not_offline("a removal"),
        Change::Uninstall { app, .. } => on_app(&app, "uninstall").map(said),
        Change::SetPinned { app, pinned } => {
            on_app(&app, if pinned { "pin" } else { "unpin" }).map(said)
        }
        Change::AddAttestor { id, pubkey, scope } => Daemon::reach()?
            .workspace(
                "workspace/trust#add",
                &json!({"id": id, "pubkey": pubkey, "scope": scope}),
            )
            .map(said),
        Change::RemoveAttestor { id } => Daemon::reach()?
            .workspace("workspace/trust#remove", &json!({"id": id}))
            .map(said),
        Change::Revoke { pubkey, reason } => Daemon::reach()?
            .workspace(
                "workspace/trust#revoke",
                &json!({"pubkey": pubkey, "reason": reason}),
            )
            .map(said),
        other => change(&other),
    }
}

/// The cache's action `asked` (a [`Change`] as JSON) is, and its arguments.
fn action_of(asked: Value) -> Result<(&'static str, Map<String, Value>), String> {
    let Value::Object(tagged) = asked else {
        return Err(format!("{asked} is no change the cache's actions make"));
    };
    let Some((name, Value::Object(mut arguments))) = tagged.into_iter().next() else {
        return Err("a change names its arguments".to_owned());
    };
    let published = crate::published().iter().find(|published| {
        published.change == name
            && published
                .fixed
                .is_none_or(|(field, value)| arguments.get(field) == Some(&Value::Bool(value)))
    });
    let Some(published) = published else {
        return Err(format!(
            "{name} is no change a program asks this computer's packages for"
        ));
    };
    if let Some((field, _)) = published.fixed {
        arguments.remove(field);
    }
    Ok((published.id, arguments))
}

/// Put every local file `value` names (`{"source": "<path>"}`) and name it by its content
/// instead.
fn put_files(daemon: &mut Daemon, value: &mut Value) -> Result<(), String> {
    match value {
        Value::Object(fields) => {
            if let Some(Value::String(source)) = fields.get_mut("source")
                && !source.starts_with("tilo.content:")
            {
                *source = daemon.put(Path::new(source.as_str()))?;
            }
            fields
                .values_mut()
                .try_for_each(|field| put_files(daemon, field))
        }
        Value::Array(items) => items
            .iter_mut()
            .try_for_each(|item| put_files(daemon, item)),
        _ => Ok(()),
    }
}

/// The report a cache's answer carries.
fn report_in(answer: &Value) -> Option<Report> {
    match answer {
        Value::Object(fields) => serde_json::from_value(answer.clone())
            .ok()
            .filter(|_| fields.contains_key("lines"))
            .or_else(|| fields.values().find_map(report_in)),
        Value::Array(items) => items.iter().find_map(report_in),
        Value::String(text) => serde_json::from_str::<Value>(text)
            .ok()
            .filter(Value::is_object)
            .and_then(|value| report_in(&value)),
        _ => None,
    }
}
