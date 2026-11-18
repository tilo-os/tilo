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
//! ([`tilo_fs::tree_put`]), and it answers the one call that takes it back (`undo`): a trash
//! answers the restore of what it took, a move the move back.

use std::collections::BTreeMap;

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

/// The folder `path` is in and its name there.
fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

/// The one call that takes a move back: the entries out of where they are now, into the folder
/// they shared. `None` when they came from several folders, where each is moved back by itself.
fn moved_back(changed: &Changed) -> Option<Value> {
    let mut origin: Option<&str> = None;
    let mut target = "";
    let mut names = Vec::new();
    for entry in &changed.entries {
        let (was_in, _) = split(entry.from.as_deref()?);
        let (now_in, name) = split(entry.to.as_deref()?);
        if origin.is_some_and(|origin| origin != was_in) {
            return None;
        }
        origin = Some(was_in);
        target = now_in;
        names.push(name);
    }
    Some(json!({"action": "move",
        "args": {"from": target, "entries": names, "to": origin?, "create": false}}))
}

/// Where each entry `changed` touched is now, as a path in the workspace.
fn now_at(changed: &Changed) -> Vec<&str> {
    changed
        .entries
        .iter()
        .filter_map(|entry| entry.to.as_deref())
        .collect()
}

/// The one call that puts `paths` in the trash, each a path in the workspace.
fn trashing(paths: &[&str]) -> Option<Value> {
    (!paths.is_empty())
        .then(|| json!({"action": "trash", "args": {"folder": "", "entries": paths}}))
}

/// Carry `verb` out on `tree` as `arguments` say, at the time `now`: what it changed, and the
/// one call that takes it back, when something was changed.
fn carry_out(
    tree: &mut Namespace,
    verb: &str,
    arguments: &Map<String, Value>,
    now: u64,
) -> Result<(Changed, Option<Value>), String> {
    match verb {
        "new-folder" => {
            let parent = folder(arguments, "parent")?;
            let changed = tree.make_folder(&parent, &word(arguments, "name")?, now)?;
            // Taking it back takes the outermost folder it made, with what it made in it.
            let made = changed.made.first().map(String::as_str).or_else(|| {
                changed
                    .entries
                    .first()
                    .and_then(|entry| entry.to.as_deref())
            });
            let undo = trashing(&made.into_iter().collect::<Vec<_>>());
            Ok((changed, undo))
        }
        "rename" => {
            let changed = tree.rename_entry(
                &folder(arguments, "folder")?,
                &word(arguments, "entry")?,
                &word(arguments, "name")?,
                now,
            )?;
            let undo = changed.entries.first().and_then(|entry| {
                let (within, named) = split(entry.to.as_deref()?);
                let (_, was) = split(entry.from.as_deref()?);
                Some(json!({"action": "rename",
                    "args": {"folder": within, "entry": named, "name": was}}))
            });
            Ok((changed, undo))
        }
        "move" => {
            let create = arguments
                .get("create")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let changed = tree.move_entries(
                &folder(arguments, "from")?,
                &listed(arguments, "entries")?,
                &destination(arguments)?,
                create,
                now,
            )?;
            let undo = moved_back(&changed);
            Ok((changed, undo))
        }
        "copy-into" => {
            let changed = tree.copy_entries(
                &folder(arguments, "from")?,
                &listed(arguments, "entries")?,
                &destination(arguments)?,
                now,
            )?;
            let undo = trashing(&now_at(&changed));
            Ok((changed, undo))
        }
        "trash" => {
            let changed = tree.trash_entries(
                &folder(arguments, "folder")?,
                &listed(arguments, "entries")?,
            )?;
            let ids: Vec<&str> = changed
                .entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect();
            let undo = json!({"action": "restore", "args": {"entries": ids}});
            Ok((changed, Some(undo)))
        }
        "restore" => {
            let changed = tree.restore_entries(&listed(arguments, "entries")?)?;
            let undo = trashing(&now_at(&changed));
            Ok((changed, undo))
        }
        _ => Err(format!("Files has no `{verb}` on a workspace's own folder")),
    }
}

/// What a verb answers: each entry it touched with where it was and where it is, the folders it
/// made on the way, and the one call that takes it back.
fn answer(verb: &str, changed: &Changed, undo: Option<&Value>) -> Value {
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
        "undo": undo,
    })
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
        if !self.may_in_files(
            workspace_id,
            caller,
            ui::permissions::policy::ActionKind::Undoable,
        ) {
            return Err(Outcome::Rejected(ActionCallError::NotPermitted));
        }
        let arguments = invoke_arguments(&request.argument);
        if arguments.contains_key("device") {
            return Err(refused(
                "a path in the workspace names no device: a device's own disk is changed by \
                 Files on that device",
            ));
        }
        let verb = request.target.address.action.as_str();
        // The tree of a governed workspace is sealed: its keys are what reading it and
        // writing to it need.
        let (mut tree, held) = self.tree_in_workspace(workspace_id, named)?;
        let now = tilo_clock::unix_now_ns() / 1_000_000;
        let (changed, undo) = carry_out(&mut tree, verb, &arguments, now).map_err(refused)?;
        let value = answer(verb, &changed, undo.as_ref());
        let writes = match &held {
            Some(_) => changed.writes,
            // A tree founded now: every document of it from nothing.
            None => tree.document_writes(&BTreeMap::new()),
        };
        self.commit_tree_writes(workspace_id, named, held.as_ref(), writes)
            .map_err(|reason| commit_failed(reason, true))?;
        Ok((
            ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
            None,
        ))
    }
}
