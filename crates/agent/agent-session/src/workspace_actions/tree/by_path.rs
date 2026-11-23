//! Files' verbs on the folder of a workspace, made where its tree is:
//! `app.so.tilo.file-explorer#new-folder`, `#rename`, `#move`, `#copy-into`, `#trash` and
//! `#restore`, each naming paths in the workspace (`Tasks/12 Plan/notes.md`).
//!
//! On a device these verbs name the device and absolute paths, and that device changes its own
//! disk. A workspace's own folder is on no one disk: it is the tree its members mount, and
//! whoever keeps the tree changes it. So a caller with no mount (an agent working while every
//! device of its person is off, the `tilo` command with only a key, a page) names a path in the
//! workspace and no device, and the verb is carried out where the tree is, as a file put into
//! the workspace is ([`super`]). Every member's folder then shows the change as if it had been
//! made there.
//!
//! One call is one change of the tree, checked whole before anything is written
//! ([`tilo_fs::namespace`]), named in History as the caller's and answered with its
//! invocation, which `#undo` takes back as History takes back any step.

use fs_protocol::tree;
use serde_json::{Map, Value, json};
use tilo_actions::ActionOutput;
use tilo_actions::remote::{ActionCallError, ActorPlace, Caller, InvokeRequest};
use tilo_fs::namespace::Changed;
use tilo_fs::namespace::Namespace;
use tilo_server_protocol::WorkspaceId;

use super::refused;
use crate::workspace_actions::{Outcome, Worker, invoke_arguments};

/// The folder `name` names as a path in the workspace; the workspace's own when it names none.
fn folder(arguments: &Map<String, Value>, name: &str) -> Result<String, String> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(folder)) => Ok(folder.trim().to_owned()),
        Some(_) => Err(format!("`{name}` is a path in the workspace, as text")),
    }
}

/// The name or path `name` gives, which the verb cannot do without.
fn word(arguments: &Map<String, Value>, name: &str) -> Result<String, String> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("`{name}` is named"))
}

/// The entries `name` lists: names in the folder, or paths from it.
fn listed(arguments: &Map<String, Value>, name: &str) -> Result<Vec<String>, String> {
    let Some(Value::Array(entries)) = arguments.get(name) else {
        return Err(format!("`{name}` lists what the verb acts on"));
    };
    entries
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(|entry| entry.trim().to_owned())
                .ok_or_else(|| format!("`{name}` lists names or paths, as text"))
        })
        .collect()
}

/// The folder a move or a copy goes into, which is always said: `""` is the workspace's own.
fn destination(arguments: &Map<String, Value>) -> Result<String, String> {
    match arguments.get("to") {
        Some(Value::String(to)) => Ok(to.trim().to_owned()),
        _ => Err(
            "`to` names the folder in the workspace they go into; \"\" is the workspace's own"
                .to_owned(),
        ),
    }
}

/// Carry `verb` out on `tree` as `arguments` say, at the time `now`: what it changed.
fn carry_out(
    tree: &mut Namespace,
    verb: &str,
    arguments: &Map<String, Value>,
    now: u64,
) -> Result<Changed, String> {
    match verb {
        "new-folder" => {
            let parent = folder(arguments, "parent")?;
            tree.make_folder(&parent, &word(arguments, "name")?, now)
        }
        "rename" => tree.rename_entry(
            &folder(arguments, "folder")?,
            &word(arguments, "entry")?,
            &word(arguments, "name")?,
            now,
        ),
        "move" => {
            let create = arguments
                .get("create")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            tree.move_entries(
                &folder(arguments, "from")?,
                &listed(arguments, "entries")?,
                &destination(arguments)?,
                create,
                now,
            )
        }
        "copy-into" => tree.copy_entries(
            &folder(arguments, "from")?,
            &listed(arguments, "entries")?,
            &destination(arguments)?,
            now,
        ),
        "trash" => tree.trash_entries(
            &folder(arguments, "folder")?,
            &listed(arguments, "entries")?,
        ),
        "restore" => tree.restore_entries(&listed(arguments, "entries")?),
        _ => Err(format!("Files has no `{verb}` on a workspace's own folder")),
    }
}

