//! Files' verbs on the folder of a workspace whose tree is kept here:
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
//! ([`tilo_fs::tree_put`]), named in History as the caller's and answered with its
//! invocation, which `#undo` takes back as History takes back any step.

use std::collections::BTreeMap;

use fs_protocol::tree;
use serde_json::{Map, Value, json};
use tilo_actions::ActionOutput;
use tilo_actions::remote::{ActionCallError, ActorPlace, Caller, InvokeRequest};
use tilo_fs::namespace::Namespace;
use tilo_fs::tree_put::Changed;
use tilo_server_protocol::WorkspaceId;

use super::refused;
use crate::workspace_actions::{Outcome, Worker, commit_failed, invoke_arguments};

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
    /// Answer one of Files' verbs on the folder of the workspace `named`
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
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
        named: &str,
    ) -> Result<(ActionOutput, Option<ActorPlace>), Outcome> {
        let verb = request.target.address.action.as_str();
        if !self.may_on_tree(workspace_id, caller, file_actions_of(verb)) {
            return Err(Outcome::Rejected(ActionCallError::NotPermitted));
        }
        let _acting = super::acting_for(caller, request);
        let arguments = invoke_arguments(&request.argument);
        if arguments.contains_key("device") {
            return Err(refused(
                "a path in the workspace names no device: a device's own disk is changed by \
                 Files on that device",
            ));
        }
        // The tree of a governed workspace is sealed: its keys are what reading it and
        // writing to it need.
        let (mut tree, held) = self.tree_in_workspace(workspace_id, named)?;
        let (value, writes) = if verb == tree::UNDO {
            let invocation = word(&arguments, "invocation")
                .and_then(|id| {
                    uuid::Uuid::parse_str(&id)
                        .map(tilo_actions::InvocationId)
                        .map_err(|_| format!("`{id}` names no call"))
                })
                .map_err(refused)?;
            let before = tree.before_change();
            let context = super::acting_context(caller, request);
            let taken = undo_call(&mut tree, invocation, &context).map_err(refused)?;
            let value = json!({
                "action": verb,
                "taken": taken,
                "invocation": request.invocation.0.to_string(),
            });
            (value, tree.document_writes(&before))
        } else {
            let now = tilo_clock::unix_now_ns() / 1_000_000;
            let changed = carry_out(&mut tree, verb, &arguments, now).map_err(refused)?;
            let value = answer(verb, &changed, request.invocation);
            let writes = match &held {
                Some(_) => changed.writes,
                // A tree founded now: every document of it from nothing.
                None => tree.document_writes(&BTreeMap::new()),
            };
            (value, writes)
        };
        self.commit_tree_writes(workspace_id, named, held.as_ref(), writes)
            .map_err(|reason| commit_failed(reason, true))?;
        self.keep_tree(workspace_id, named, tree);
        Ok((
            ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
            None,
        ))
    }
}
