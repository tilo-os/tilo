//! Files' verbs that change a workspace's own folder, made where this device holds its tree:
//! `new-folder`, `rename`, `move`, `copy-into`, `trash`, `restore` and `write-file`, each naming
//! paths in the workspace (`Tasks/12 Plan/notes.md`), and `undo`, which takes one of them back.
//!
//! On a device these verbs name the device and absolute paths, and that device changes its own
//! disk. A workspace's own folder is on no one disk: it is the tree its members mount, and
//! whoever holds the tree changes it. So a caller with no mount (an agent working while every
//! device of its person is off, the `tilo` command with only a key, a page) names a path in the
//! workspace and no device, and every member's folder then shows the change as if it had been
//! made there.
//!
//! One call is one change of the tree, checked whole before anything is written
//! ([`tilo_fs::namespace`]), made in the caller's context and answered with its invocation, which
//! `undo` takes back as History takes back any step.

use fs_protocol::tree::{UNDO, WRITE_FILE};
use serde_json::{Map, Value, json};
use tilo_actions::{ActionContext, InvocationId};
use tilo_fs::namespace::{Changed, Namespace, Written};
use tilo_server_protocol::WorkspaceId;
use ui::history_source::DocumentHistoryEdit;
use ui::history_source::verbs::HistoryEdit;

use super::{OpenedFiles, Putting, put_scope};
use crate::apps::services::FilesPutCall;

/// One verb's change of the tree, answered with what the verb answers.
type TreeChange<'a> = Box<dyn FnMut(&mut Namespace) -> Result<Value, String> + 'a>;

/// The most text one `write-file` takes, as Files on a device does: a longer file is put into
/// the workspace and named by its `content`.
const MOST_TEXT_BYTES: usize = 256 * 1024;

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

/// `edit` of the tree's documents as the executor runs it, decoding what the tree recorded with
/// the tree's own actions.
fn tree_edit(edit: HistoryEdit) -> DocumentHistoryEdit {
    DocumentHistoryEdit::decoded_with(
        edit,
        tilo_resource::group::register::<fs_protocol::FilesTree>,
    )
}

/// Take back the call `invocation` made to `tree`, as History takes back any step: its change in
/// each document of the tree it wrote, done as `context` says. What somebody changed since stays
/// theirs. How many of its changes were taken back.
fn undo_call(
    tree: &mut Namespace,
    invocation: InvocationId,
    context: &ActionContext,
) -> Result<usize, String> {
    let changes = tree.changes_of_invocation(invocation);
    if changes.is_empty() {
        return Err("the workspace's folder holds no change of that call".to_owned());
    }
    let mut taken: usize = 0;
    let mut why = None;
    for (chunk, hash) in changes.into_iter().rev() {
        let action = tree_edit(HistoryEdit::Undo {
            steps: None,
            invocation: None,
            by: None,
            to: None,
            step: Some(hash.to_string()),
            chosen: None,
        });
        let encoded = action.encoded();
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

/// History's undo of the one change `step` names (its id, or the start of it), in the document
/// of the tree that keeps it, done as `context` says. The step is taken back as whoever made it:
/// the agent session it was a step of, or the agent or app that wrote it, an agent's write
/// through a mounted folder too. What somebody changed since stays theirs. How many changes were
/// taken back: one.
fn undo_step(tree: &mut Namespace, step: &str, context: &ActionContext) -> Result<usize, String> {
    let step = step.to_ascii_lowercase();
    if step.len() < 8 || !step.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "`{step}` names no change: `step` is a change's id as History lists it"
        ));
    }
    let mut found = Vec::new();
    for write in tree.document_writes(&std::collections::BTreeMap::new()) {
        let mut doc = automerge::AutoCommit::new();
        tilo_automerge::ApplyIncoming::load_incoming(&mut doc, &write.changes)
            .map_err(|error| format!("the workspace's folder does not read: {error}"))?;
        for change in doc.get_changes_meta(&[]) {
            if change.hash.to_string().starts_with(&step) {
                let made = change
                    .message
                    .as_deref()
                    .and_then(ActionContext::from_change_message);
                found.push((write.chunk, change.hash, made));
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
    let action = tree_edit(HistoryEdit::Undo {
        steps: None,
        invocation: session,
        by,
        to: None,
        step: Some(hash.to_string()),
        chosen: None,
    });
    let encoded = action.encoded();
    tree.apply_history(chunk, (&action, encoded), context.clone())
        .map_err(|error| error.to_string())?;
    Ok(1)
}

/// Each entry `changed` touched, with where it was and where it is.
fn entries(changed: &Changed) -> Vec<Value> {
    changed
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
        .collect()
}

/// What a verb answers: each entry it touched, the folders it made on the way, and the call it
/// was, which `undo` takes back.
fn answer(verb: &str, changed: &Changed, invocation: Option<InvocationId>) -> Value {
    json!({
        "action": verb,
        "entries": entries(changed),
        "made": changed.made,
        "invocation": invocation.map(|invocation| invocation.0.to_string()),
    })
}

/// What `write-file` answers: where the file is and how long, whether it took the place of
/// another's content, the folders made on the way, and the call it was.
fn written(written: &Written, size: u64, invocation: Option<InvocationId>) -> Value {
    let changed = &written.changed;
    json!({
        "action": WRITE_FILE,
        "path": changed.entries.first().and_then(|entry| entry.to.as_deref()),
        "size": size,
        "replaced": written.replaced,
        "entries": entries(changed),
        "made": changed.made,
        "invocation": invocation.map(|invocation| invocation.0.to_string()),
    })
}

/// What a `write-file` names: the file `name` in the folder `folder`, written over a file of
/// that name only when `replace` says so.
struct Writing {
    putting: Putting,
    replace: bool,
}

impl Writing {
    /// What `arguments` name of the file to write; text it is written from is kept in the
    /// workspace first, as content put into it is.
    fn named(
        files: &OpenedFiles,
        workspace: WorkspaceId,
        arguments: &Map<String, Value>,
    ) -> Result<Self, String> {
        let dir = folder(arguments, "folder")?;
        let name = word(arguments, "name").map_err(|_| "`name` is the file's name".to_owned())?;
        let replace = match arguments.get("replace") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(replace)) => *replace,
            Some(_) => return Err("`replace` is true or false".to_owned()),
        };
        let (content, hash) = match (arguments.get("text"), arguments.get("content")) {
            (Some(Value::String(text)), None | Some(Value::Null)) => {
                if text.len() > MOST_TEXT_BYTES {
                    return Err(format!(
                        "`text` is {} bytes, and `write-file` takes at most {MOST_TEXT_BYTES} as \
                         text: put a longer file into the workspace and name its `content`",
                        text.len()
                    ));
                }
                let hash = tilo_content::hash_bytes(text.as_bytes());
                files
                    .shelf
                    .keep(&put_scope(workspace), &hash, text.as_bytes())?;
                (format!("tilo.content:{hash}"), hash)
            }
            (None | Some(Value::Null), Some(Value::String(content))) => {
                let content = content.trim();
                let hash = tilo_actions::content_hash(content).ok_or_else(|| {
                    format!(
                        "`{content}` names no content: give the file's `tilo.content:<reference>`"
                    )
                })?;
                (content.to_owned(), hash.to_owned())
            }
            _ => {
                return Err(
                    "`write-file` takes the file's `text`, or its `content` as a `tilo.content:` \
                     reference: one of the two"
                        .to_owned(),
                );
            }
        };
        Ok(Self {
            putting: Putting {
                content,
                hash,
                dir,
                name,
            },
            replace,
        })
    }
}