/// Take back the call `invocation` made to `tree`, as History takes back any step: its change
/// in each document of the tree it wrote, through the history verb `undo` every resource
/// answers ([`HistoryEdit`]), done as `context` says. What somebody changed since stays theirs.
/// How many of its changes were taken back.
fn undo_call(
    tree: &mut Namespace,
    invocation: tilo_actions::InvocationId,
    context: &tilo_actions::ActionContext,
) -> Result<usize, String> {
    use ui::framework::history::{EDIT_TYPE, HistoryEdit, ResourceHistoryEdit};
    let changes = tree.changes_of_invocation(invocation);
    if changes.is_empty() {
        return Err("the workspace's folder holds no change of that call".to_owned());
    }
    let mut taken: usize = 0;
    let mut why = None;
    for (chunk, hash) in changes.into_iter().rev() {
        let edit = HistoryEdit::Undo {
            steps: None,
            invocation: None,
            by: None,
            to: None,
            step: Some(hash.to_string()),
        };
        let encoded = tilo_actions::EncodedAction {
            type_id: EDIT_TYPE.to_owned(),
            payload: serde_json::to_vec(&edit).map_err(|error| error.to_string())?,
        };
        let action = ResourceHistoryEdit::<fs_protocol::FilesTree>::new(edit);
        match tree.apply_history(chunk, (&action, encoded), context.clone()) {
            Ok(()) => taken = taken.saturating_add(1),
            Err(error) => why = Some(error.to_string()),
        }
    }
    match (taken, why) {
        (0, Some(why)) => Err(why),
        (taken, _) => Ok(taken),
    }
}

/// History's undo of the one change `step` names (its `change` as Files' `history` lists it, or
/// its whole id), in the document of the tree that keeps it, done as `context` says. The step is
/// taken back as whoever made it: the agent session it was a step of, or the agent or app that
/// wrote it, an agent's write through a mounted folder too. What somebody changed since stays
/// theirs. How many changes were taken back: one.
fn undo_step(
    tree: &mut Namespace,
    step: &str,
    context: &tilo_actions::ActionContext,
) -> Result<usize, String> {
    use ui::framework::history::{EDIT_TYPE, HistoryEdit, ResourceHistoryEdit};
    let step = step.to_ascii_lowercase();
    if step.len() < 8 || !step.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "`{step}` names no change: `step` is a version's `change` as `history` lists it"
        ));
    }
    let mut found = Vec::new();
    for (chunk, mut doc) in super::read::chunk_documents(tree) {
        for change in doc.get_changes_meta(&[]) {
            if change.hash.to_string().starts_with(&step) {
                let made = change
                    .message
                    .as_deref()
                    .and_then(tilo_actions::ActionContext::from_change_message);
                found.push((chunk, change.hash, made));
            }
        }
    }
    let (chunk, hash, made) = match found.len() {
        0 => return Err(format!("the workspace's folder holds no change `{step}`")),
        1 => found.remove(0),
        _ => {
            return Err(format!(
                "`{step}` names more than one change: give more of it"
            ));
        }
    };
    let session = made
        .as_ref()
        .and_then(|made| made.group.as_ref())
        .filter(|group| tilo_actions::context::names_a_session(&group.kind))
        .map(|group| group.id.clone());
    let by = made
        .as_ref()
        .and_then(|made| made.invoker.as_ref())
        .map(|invoker| invoker.acting.by.clone());
    let edit = HistoryEdit::Undo {
        steps: None,
        invocation: session,
        by,
        to: None,
        step: Some(hash.to_string()),
    };
    let encoded = tilo_actions::EncodedAction {
        type_id: EDIT_TYPE.to_owned(),
        payload: serde_json::to_vec(&edit).map_err(|error| error.to_string())?,
    };
    let action = ResourceHistoryEdit::<fs_protocol::FilesTree>::new(edit);
    tree.apply_history(chunk, (&action, encoded), context.clone())
        .map_err(|error| error.to_string())?;
    Ok(1)
}

