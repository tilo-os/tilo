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

use std::io::Read as _;
use std::path::Path;
use std::time::{Duration, Instant};

use app_cli::device::{
    CHANGE_WAIT_MS, Daemon, install_file, line, line_at, on_app, update_offered,
};
use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, CatalogueQuery, ContentPut as _, InvokeArgument,
    InvokeRequest, LineClient, MAX_REMOTE_WAIT, Pending, ReceiptStatus, RemoteActionTarget,
};
use tilo_actions::{InvocationId, InvocationUpdate};

use crate::{Change, Report};

/// How long a change made through the daemon may take to run.
const CHANGE_WAIT: Duration = Duration::from_millis(CHANGE_WAIT_MS);

/// The bytes of a package file one piece of an upload carries.
const PUT_PIECE: usize = 512 * 1024;

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

/// One connection to the daemon over its action wire, as this computer's own account, and the
/// caller its calls are named as here.
struct Connection {
    line: LineClient,
    caller: Caller,
}

impl Connection {
    /// The daemon serving this computer ([`line`] at its own socket).
    fn reach() -> Result<Self, String> {
        Ok(Self::of(line()?))
    }

    /// The daemon serving `socket` ([`line_at`]).
    fn at(socket: &Path) -> Result<Self, String> {
        Ok(Self::of(line_at(socket)?))
    }

    fn of(line: LineClient) -> Self {
        Self {
            line,
            // Who a call is, the daemon decides from the connection: the workspace a call names
            // is all a frame carries of its caller, and a change names none.
            caller: Caller::local_person(""),
        }
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
        let page = waited(self.line.query(&self.caller, query), CHANGE_WAIT)?;
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
        let request = InvokeRequest {
            invocation: InvocationId::new(),
            target: target.clone(),
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let id = request.invocation;
        let receipt = waited(self.line.invoke(&self.caller, request), CHANGE_WAIT)?;
        let said = |answer: Value| {
            report_in(&answer).unwrap_or_else(|| Report {
                lines: vec![answer.to_string()],
                warnings: Vec::new(),
                installed: None,
            })
        };
        let deadline = Instant::now().checked_add(CHANGE_WAIT);
        let mut status = receipt.status;
        loop {
            match status {
                ReceiptStatus::Accepted => {}
                ReceiptStatus::Completed { output } => {
                    return Ok(said(output.value.unwrap_or_default()));
                }
                ReceiptStatus::Failed { error } => {
                    return Err(format!("{}: {}", error.code, error.message));
                }
                ReceiptStatus::Rejected { error } => return Err(error.said()),
                ReceiptStatus::Cancelled { message } => {
                    return Err(message.unwrap_or_else(|| "the change was stopped".to_owned()));
                }
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err("the change did not finish in time".to_owned());
            }
            // The daemon answers once the change moves on, or after its longest wait: a change
            // that takes minutes is followed until it ends.
            match self.line.next_change(&self.caller, &id) {
                Some(moved) => {
                    let _ = moved.wait_timeout(MAX_REMOTE_WAIT);
                }
                None => std::thread::sleep(Duration::from_millis(100)),
            }
            let Some(state) = self.line.observe(&self.caller, &id) else {
                return Err("the daemon no longer keeps the change".to_owned());
            };
            status = match state.update {
                InvocationUpdate::Succeeded { output, .. } => ReceiptStatus::Completed { output },
                InvocationUpdate::Failed { error, .. } => ReceiptStatus::Failed { error },
                InvocationUpdate::Cancelled { message, .. } => ReceiptStatus::Cancelled { message },
                InvocationUpdate::Accepted { .. }
                | InvocationUpdate::Progress { .. }
                | InvocationUpdate::Item { .. } => ReceiptStatus::Accepted,
            };
        }
    }

    /// Put the file at `path` into the store the action `target` names reads from: its
    /// reference there.
    fn put(&self, path: &Path, target: &RemoteActionTarget) -> Result<String, String> {
        let unreadable = |error: std::io::Error| format!("{}: {error}", path.display());
        let mut file = std::fs::File::open(path).map_err(unreadable)?;
        let size = file.metadata().map_err(unreadable)?.len();
        let mut put = self
            .line
            .begin_put_for(size, Some(target.clone()))
            .map_err(|error| error.said())?;
        let mut offset = 0_u64;
        let mut piece = vec![0_u8; PUT_PIECE];
        loop {
            let read = file.read(&mut piece).map_err(unreadable)?;
            let Some(bytes) = piece.get(..read).filter(|bytes| !bytes.is_empty()) else {
                break;
            };
            put.append(offset, bytes).map_err(|error| error.said())?;
            offset = offset.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        }
        Box::new(put).finish().map_err(|error| error.said())
    }
}

/// `pending`, waited for `within`.
fn waited<T>(pending: Pending<Result<T, ActionCallError>>, within: Duration) -> Result<T, String> {
    match pending.wait_timeout(within) {
        Ok(Some(Ok(answer))) => Ok(answer),
        Ok(Some(Err(refused))) => Err(refused.said()),
        Ok(None) | Err(_) => Err("the daemon did not answer in time".to_owned()),
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