impl OpenedFiles {
    /// Carry out Files' `verb` on `workspace`'s folder as `arguments` say, as one change of its
    /// tree made in `call`'s context, and answer what changed and the call it was.
    ///
    /// `confirm` is asked once what the change needs is ready and before the tree is touched:
    /// whether the workspace still lets the caller change its files.
    ///
    /// # Errors
    /// A call naming a device (whose own disk only that device changes), a path that is none of
    /// the workspace, what the verb itself refuses, content the workspace does not hold, keys
    /// the workspace's files need and this device cannot read, a call that does not sign here, a
    /// grant taken back meanwhile, or a change the workspace did not take.
    pub fn change_folder(
        &self,
        workspace: WorkspaceId,
        verb: &str,
        arguments: &Map<String, Value>,
        call: Option<FilesPutCall<'_>>,
        confirm: &dyn Fn() -> bool,
    ) -> Result<Value, String> {
        if arguments.contains_key("device") {
            return Err(
                "a path in the workspace names no device: a device's own disk is changed by \
                 Files on that device"
                    .to_owned(),
            );
        }
        let acting = call
            .map(|call| self.kept_call(workspace, call))
            .transpose()?;
        let invocation = acting
            .as_ref()
            .and_then(|context| context.source_invocation);
        let context = acting.clone().unwrap_or_default();
        let now = tilo_clock::unix_now_ms();
        let mut change: TreeChange<'_> = match verb {
            WRITE_FILE => {
                let writing = Writing::named(self, workspace, arguments)?;
                let sealing = self.sealing(workspace)?;
                let mut named =
                    self.named_for_the_tree(workspace, sealing.as_ref(), vec![writing.putting])?;
                self.same_keys(workspace, sealing.as_ref())?;
                let (putting, manifest, size) = named.pop().ok_or("no file was named")?;
                let replace = writing.replace;
                Box::new(move |tree| {
                    let done = tree.write_file(
                        &putting.dir,
                        &putting.name,
                        (&manifest, size),
                        replace,
                        now,
                    )?;
                    Ok(written(&done, size, invocation))
                })
            }
            UNDO => Box::new(|tree| {
                let taken = if let Some(step) = arguments.get("step").and_then(Value::as_str) {
                    undo_step(tree, step.trim(), &context)?
                } else {
                    let id = word(arguments, "invocation")?;
                    let undone = uuid::Uuid::parse_str(&id)
                        .map(InvocationId)
                        .map_err(|_| format!("`{id}` names no call"))?;
                    undo_call(tree, undone, &context)?
                };
                Ok(json!({
                    "action": UNDO,
                    "taken": taken,
                    "invocation": invocation.map(|invocation| invocation.0.to_string()),
                }))
            }),
            verb => Box::new(move |tree| {
                let changed = carry_out(tree, verb, arguments, now)?;
                Ok(answer(verb, &changed, invocation))
            }),
        };
        if !confirm() {
            return Err("the workspace no longer lets this caller change its files".to_owned());
        }
        tilo_rt::app()
            .block_on(
                self.workspaces
                    .puts()
                    .change(workspace, acting, &mut *change),
            )
            .map_err(|error| error.to_string())?
    }
}