/// What a verb answers: each entry it touched with where it was and where it is, the folders it
/// made on the way, and the call it was, which `undo` takes back.
fn answer(verb: &str, changed: &Changed, invocation: tilo_actions::InvocationId) -> Value {
    let entries: Vec<Value> = changed
        .entries
        .iter()
        .map(|entry| {
            json!({
                "id": entry.id,
                "from": entry.from,
                "to": entry.to,
                "folder": entry.folder,
            })
        })
        .collect();
    json!({
        "action": verb,
        "entries": entries,
        "made": changed.made,
        "invocation": invocation.0.to_string(),
    })
}

/// The tree's file actions Files' verb `verb` on a path does: what the caller is decided by.
fn file_actions_of(verb: &str) -> &'static [&'static str] {
    use fs_protocol::file_action::verbs;
    match verb {
        "new-folder" => &[verbs::NEW_FOLDER],
        "rename" | "move" => &[verbs::NEW_FOLDER, verbs::MOVE],
        "copy-into" => &[verbs::NEW_FOLDER, verbs::NEW_FILE],
        "trash" => &[verbs::TRASH],
        "restore" => &[verbs::RESTORE],
        // Taking a call back may undo any of what the tree takes.
        // A verb nobody knows is decided as everything the tree takes.
        _ => &verbs::ALL,
    }
}

impl Worker {
    /// Answer one of Files' verbs on the folder of the workspace
    /// ([`fs_protocol::tree::TREE_VERBS`]): change the tree as it asks, and answer what changed.
    ///
    /// # Errors
    /// A caller who may not change what Files keeps in the workspace, a call naming a device (whose own
    /// disk only that device changes), a path that is none of the workspace, what the verb
    /// itself refuses, a tree this holder cannot read yet, or a change the workspace did not
    /// take.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the holder keeps"
    )]
    pub(in crate::workspace_actions) fn change_tree(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<(ActionOutput, Option<ActorPlace>), Outcome> {
        let verb = request.target.address.action.as_str();
        if !self.may_on_tree(workspace_id, caller, file_actions_of(verb)) {
            return Err(Outcome::Rejected(ActionCallError::NotPermitted));
        }
        let arguments = invoke_arguments(&request.argument);
        if arguments.contains_key("device") {
            return Err(refused(
                "a path in the workspace names no device: a device's own disk is changed by \
                 Files on that device",
            ));
        }
        let link = self
            .shared
            .link
            .get()
            .cloned()
            .ok_or_else(|| refused("this host keeps no workspace's tree"))?;
        let context = super::acting_context(caller, request);
        let invocation = request.invocation;
        let value = link
            .change_tree(workspace_id, Some(context.clone()), &mut |tree| {
                if verb == tree::UNDO {
                    let taken = match arguments.get("step").and_then(Value::as_str) {
                        Some(step) => undo_step(tree, step.trim(), &context),
                        None => word(&arguments, "invocation")
                            .and_then(|id| {
                                uuid::Uuid::parse_str(&id)
                                    .map(tilo_actions::InvocationId)
                                    .map_err(|_| format!("`{id}` names no call"))
                            })
                            .and_then(|invocation| undo_call(tree, invocation, &context)),
                    }?;
                    Ok(json!({
                        "action": verb,
                        "taken": taken,
                        "invocation": invocation.0.to_string(),
                    }))
                } else {
                    let now = tilo_clock::unix_now_ns() / 1_000_000;
                    let changed = carry_out(tree, verb, &arguments, now)?;
                    Ok(answer(verb, &changed, invocation))
                }
            })
            .map_err(refused)?;
        Ok((
            ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
            None,
        ))
    }
}
