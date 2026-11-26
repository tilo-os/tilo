//! How a program of the person's (`tilo-install`, `app`) changes what this computer runs. A
//! runtime, an image, a collection or a check is the cache's published action (`packages#<id>`);
//! an app installed, updated, kept or removed is the action of the workspace's tree
//! (`workspace/apps…`), whose every device then follows it. Both are asked of the daemon over
//! its native endpoint, each file a change names put first. So a change from a terminal is the
//! same action, admitted the same way, as one from an agent or another device.
//!
//! The cache's actions go over the daemon's action wire, which admits this computer's own
//! account as the person themselves: installing a development build is their own decision, and
//! only their own connection is let in ([`crate::dev_build_admitted`]). A file it names is put
//! for that action, so the bytes stay on this device, in the store the action reads them from.

use std::path::Path;

use app_cli::device::{
    CHANGE_WAIT_MS, Daemon, Wire, install_file, line, line_at, on_app, update_offered, waited,
};
use serde_json::{Map, Value, json};
use tilo_action_services::device_tickets;
use tilo_actions::remote::{ActionProvider as _, CatalogueQuery, RemoteActionTarget};

use crate::{Change, Report};

/// The device action that names the id a development build of an app runs under on this computer
/// (`devices#dev-id`): asked of the device, as installing the build is.
const DEV_ID: &str = "dev-id";

/// How long a change made through the daemon may take to run.
const CHANGE_WAIT: std::time::Duration = std::time::Duration::from_millis(CHANGE_WAIT_MS);

/// Make `change` through this computer's daemon, putting each local file it names first.
///
/// # Errors
/// The daemon could not be reached, a file it names could not be read or put, the change is no
/// action of the cache's, or the cache refused or failed it (what it said before then included).
pub fn change(change: &Change) -> Result<Report, String> {
    let daemon = Connection::reach()?;
    ask(&daemon, change)
}

/// [`change`] through the daemon answering on `socket` instead of this account's own: the
/// device whose profile names the socket.
///
/// # Errors
/// As [`change`].
pub fn change_at(socket: &Path, change: &Change) -> Result<Report, String> {
    let daemon = Connection::at(socket)?;
    ask(&daemon, change)
}

/// The id `app`'s development build runs under on this computer: the daemon derives it from the
/// device key it keeps (`devices#dev-id`), and a build is compiled for it before the device
/// installs it ([`change`] with [`Change::InstallDev`]).
///
/// The read is asked over the action wire, the connection an install of the build is made on:
/// the daemon admits that connection as the person themselves. Its MCP endpoint admits a
/// connection as the person's MCP client agent, which a device action only the person may take
/// is refused to, so the read is never asked there.
///
/// # Errors
/// The daemon could not be reached, refused the read, or named no id.
pub fn dev_id(app: &str) -> Result<String, String> {
    Connection::reach()?.dev_id(app)
}

/// One connection to the daemon over its action wire, as this computer's own account, asking the
/// device's package cache.
struct Connection(Wire);

impl Connection {
    /// The daemon serving this computer ([`line`] at its own socket).
    fn reach() -> Result<Self, String> {
        Ok(Self(Wire::of(line()?)))
    }

    /// The daemon serving `socket` ([`line_at`]).
    fn at(socket: &Path) -> Result<Self, String> {
        Ok(Self(Wire::of(line_at(socket)?)))
    }

    /// The action `id` of the device's package cache, as the daemon publishes it now: the one
    /// provider of this computer's whose address names it.
    fn target(&self, id: &str) -> Result<RemoteActionTarget, String> {
        let query = CatalogueQuery {
            provider: Some(crate::PROVIDER.to_owned()),
            action: Some(id.to_owned()),
            max_results: 1,
            ..CatalogueQuery::default()
        };
        let page = waited(self.0.line().query(self.0.caller(), query), CHANGE_WAIT)?;
        page.rows
            .iter()
            .find_map(|row| {
                let action = row.action.as_ref()?;
                (action.address.action == id).then(|| RemoteActionTarget {
                    provider: row.provider.clone(),
                    address: action.address.clone(),
                })
            })
            .ok_or_else(|| format!("this computer's package cache publishes no {id}"))
    }

    /// Make the change `target` names with `arguments`, to its end.
    fn invoke(
        &self,
        target: &RemoteActionTarget,
        arguments: Map<String, Value>,
    ) -> Result<Report, String> {
        let answer = self.0.invoke(target, arguments)?;
        Ok(report_in(&answer).unwrap_or_else(|| Report {
            lines: vec![answer.to_string()],
            warnings: Vec::new(),
            installed: None,
        }))
    }

    /// The id `app`'s development build runs under on this computer, as its daemon derives it
    /// from the key it keeps (`devices#dev-id`).
    fn dev_id(&self, app: &str) -> Result<String, String> {
        let mut arguments = Map::new();
        arguments.insert("app".to_owned(), Value::String(app.to_owned()));
        let answer = self.0.invoke(&device_tickets::target(DEV_ID), arguments)?;
        answer
            .get("app")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                format!("the device named no id {app}'s development build runs under: {answer}")
            })
    }

    /// Put the file at `path` into the store the action `target` names reads from: its
    /// reference there.
    fn put(&self, path: &Path, target: &RemoteActionTarget) -> Result<String, String> {
        self.0.put(path, target)
    }
}

/// Make `change` through `daemon`, putting each local file it names first.
fn ask(daemon: &Connection, change: &Change) -> Result<Report, String> {
    let mut asked = serde_json::to_value(change).map_err(|error| error.to_string())?;
    let (action, _) = action_of(asked.clone())?;
    let target = daemon.target(action)?;
    put_files(daemon, &target, &mut asked)?;
    let (action, arguments) = action_of(asked)?;
    if action != target.address.action {
        return Err(format!("{action} is no action this change asked for"));
    }
    daemon.invoke(&target, arguments)
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
        installed: None,
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

/// Put every local file `value` names (`{"source": "<path>"}`) into the store the action
/// `target` reads, and name it by its content instead.
fn put_files(
    daemon: &Connection,
    target: &RemoteActionTarget,
    value: &mut Value,
) -> Result<(), String> {
    match value {
        Value::Object(fields) => {
            if let Some(Value::String(source)) = fields.get_mut("source")
                && !source.starts_with("tilo.content:")
            {
                *source = daemon.put(Path::new(source.as_str()), target)?;
            }
            fields
                .values_mut()
                .try_for_each(|field| put_files(daemon, target, field))
        }
        Value::Array(items) => items
            .iter_mut()
            .try_for_each(|item| put_files(daemon, target, item)),
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
