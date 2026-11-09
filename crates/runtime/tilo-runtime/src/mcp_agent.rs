//! The agent surface: five tools an AI collaborator can drive any app with.
//!
//! `tilo_ls`, `tilo_read`, `tilo_do`, `tilo_find` and `tilo_status` address
//! everything by **path**: `notes/Todo/body#append-paragraph` is the app
//! (`notes`), the resource by its title, kind or key (`Todo`, `body`) and the
//! action after `#`. `browser@4711/page#read-page` names one window of an
//! app by its provider (what the desktop's `open` answers, what `tilo_ls`
//! lists); without it the app's newest window answers. The host's own
//! services answer to their app too: `desktop#list-windows` is the desktop's
//! layout. A path names things as a person would and survives a
//! compositor restart; the opaque ids a broker mints per publication
//! (`tilo-ui-bound:v1:<uuid>:<uuid>:<hex>`) stay inside this module, which
//! resolves a path against the live directory on every call.
//!
//! Answers are text, not JSON: a model pays for every byte it reads, and a
//! listing line is a third the size of the same row as an object. Reads come
//! back as markdown with block ids, invocations answer with their result
//! (they wait for it), and nothing here needs a second call to learn
//! whether the first one worked.
use super::{
    ActionHost, Addressed, CatalogueLimits, CatalogueQuery, CatalogueRow, DEFAULT_WAIT_MS,
    Deserialize, MAX_CALLS, McpServer, OneCall, RemoteActionTarget, Value, batch_invocation,
    invocation_id, json, page, parse,
};
use std::collections::HashMap;
use std::fmt::Write as _;
use tilo_actions::remote::Found;
use tilo_actions::{ActionDescriptor, OperationKind};
use tilo_lock::MutexRecover as _;

#[path = "mcp_answer.rs"]
mod answer;
#[path = "mcp_agent_find.rs"]
mod find;
#[path = "mcp_render.rs"]
mod render;
#[path = "mcp_tilo.rs"]
mod tilo;
#[path = "mcp_watch.rs"]
pub(super) mod watch;
use answer::{action_row, action_rows, answer_of, text_result};
use render::{
    action_line, action_page, block_line, parse_read_cursor, read_answer, render_value,
    restart_pause, strip_blank,
};
use tilo::actions_note;
/// A person's own line in a run's conversation, run in the task's host as they are
/// (shell.md §8.1): what the door's `person_line` hands on.
pub use tilo::{PersonStep, literals_in};
pub(super) use tilo::{ProgramForms, Programs, TaskLink};
pub use tilo::{TaskHosts, TaskProgram};

/// Resource rows one directory level reads at most, across its pages.
const LEVEL_PAGES: usize = 4;
/// Rows asked per page.
const PAGE_ROWS: u32 = 128;
/// Containers at an app's top level whose contents the app's root also
/// offers: a notebook's pages are the app's own.
const ROOT_PEEK: usize = 3;
/// Containers a name that is not where it was looked for reads on, one by
/// one, before it is called missing.
const DEEP_READS: usize = 16;
/// Longest name asked of an app whole; a longer one is a key or an id.
const NAME_BYTES: usize = 256;
/// Lines `tilo_ls` answers before a cursor.
const LS_LINES: usize = 40;
/// Hits `tilo_find` answers.
const FIND_HITS: usize = 15;
/// Characters of a read answered before the rest is cut.
const READ_CHARS: usize = 12_000;
/// What a cursor `tilo_read` issued for the rest of a cut read starts with:
/// `read+<characters already answered>`, then `+<the app's cursor>` when the
/// cut answer was a page the app's own cursor asked for.
const READ_CURSOR: &str = "read+";
/// The longest pause between two reads of a directory level that keeps
/// changing while it is read.
const RESTART_PAUSE_MAX: std::time::Duration = std::time::Duration::from_millis(200);
/// Longest label shown whole in a listing line.
const LABEL_CHARS: usize = 60;
/// Longest first sentence of an action's description in an action's page.
const DESCRIPTION_CHARS: usize = 200;
/// How many names an error lists before it stops.
const NAMES_LISTED: usize = 12;
/// What a bound UI resource's id starts with (ui-core's publication prefix).
const BOUND_PREFIX: &str = "tilo-ui-bound:v1:";
/// Pages of installed apps read at most: a page holds 256.
const INSTALLED_PAGES: usize = 4;

/// The agent tools, built once.
pub(super) fn tool_definitions() -> &'static [Value] {
    static TOOLS: std::sync::LazyLock<Vec<Value>> = std::sync::LazyLock::new(tools);
    &TOOLS
}

fn tools() -> Vec<Value> {
    let string = json!({"type":"string"});
    let integer = json!({"type":"integer"});
    let read = json!({"readOnlyHint":true,"openWorldHint":false});
    let call = json!({
        "type":"object",
        "properties":{"path":string,"args":{"type":"object"}},
        "required":["path"],
        "additionalProperties":false
    });
    [
        (
            "tilo_ls",
            "List apps, then what is in them. No path: the apps. `notes`: that app's resources and actions. `notes/Todo`: a note's children and actions. `notes/Todo#rename`: one action's parameters. A path is app[@window]/resource names (a title, kind or key, any case), stable across restarts; an action follows `#`. kind narrows to resources or actions; limit and cursor page.",
            json!({"path":string,"kind":{"enum":["resources","actions"]},"limit":integer,"cursor":string}),
            json!([]),
            read.clone(),
        ),
        (
            "tilo_read",
            "Read a resource as compact text. `notes/Todo` answers the note as markdown, each block ending in its id (`^c01e`; a document's `{#c01e}`); block actions take the id alone: `c01e`. A long read ends with a cursor line; pass it back as cursor for the rest.",
            json!({"path":string,"cursor":string,"max_chars":integer}),
            json!(["path"]),
            read.clone(),
        ),
        (
            "tilo_do",
            "Run an action, get its result: `{path: \"notes/Todo#append-paragraph\", args: {text: \"Buy milk\"}}`. Several in order, stopping at a failure: `calls: [{path, args}, …]`. `notes#open` starts an app; `terminal#run` runs a shell `command`. Answers `ok` with the result, or `failed: why`; running past wait_ms (default 5000): `pending <id>` for tilo_status. Same invocation_id never runs twice.",
            json!({"path":string,"args":{"type":"object"},"calls":{"type":"array","minItems":1,"maxItems":MAX_CALLS,"items":call},"wait_ms":integer,"invocation_id":string}),
            json!([]),
            json!({"readOnlyHint":false,"destructiveHint":true,"idempotentHint":false,"openWorldHint":true}),
        ),
        (
            "tilo_find",
            "Search the workspace by words, best first, as the desktop's palette does: `{query: \"my mails\"}`, optionally app: \"notes\". One line per hit, a path `tilo_ls` and `tilo_do` take, `path#action` for an action.",
            json!({"query":string,"app":string,"limit":integer}),
            json!(["query"]),
            read,
        ),
        (
            "tilo",
            "Run a tilolang program as this task: `notes.notebook.notes.filter(.pinned)`. Lets persist; answers' refs (n3) can be reused. A bulk or destructive program previews: resend with confirm: true. `undo`, `undo task`, `history task`.",
            json!({"program":string,"confirm":{"type":"boolean"}}),
            json!(["program"]),
            json!({"readOnlyHint":false,"destructiveHint":true,"idempotentHint":false,"openWorldHint":false}),
        ),
        (
            "tilo_watch",
            "What changed since cursor: a resource's changes (`path`), runs starting and finishing, and who is in the workspace. Held until something happens, at most wait_ms (default 25000). Pass back the cursor it answers.",
            json!({"cursor":string,"path":string,"wait_ms":integer}),
            json!([]),
            json!({"readOnlyHint":true,"openWorldHint":false}),
        ),
        (
            "tilo_status",
            "How a pending invocation went (cancel: true stops it).",
            json!({"invocation_id":string,"cancel":{"type":"boolean"}}),
            json!(["invocation_id"]),
            json!({"readOnlyHint":false,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}),
        ),
    ]
    .into_iter()
    .map(|(name, description, properties, required, annotations)| {
        json!({
            "name": name,
            "description": description,
            "inputSchema": {
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false
            },
            "annotations": annotations
        })
    })
    .collect()
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LsArgs {
    path: Option<String>,
    kind: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: String,
    cursor: Option<String>,
    max_chars: Option<usize>,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct DoStep {
    path: String,
    #[serde(default)]
    args: serde_json::Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DoArgs {
    path: Option<String>,
    #[serde(default)]
    args: serde_json::Map<String, Value>,
    calls: Option<Vec<DoStep>>,
    wait_ms: Option<u64>,
    invocation_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FindArgs {
    query: String,
    app: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentStatusArgs {
    invocation_id: String,
    #[serde(default)]
    cancel: bool,
}

/// What an agent tool answers: the text and whether it reports a failure.
/// How running one action by its path went.
enum Done {
    /// The app was not running and was started: what is in it now, and the
    /// workspace's answer that started it.
    Launched { value: Value, reply: Reply },
    /// The action ran: the host's answer, the action at its resolved path,
    /// and the app it ran in.
    Invoked {
        value: Value,
        path: String,
        descriptor: Box<ActionDescriptor>,
        app: String,
    },
}

pub(super) struct Reply {
    text: String,
    failed: bool,
    /// The typed answer, for a caller that asked for it (a structured read).
    structured: Option<Value>,
}

impl Reply {
    fn ok(text: String) -> Self {
        Self {
            text,
            failed: false,
            structured: None,
        }
    }

    fn failed(text: String) -> Self {
        Self {
            text,
            failed: true,
            structured: None,
        }
    }
}

/// A path, taken apart: `app/segment/segment#action`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ParsedPath {
    pub(super) app: String,
    pub(super) segments: Vec<String>,
    pub(super) action: Option<String>,
}

/// Parse `text`. A name containing `/`, `#` or `\` writes it with a leading
/// backslash.
pub(super) fn parse_path(text: &str) -> Result<ParsedPath, String> {
    let mut parts: Vec<String> = vec![String::new()];
    let mut action = String::new();
    let mut in_action = false;
    let mut escaped = false;
    for character in text.trim().chars() {
        if escaped {
            escaped = false;
            push_char(&mut parts, &mut action, in_action, character);
            continue;
        }
        match character {
            '\\' => escaped = true,
            '#' if !in_action => in_action = true,
            '/' if !in_action => parts.push(String::new()),
            other => push_char(&mut parts, &mut action, in_action, other),
        }
    }
    let action = in_action.then_some(action);
    let mut parts = parts
        .into_iter()
        .map(|part| part.trim().to_owned())
        .filter(|part| !part.is_empty());
    let app = parts
        .next()
        .ok_or("a path starts with an app: notes/Todo")?;
    let action = action
        .map(|action| action.trim().to_owned())
        .filter(|action| !action.is_empty());
    Ok(ParsedPath {
        app,
        segments: parts.collect(),
        action,
    })
}

fn push_char(parts: &mut [String], action: &mut String, in_action: bool, character: char) {
    if in_action {
        action.push(character);
    } else if let Some(part) = parts.last_mut() {
        part.push(character);
    }
}

/// A name written into a path: `/`, `#` and `\` escaped.
fn path_name(name: &str) -> String {
    let mut written = String::with_capacity(name.len());
    for character in name.chars() {
        if matches!(character, '/' | '#' | '\\') {
            written.push('\\');
        }
        written.push(character);
    }
    written
}

/// The logical id a bound UI resource's id carries in hex at its end
/// (`tilo-ui-bound:v1:<publication>:<instance>:<hex>`): what the app calls
/// the resource, which survives the publication the rest of the id names.
fn bound_key(id: &str) -> Option<String> {
    let hex = id.strip_prefix(BOUND_PREFIX)?;
    let hex = hex.rsplit(':').next()?;
    if hex.is_empty() || hex.len() % 2 != 0 {
        return None;
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(hex.get(at..at.checked_add(2)?)?, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    String::from_utf8(bytes)
        .ok()
        .filter(|key| !key.is_empty() && !key.chars().any(char::is_control))
}

/// One resource of an app's directory, as an agent names it.
#[derive(Clone, Debug)]
struct Node {
    /// The broker's id: never shown, only sent back.
    id: String,
    kind: String,
    label: String,
    /// The app's own stable name for it: its semantic key, else the logical
    /// id inside a bound id, else the id.
    key: String,
    parent: String,
    container: bool,
    actions: Vec<ActionDescriptor>,
}

impl Node {
    /// The part of the key that tells two resources of one kind apart
    /// (`notes.page:3fa2…` → `3fa2…`).
    fn token(&self) -> &str {
        self.key
            .rsplit([':', '/'])
            .find(|part| !part.is_empty())
            .unwrap_or(&self.key)
    }

    /// Whether `other`'s key names this one's distinguishing part.
    fn owns(&self, other: &Node) -> bool {
        let token = self.token();
        token.len() >= 8 && other.key != self.key && other.key.contains(token)
    }

    /// Whether `other` is this resource's own and not one of its children.
    fn beside(&self, other: &Node) -> bool {
        self.owns(other) && !other.key.starts_with(&self.key)
    }

    /// Whether this resource is one of `other`'s: it sits under the other's id.
    fn within(&self, other: &Node) -> bool {
        matches!(self.id.strip_prefix(other.id.as_str()), Some(rest) if rest.starts_with('/'))
    }

    /// How well `name` names this node: 0 its label, 1 its key (whole or last
    /// part), 2 its kind, 3 the start of either; `None` when it does not.
    fn answers_to(&self, name: &str) -> Option<u8> {
        let same = |text: &str| text.trim().eq_ignore_ascii_case(name);
        let tail = |text: &str| {
            text.rsplit(['.', ':', '/'])
                .next()
                .is_some_and(|tail| tail.eq_ignore_ascii_case(name))
        };
        if same(&self.label) {
            return Some(0);
        }
        let key_tail = self.key.rsplit('/').next().unwrap_or_default();
        if same(&self.key) || same(&self.id) || same(key_tail) || same(self.token()) {
            return Some(1);
        }
        if same(&self.kind) || tail(&self.kind) {
            return Some(2);
        }
        let lower = name.to_lowercase();
        let starts = |text: &str| text.to_lowercase().starts_with(&lower);
        (lower.chars().count() >= 3
            && (starts(&self.label) || (lower.len() >= 6 && starts(self.token()))))
        .then_some(3)
    }

    /// The name that goes in a path: its label when it tells it apart from
    /// `siblings`, else its key's last part (`body`), else its key.
    fn name(&self, siblings: &[Node]) -> String {
        // Neither what belongs to this resource nor what it is a part of
        // competes with it: a note titled like its notebook keeps its title.
        let apart = |name: &str| {
            !name.trim().is_empty()
                && siblings
                    .iter()
                    .filter(|other| other.id != self.id)
                    .filter(|other| !self.owns(other) && !self.within(other))
                    .all(|other| other.answers_to(name).is_none_or(|rank| rank > 0))
        };
        let label = self.label.trim();
        if apart(label) {
            return path_name(label);
        }
        let tail = self.key.rsplit('/').next().unwrap_or_default();
        if tail != self.key && apart(tail) {
            return path_name(tail);
        }
        path_name(&self.key)
    }
}

/// The resources and actions one directory listing holds under `parent`.
#[derive(Clone, Default)]
struct Level {
    /// Actions of the parent itself.
    own: Vec<ActionDescriptor>,
    children: Vec<Node>,
    /// The directory revision it was read at.
    revision: String,
}

fn level_of(rows: &[CatalogueRow], parent: &str) -> Level {
    let mut level = Level::default();
    let mut at: HashMap<&str, usize> = HashMap::new();
    for row in rows {
        if !parent.is_empty() && row.resource.id == parent {
            if let Some(action) = &row.action {
                level.own.push(action.clone());
            }
            continue;
        }
        let key = row
            .action
            .as_ref()
            .and_then(|action| action.semantic_identity.as_ref())
            .map(|identity| identity.key.clone())
            .or_else(|| bound_key(&row.resource.id));
        let index = *at.entry(row.resource.id.as_str()).or_insert_with(|| {
            level.children.push(Node {
                id: row.resource.id.clone(),
                kind: row.resource.kind.clone(),
                label: row.resource.label.clone(),
                key: key.clone().unwrap_or_else(|| row.resource.id.clone()),
                parent: row.parent_resource.clone(),
                container: false,
                actions: Vec::new(),
            });
            level.children.len().saturating_sub(1)
        });
        if let Some(node) = level.children.get_mut(index) {
            node.container |= row.has_children;
            if let Some(key) = key.filter(|_| node.key == node.id) {
                node.key = key;
            }
            if let Some(action) = &row.action {
                node.actions.push(action.clone());
            }
        }
    }
    level
}

/// Whether `segment`, a segment of a path, is written the way a program types a type
/// (`tasks.Task`): a package's alias, a dot, and a name that begins with a capital letter.
/// Nothing else is asked of the task's programs: a title, a key or a kind (`notes.notebook`)
/// is never a program form, so a path that reads one does not start a task's host.
fn is_program_form(segment: &str) -> bool {
    segment.rsplit_once('.').is_some_and(|(alias, name)| {
        !alias.is_empty() && name.starts_with(|c: char| c.is_ascii_uppercase())
    })
}

/// How well `name`, a segment of a path, names `node`: [`Node::answers_to`]'s rank, and a type
/// named the way a program types it (`tasks.Task`) is rank 2 too, as its kind is: a listing
/// names a resource by that program form, so a name read off a row walks back to the resource.
fn rank(name: &str, node: &Node, forms: Option<&ProgramForms>) -> Option<u8> {
    node.answers_to(name).or_else(|| {
        forms
            .is_some_and(|forms| forms.answers(name, &node.kind))
            .then_some(2)
    })
}

/// Pick the node `name` means among `nodes`: the best-ranked one, when one
/// is. Several equally good is an error naming them.
fn pick<'a>(
    name: &str,
    nodes: &'a [Node],
    forms: Option<&ProgramForms>,
) -> Result<Option<&'a Node>, String> {
    let ranked: Vec<(u8, &Node)> = nodes
        .iter()
        .filter_map(|node| Some((rank(name, node, forms)?, node)))
        .collect();
    let Some(best) = ranked.iter().map(|(rank, _)| *rank).min() else {
        return Ok(None);
    };
    let mut found: Vec<&Node> = Vec::new();
    for (_, node) in ranked.iter().filter(|(rank, _)| *rank == best) {
        if !found.iter().any(|seen| seen.id == node.id) {
            found.push(node);
        }
    }
    // One that owns the rest (their keys name it) is the one meant: a note,
    // not the body published beside it under the same title.
    let owners: Vec<&&Node> = found
        .iter()
        .filter(|node| {
            found
                .iter()
                .all(|other| other.id == node.id || node.owns(other))
        })
        .collect();
    if let [owner] = owners.as_slice() {
        return Ok(Some(**owner));
    }
    match found.as_slice() {
        [one] => Ok(Some(one)),
        many => Err(format!(
            "{name} names {} resources: {}; name one by its key",
            many.len(),
            many.iter()
                .take(NAMES_LISTED)
                .map(|node| format!("{} ({})", node.label.trim(), node.key))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The action `name` means among `actions`: its id, its label, or the one id
/// that starts with it.
fn pick_action<'a>(
    name: &str,
    actions: &'a [ActionDescriptor],
    at: &str,
) -> Result<&'a ActionDescriptor, String> {
    let id = |action: &ActionDescriptor| action.address.action.clone();
    if let Some(found) = actions
        .iter()
        .find(|action| id(action).eq_ignore_ascii_case(name))
        .or_else(|| {
            actions
                .iter()
                .find(|action| action.action_label.eq_ignore_ascii_case(name))
        })
    {
        return Ok(found);
    }
    let lower = name.to_lowercase();
    let mut starts = actions
        .iter()
        .filter(|action| id(action).to_lowercase().starts_with(&lower));
    if let (Some(only), None) = (starts.next(), starts.next()) {
        return Ok(only);
    }
    let mut ids: Vec<String> = actions.iter().map(id).collect();
    ids.dedup();
    Err(if ids.is_empty() {
        format!("{at} has no actions")
    } else {
        format!(
            "{at} has no action {name}; its actions: {}",
            list_names(&ids)
        )
    })
}

/// The names below an app that reach `node`, in the levels this call read
/// (`Notebook/Todo`), or `None` when they do not reach it.
fn path_of(node: &Node, levels: &Levels) -> Option<String> {
    let mut names: Vec<String> = Vec::new();
    let mut current = node.clone();
    for _ in 0..DEEP_READS {
        let level = levels
            .values()
            .find(|level| level.children.iter().any(|seen| seen.id == current.id))?;
        names.push(current.name(&level.children));
        if current.parent.is_empty() {
            names.reverse();
            return Some(names.join("/"));
        }
        current = levels
            .values()
            .flat_map(|level| level.children.iter())
            .find(|seen| seen.id == current.parent)?
            .clone();
    }
    None
}

/// The action `name` means anywhere in what `resolved` names: the target's
/// own actions (`notes/Todo#rename`), else one the app publishes on a
/// resource of its own (`settings#add-email-account` is on `settings.root`).
/// `ranked` is what the app answered for `name`, each action with how well it
/// is named and the key of the resource it is published on. Several
/// resources carrying it equally well is refused naming them, each with the
/// path that names it: the app alone does not mean one of two.
fn pick_action_anywhere<'a>(
    name: &str,
    resolved: &'a Resolved,
    ranked: &[(u8, String, &'a ActionDescriptor)],
) -> Result<(Option<String>, &'a ActionDescriptor), String> {
    let error = match pick_action(name, &resolved.actions, &resolved.path) {
        Ok(action) => return Ok((None, action)),
        Err(error) => error,
    };
    // An action some way below what the path names is not that resource's:
    // a note's own action is the note's, not the notebook's. The app is the
    // one name that stands for what it publishes anywhere in its directory.
    if resolved.node.is_some() {
        return Err(error);
    }
    let Some(best) = ranked.iter().map(|(rank, ..)| *rank).min() else {
        return Err(error);
    };
    let mut found: Vec<(String, &'a ActionDescriptor)> = Vec::new();
    for (_, key, action) in ranked.iter().filter(|(rank, ..)| *rank == best) {
        if !found.iter().any(|(_, seen)| seen.address == action.address) {
            found.push((key.clone(), action));
        }
    }
    match found.as_slice() {
        [(key, only)] => Ok((Some(key.clone()), only)),
        many => Err(format!(
            "{name} names {} actions in {}: {}; name one with the resource it is on",
            many.len(),
            resolved.path,
            many.iter()
                .take(NAMES_LISTED)
                .map(|(key, action)| format!("{}/{}#{}", resolved.path, key, action.address.action))
                .chain(
                    (many.len() > NAMES_LISTED)
                        .then(|| format!("… +{}", many.len().saturating_sub(NAMES_LISTED)))
                )
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// How well `name` names `action`: its id, its label, the start of its id,
/// or not at all.
fn action_rank(name: &str, action: &ActionDescriptor) -> Option<u8> {
    if action.address.action.eq_ignore_ascii_case(name) {
        return Some(0);
    }
    if action.action_label.eq_ignore_ascii_case(name) {
        return Some(1);
    }
    action
        .address
        .action
        .to_lowercase()
        .starts_with(&name.to_lowercase())
        .then_some(2)
}

/// The action that reads a resource, among those its directory level offers:
/// the one declared to read its document, else one named `read`, else a view
/// whose action starts with `read` (`read-messages` next to `read`).
fn reads_action(actions: &[ActionDescriptor]) -> Option<ActionDescriptor> {
    actions
        .iter()
        .find(|action| action.contract.reads_document())
        .or_else(|| {
            actions
                .iter()
                .find(|action| action.address.action == "read")
        })
        .or_else(|| {
            actions.iter().find(|action| {
                action.kind == OperationKind::View && action.address.action.starts_with("read")
            })
        })
        .cloned()
}

/// [`list_names`], or `none` for no names: an empty list reads as a
/// sentence cut short.
fn list_or_none(names: &[String]) -> String {
    if names.is_empty() {
        "none".to_owned()
    } else {
        list_names(names)
    }
}

fn list_names(names: &[String]) -> String {
    let mut shown: Vec<&str> = names
        .iter()
        .take(NAMES_LISTED)
        .map(String::as_str)
        .collect();
    let more = names.len().saturating_sub(NAMES_LISTED);
    let tail = format!("… +{more}");
    if more > 0 {
        shown.push(&tail);
    }
    shown.join(", ")
}

/// Cut `text` to at most `chars` characters at a line break. The count of
/// characters left out, when any.
fn cut_at_line(text: &mut String, chars: usize) -> Option<usize> {
    let total = text.chars().count();
    if total <= chars {
        return None;
    }
    let end = text
        .char_indices()
        .nth(chars)
        .map_or(text.len(), |(at, _)| at);
    let head = text.get(..end).unwrap_or_default();
    let keep = head.rfind('\n').filter(|at| *at > end / 2).unwrap_or(end);
    text.truncate(keep);
    Some(total.saturating_sub(text.chars().count()))
}

/// The arguments `reader`, offered by `owner`, reads `resolved` with: the
/// target as its `item` when an ancestor reads it, the app's cursor, and as
/// large a page as a paged read allows.
fn read_arguments(
    resolved: &Resolved,
    owner: &str,
    reader: &ActionDescriptor,
    app_cursor: Option<&str>,
) -> Result<serde_json::Map<String, Value>, String> {
    let mut arguments = serde_json::Map::new();
    let has = |name: &str| {
        reader
            .params
            .iter()
            .any(|param| param.name.as_ref() == name)
    };
    if let Some(node) = resolved.node.as_ref().filter(|node| node.id != owner)
        && has("item")
    {
        arguments.insert("item".into(), node.key.clone().into());
    }
    if let Some(cursor) = app_cursor {
        if has("cursor") {
            arguments.insert("cursor".into(), cursor.to_owned().into());
        } else {
            return Err(format!(
                "{} reads in one piece; it takes no cursor",
                resolved.path
            ));
        }
    }
    // A paged read asks for as much as it allows.
    for param in &reader.params {
        if param.name.as_ref() == "pageSize"
            && param.required
            && !arguments.contains_key("pageSize")
        {
            let bound = param
                .schema
                .as_deref()
                .and_then(|schema| schema.get("maximum"))
                .and_then(Value::as_u64)
                .unwrap_or(64);
            arguments.insert("pageSize".into(), bound.into());
        }
    }
    Ok(arguments)
}

/// The cursor for the rest of a read cut after `answered` characters of the
/// page `app_cursor` asked for (the first page when `None`).
fn read_cursor(answered: usize, app_cursor: Option<&str>) -> String {
    match app_cursor {
        Some(app_cursor) => format!("{READ_CURSOR}{answered}+{app_cursor}"),
        None => format!("{READ_CURSOR}{answered}"),
    }
}

/// A resource as a read answers it, typed: what a program takes from
/// `tilo_read` (its structured form), and what the markdown read is a
/// rendering of. One reader answers both, at the same cost.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(super) struct ResourceRead {
    /// The resource read, as a path the other tools take.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(super) path: String,
    /// Its kind, as the app names it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) kind: Option<String>,
    /// What the app answered, each field typed as the app typed it, but for
    /// the lists of what the resource holds, which are its children.
    pub(super) fields: Value,
    /// What it holds, each by the name a person reads and the reference a
    /// read or an action takes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) children: Vec<ChildRef>,
    /// The app's cursor for the next page, passed back as `cursor`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) cursor: Option<String>,
}

/// One thing a resource holds, as its read lists it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(super) struct ChildRef {
    /// The list of the read it is in (`pages`).
    pub(super) list: String,
    /// Its title, name or label.
    pub(super) name: String,
    /// The app's own key for it: what a read takes as its `item`.
    pub(super) item: String,
    /// The path that reads it or acts on it, once the read is of a path.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(super) path: String,
}

impl ResourceRead {
    /// `value`, an app's read or an action's output, taken apart: a list
    /// whose every entry names an item and a title is children, the rest is
    /// fields.
    fn of(value: &Value) -> Self {
        let mut fields = value.clone();
        let mut children = Vec::new();
        if let Value::Object(map) = &mut fields {
            let mut whole = Vec::new();
            for (list, entries) in map.iter() {
                let Value::Array(entries) = entries else {
                    continue;
                };
                let named: Vec<ChildRef> = entries
                    .iter()
                    .filter_map(|entry| {
                        let item = entry.get("item").and_then(Value::as_str)?;
                        let name = ["title", "name", "label"]
                            .iter()
                            .find_map(|key| entry.get(*key).and_then(Value::as_str))?;
                        Some(ChildRef {
                            list: list.clone(),
                            name: name.to_owned(),
                            item: item.to_owned(),
                            path: String::new(),
                        })
                    })
                    .collect();
                if !named.is_empty() && named.len() == entries.len() {
                    whole.push(list.clone());
                }
                children.extend(named);
            }
            for list in whole {
                map.remove(&list);
            }
        }
        Self {
            path: String::new(),
            kind: value.get("kind").and_then(Value::as_str).map(str::to_owned),
            cursor: next_cursor(value),
            fields,
            children,
        }
    }

    /// The read of the resource at `path`, its children addressed under it.
    fn at(mut self, path: &str, kind: Option<&str>) -> Self {
        for child in &mut self.children {
            child.path = format!("{path}/{}", path_name(&child.item));
        }
        path.clone_into(&mut self.path);
        if self.kind.is_none() {
            self.kind = kind.map(str::to_owned);
        }
        self
    }

    /// The least text that says it: markdown stays markdown, a document's
    /// blocks become markdown with their ids, children become a list, and
    /// anything else compact JSON.
    fn render(&self) -> String {
        match &self.fields {
            Value::String(text) => text.clone(),
            Value::Object(fields) => {
                let markdown = fields
                    .get("markdown")
                    .or_else(|| {
                        fields
                            .get("content")
                            .and_then(|content| content.get("markdown"))
                    })
                    .and_then(Value::as_str);
                if let Some(markdown) = markdown {
                    // A note's read keeps its title beside its body.
                    return match fields.get("title").and_then(Value::as_str) {
                        Some(title) if fields.contains_key("markdown") => {
                            format!("# {title}\n{markdown}")
                        }
                        _ => markdown.to_owned(),
                    };
                }
                if let Some(Value::Array(blocks)) = fields.get("blocks") {
                    let lines: Vec<String> = blocks.iter().filter_map(block_line).collect();
                    if !lines.is_empty() || blocks.is_empty() {
                        let mut text = String::new();
                        if let Some(title) = fields.get("title").and_then(Value::as_str) {
                            let _ = writeln!(text, "# {title}");
                        }
                        text.push_str(&lines.join("\n"));
                        return text;
                    }
                }
                if let Some(list) = self.children_list() {
                    return list;
                }
                strip_blank(&self.fields).to_string()
            }
            Value::Null => String::new(),
            other => strip_blank(other).to_string(),
        }
    }

    /// The children, one line each under the list they are in: the name and
    /// the item to read it by.
    fn children_list(&self) -> Option<String> {
        let mut text = String::new();
        let mut list = None;
        for child in &self.children {
            if list != Some(&child.list) {
                let _ = writeln!(text, "{}:", child.list);
                list = Some(&child.list);
            }
            let _ = writeln!(text, "  {}  ({})", child.name, child.item);
        }
        (!text.is_empty()).then(|| text.trim_end().to_owned())
    }
}

/// What an invocation's final update says, as an agent reads it.
fn outcome(value: &Value) -> Reply {
    let state = value
        .pointer("/update/state")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut text = match state {
        "succeeded" => {
            let mut text = String::from("ok");
            let output = value.pointer("/update/output");
            if let Some(result) = output
                .and_then(|output| output.get("value"))
                .map(render_value)
                .filter(|result| !result.is_empty())
            {
                text.push('\n');
                text.push_str(&result);
            }
            for resource in output
                .and_then(|output| output.get("resources"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .take(8)
            {
                let _ = write!(text, "\n-> {resource}");
            }
            text
        }
        "failed" => {
            let code = value
                .pointer("/update/error/code")
                .and_then(Value::as_str)
                .unwrap_or("failed");
            let message = value
                .pointer("/update/error/message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            format!("failed: {code}: {message}")
        }
        "cancelled" => "cancelled".to_owned(),
        "item" | "progress" => {
            let id = value
                .get("invocation_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut text = format!("pending {id}: still running");
            // A run in a shell says what it printed so far as it prints it.
            if let Some(tail) = value
                .pointer("/update/value/tail")
                .and_then(Value::as_str)
                .filter(|tail| !tail.trim().is_empty())
            {
                let total = value
                    .pointer("/update/value/total")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                let _ = write!(
                    text,
                    "; its output so far ({total} bytes), newest last:\n{}",
                    tail.trim_end()
                );
            }
            let _ = write!(
                text,
                "\ntilo_status {{invocation_id}} says how it went; {{\"cancel\": true}} stops it"
            );
            text
        }
        _ => {
            let id = value
                .get("invocation_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            format!(
                "pending {id}: not finished yet; tilo_status {{invocation_id}} says how it went"
            )
        }
    };
    if let Some(adjusted) = value.get("adjusted").and_then(Value::as_str) {
        let _ = write!(text, "\n(adjusted: {adjusted})");
    }
    Reply {
        failed: state != "succeeded",
        text,
        structured: None,
    }
}

/// The output's cursor to continue a read from, when the app gave one.
fn next_cursor(value: &Value) -> Option<String> {
    let found = ["nextCursor", "next_cursor"]
        .iter()
        .find_map(|key| value.get(*key))?;
    match found {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// What an app name names.
enum AppRef {
    /// A running publisher of the app: its newest window, the window the
    /// path named (`browser@4711`, `pinned`), or the host's own service
    /// answering for it (`desktop`).
    Running {
        app_id: String,
        provider: String,
        pinned: bool,
    },
    /// `opens` when this caller may start it.
    Installed { app_id: String, opens: bool },
}

/// An app installed for the workspace, as the workspace lists it.
struct InstalledApp {
    app_id: String,
    label: String,
    /// Whether this caller may open it: the workspace names an app it may
    /// not open without the action.
    opens: bool,
}

/// A directory read once per call.
type Levels = HashMap<String, Level>;

/// A path, resolved against the live directory.
struct Resolved {
    app_id: String,
    provider: String,
    /// Written back as a path: the app and the names down to the target.
    path: String,
    /// The target resource; `None` for the app itself.
    node: Option<Node>,
    /// Actions on the target (the app's root actions for the app itself).
    actions: Vec<ActionDescriptor>,
    /// Its children (the app's resources for the app itself).
    children: Vec<Node>,
    /// The nodes this resolution passed through, root first.
    chain: Vec<Node>,
    /// The directory levels the resolution read, which what the call does
    /// next (finding the action that reads the target) reads again from
    /// here rather than asking the app twice.
    levels: Levels,
    /// The directory revision the target was found at.
    revision: String,
}

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// One directory level: the resources under `parent` and their actions,
    /// over as many pages as a bounded read takes.
    async fn level(
        &self,
        provider: &str,
        parent: &str,
        levels: &mut Levels,
    ) -> Result<Level, String> {
        if let Some(read) = levels.get(parent) {
            return Ok(read.clone());
        }
        // A level is one snapshot: every page of it at one revision. When the
        // app's directory moves on between pages (its document changed, or an
        // invocation refreshed it), what was read belongs to the old revision
        // and the read starts again from the first page at the new one, for
        // as long as an action is waited for, pausing longer each time a
        // directory that keeps changing is read again.
        let deadline = tokio::time::Instant::now()
            .checked_add(std::time::Duration::from_millis(DEFAULT_WAIT_MS));
        let mut rows = Vec::new();
        let mut reads: u32 = 0;
        let mut read_at = String::new();
        'snapshot: loop {
            let pause = restart_pause(reads);
            if !pause.is_zero()
                && let Some(deadline) = deadline
            {
                let until = tokio::time::Instant::now()
                    .checked_add(pause)
                    .map_or(deadline, |until| until.min(deadline));
                tokio::time::sleep_until(until).await;
            }
            reads = reads.saturating_add(1);
            rows.clear();
            let mut cursor = Vec::new();
            let mut revision = None;
            for _ in 0..LEVEL_PAGES {
                let page = self
                    .ask(CatalogueQuery {
                        provider: Some(provider.to_owned()),
                        parent_resource: (!parent.is_empty()).then(|| parent.to_owned()),
                        cursor: std::mem::take(&mut cursor),
                        revision: revision.take(),
                        max_results: PAGE_ROWS,
                        limits: Some(CatalogueLimits {
                            max_bytes: Some(65536),
                            max_work: Some(512),
                            max_depth: Some(16),
                        }),
                        ..CatalogueQuery::default()
                    })
                    .await?;
                if page.stale_cursor {
                    if deadline.is_some_and(|deadline| tokio::time::Instant::now() < deadline) {
                        continue 'snapshot;
                    }
                    return Err(
                        "the app's directory kept changing while it was read; try again".into(),
                    );
                }
                rows.extend(page.rows);
                read_at.clone_from(&page.revision);
                if page.exhausted || page.next_cursor.is_empty() {
                    break 'snapshot;
                }
                revision = Some(page.revision);
                cursor = page.next_cursor;
            }
            break;
        }
        let mut read = level_of(&rows, parent);
        read.revision = read_at;
        levels.insert(parent.to_owned(), read.clone());
        Ok(read)
    }

    /// Whether the resource `resource` a provider publishes answers a read of itself: whether
    /// its directory lists a read on the resource itself. A resource that does not is read by
    /// the resource its directory lists it under, which takes it as its `item` ([`Self::reader`]).
    pub(super) async fn level_reads(
        &self,
        provider: &Addressed,
        resource: &str,
    ) -> Result<bool, String> {
        let mut levels: Levels = HashMap::new();
        let level = self
            .level(&provider.provider, resource, &mut levels)
            .await?;
        Ok(reads_action(&level.own).is_some())
    }

    /// The resources `provider` lists under `resource`, as an agent names them: its
    /// directory level there, one row per resource (`{id, kind, key, name}`), the key
    /// being what a read of it takes as its `item`.
    pub(super) async fn children_rows(
        &self,
        provider: &Addressed,
        resource: &str,
    ) -> Result<Vec<Value>, String> {
        let mut levels: Levels = HashMap::new();
        let level = self
            .level(&provider.provider, resource, &mut levels)
            .await?;
        Ok(level
            .children
            .iter()
            .map(|node| {
                json!({
                    "id": node.id,
                    "kind": node.kind,
                    "key": node.key,
                    "name": node.label,
                })
            })
            .collect())
    }

    /// The app `name` means: a running one by its id or last part, else an
    /// installed one the workspace offers to open.
    async fn app_ref(&self, name: &str) -> Result<AppRef, String> {
        let (name, instance) = match name.rsplit_once('@') {
            Some((app, instance)) if !instance.trim().is_empty() => {
                (app.trim(), Some(instance.trim()))
            }
            _ => (name, None),
        };
        let (_, running) = self.providers().await?;
        if let Some(instance) = instance {
            return pinned_window(&running, name, instance);
        }
        // A record a device serves whoever has it open (a host serving its
        // workspace's records) is the app, live, as a window of it is.
        let is_window = |provider: &str| {
            provider.parse::<u64>().is_ok() || tilo_actions::remote::is_record_provider(provider)
        };
        let ids_where = |window: bool| {
            running
                .iter()
                .filter(|(provider, _)| is_window(provider) == window)
                .filter_map(|(_, app)| app["app_id"].as_str())
                .collect::<Vec<_>>()
        };
        let newest = |app_id: &str, window: bool| {
            running
                .iter()
                .rev()
                .filter(|(provider, _)| is_window(provider) == window)
                .find(|(_, app)| app["app_id"].as_str() == Some(app_id))
                .map(|(provider, _)| provider.clone())
        };
        let windows = ids_where(true);
        match page::app_named(name, &windows) {
            Ok(app_id) => {
                let provider =
                    newest(&app_id, true).ok_or_else(|| page::not_running(&app_id, &windows))?;
                return Ok(AppRef::Running {
                    app_id,
                    provider,
                    pinned: false,
                });
            }
            Err(error) if error.contains("names several") => return Err(error),
            Err(_) => {}
        }
        // The host's own services (the desktop's layout, the workspace's
        // activity) answer for app ids no app installs: the app itself,
        // when it is installed, is what its name opens.
        let installed = self.installed().await;
        let services = ids_where(false);
        let service = page::app_named(name, &services).ok().filter(|app_id| {
            installed
                .as_ref()
                .is_ok_and(|installed| !installed.iter().any(|app| app.app_id == *app_id))
                || installed.is_err()
        });
        if let Some(app_id) = service
            && let Some(provider) = newest(&app_id, false)
        {
            return Ok(AppRef::Running {
                app_id,
                provider,
                pinned: false,
            });
        }
        // An installed list that could not be read is said, never taken for
        // an empty one: the agent would conclude the app does not exist.
        let installed = installed.map_err(|reason| {
            format!(
                "no running app is named {name}, and the installed apps could not be read: {reason}"
            )
        })?;
        let ids = installed
            .iter()
            .map(|app| app.app_id.as_str())
            .collect::<Vec<_>>();
        match page::app_named(name, &ids) {
            Ok(app_id) => {
                let opens = installed
                    .iter()
                    .any(|app| app.app_id == app_id && app.opens);
                Ok(AppRef::Installed { app_id, opens })
            }
            Err(error) if error.contains("names several") => Err(error),
            // The name is no app's and no tail of one: a word an agent
            // guessed (`host` for the Browser, `so.tilo.browser`) is the
            // likeliest cause, so the running apps are named as they are to
            // be addressed.
            Err(_) => Err(format!(
                "no app {name}; name a running app by its id or last part, e.g. {}. Running {}; installed {}",
                running
                    .iter()
                    .find_map(|(_, app)| app["app_id"].as_str())
                    .unwrap_or("<app_id>"),
                list_or_none(&app_names(
                    &running
                        .iter()
                        .filter_map(|(_, app)| app["app_id"].as_str().map(str::to_owned))
                        .collect::<Vec<_>>()
                )),
                list_or_none(&app_names(
                    &installed
                        .iter()
                        .map(|app| app.app_id.clone())
                        .collect::<Vec<_>>()
                )),
            )),
        }
    }

    /// The apps installed for this workspace, with their labels and whether
    /// this caller may open them. The workspace answers them by their kind,
    /// whole, wherever they fall among its other rows (governance, shortcuts),
    /// so a workspace with many of those still lists every app.
    async fn installed(&self) -> Result<Vec<InstalledApp>, String> {
        let provider = tilo_actions::remote::workspace_provider(&self.workspace);
        let mut apps: Vec<InstalledApp> = Vec::new();
        let mut cursor = Vec::new();
        let mut revision = None;
        for _ in 0..INSTALLED_PAGES {
            let page = self
                .ask(CatalogueQuery {
                    provider: Some(provider.clone()),
                    resource_name: Some(tilo_actions::remote::APPLICATION_RESOURCE_KIND.to_owned()),
                    max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
                    cursor: std::mem::take(&mut cursor),
                    revision: revision.take(),
                    ..CatalogueQuery::default()
                })
                .await?;
            for row in page.rows {
                let Some(app_id) = row
                    .resource
                    .id
                    .strip_prefix(tilo_actions::remote::APPLICATION_RESOURCE_PREFIX)
                    .filter(|app_id| !app_id.starts_with("host:"))
                else {
                    continue;
                };
                // The workspace offers an action on an app (open, or close
                // while it runs) only to a caller that may use it, and names
                // one it may not without any: a running app shows only its
                // close, and is still this caller's to open.
                let opens = row.action.is_some();
                match apps.iter_mut().find(|app| app.app_id == app_id) {
                    Some(app) => app.opens |= opens,
                    None => apps.push(InstalledApp {
                        app_id: app_id.to_owned(),
                        label: row.resource.label.clone(),
                        opens,
                    }),
                }
            }
            if page.exhausted || page.next_cursor.is_empty() {
                break;
            }
            cursor = page.next_cursor;
            revision = Some(page.revision);
        }
        Ok(apps)
    }

    /// The resources an app's path starts from: its top-level resources and
    /// the resources inside its containers (a notebook's pages are the app's
    /// own), with the containers' own actions (`notes#new-page`).
    async fn app_children(&self, provider: &str, levels: &mut Levels) -> Result<Level, String> {
        let mut root = self.level(provider, "", levels).await?;
        let containers: Vec<Node> = root
            .children
            .iter()
            .filter(|node| node.container)
            .take(ROOT_PEEK)
            .cloned()
            .collect();
        for node in containers {
            let inside = self.level(provider, &node.id, levels).await?;
            for child in inside.children {
                if !root.children.iter().any(|seen| seen.id == child.id) {
                    root.children.push(child);
                }
            }
            root.own.extend(inside.own);
        }
        let all = root.children.clone();
        root.children
            .retain(|node| !all.iter().any(|owner| owner.beside(node)));
        Ok(root)
    }

    /// The resources under `parent` that `name` names (by label or kind),
    /// asked of the app whole: one note among a notebook's thousands costs
    /// the rows naming it, not a page of every note. `None` when the app
    /// cannot say (a host from before parents were named answers about its
    /// top level), and the level is listed instead.
    async fn named(
        &self,
        provider: &str,
        parent: &str,
        name: &str,
    ) -> Result<Option<Level>, String> {
        let name = name.trim();
        if name.is_empty() || name.len() > NAME_BYTES {
            return Ok(None);
        }
        let Ok(page) = self
            .ask(CatalogueQuery {
                provider: Some(provider.to_owned()),
                parent_resource: (!parent.is_empty()).then(|| parent.to_owned()),
                resource_name: Some(name.to_owned()),
                max_results: PAGE_ROWS,
                ..CatalogueQuery::default()
            })
            .await
        else {
            return Ok(None);
        };
        if !page.exhausted
            || page.stale_cursor
            || page.rows.iter().any(|row| row.parent_resource != parent)
        {
            return Ok(None);
        }
        let mut named = level_of(&page.rows, parent);
        named.revision = page.revision;
        Ok(Some(named))
    }

    /// What `name` may mean at the app's level, asked by name: the app's top
    /// level and, inside each of its containers, what `name` names there,
    /// less what another of them owns; `None` when one cannot be asked.
    async fn named_at_app(
        &self,
        provider: &str,
        name: &str,
        levels: &mut Levels,
    ) -> Result<Option<Level>, String> {
        let root = self.level(provider, "", levels).await?;
        let mut revision = root.revision.clone();
        let mut pool = root.children.clone();
        for container in root
            .children
            .iter()
            .filter(|node| node.container)
            .take(ROOT_PEEK)
        {
            let Some(inside) = self.named(provider, &container.id, name).await? else {
                return Ok(None);
            };
            revision = inside.revision;
            for child in inside.children {
                if !pool.iter().any(|seen| seen.id == child.id) {
                    pool.push(child);
                }
            }
        }
        let all = pool.clone();
        pool.retain(|node| !all.iter().any(|owner| owner.beside(node)));
        Ok(Some(Level {
            own: Vec::new(),
            children: pool,
            revision,
        }))
    }

    /// The resource `segment` names under `parent` (the app itself for
    /// `None`), and the resources it was named among. A name only one
    /// resource there carries is asked of the app by name; anything else (a
    /// key, the start of a name, a name several carry, one that is not
    /// there) is decided on the whole level, as listing it shows. With them
    /// comes the directory revision it was found at.
    async fn step(
        &self,
        provider: &str,
        app: &str,
        parent: Option<&Node>,
        segment: &str,
        levels: &mut Levels,
        forms: Option<&ProgramForms>,
    ) -> Result<(Node, Level), String> {
        let asked = match parent {
            Some(owner) if !levels.contains_key(&owner.id) => {
                self.named(provider, &owner.id, segment).await?
            }
            None => self.named_at_app(provider, segment, levels).await?,
            Some(_) => None,
        };
        if let Some(among) = asked {
            let mut named = among
                .children
                .iter()
                .filter(|node| node.answers_to(segment) == Some(0));
            if let (Some(only), None) = (named.next(), named.next()) {
                let only = only.clone();
                return Ok((only, among));
            }
        }
        let here = match parent {
            Some(owner) => self.level(provider, &owner.id, levels).await?,
            None => self.app_children(provider, levels).await?,
        };
        let found = match pick(segment, &here.children, forms)? {
            Some(node) => node.clone(),
            None => self
                .deep(provider, segment, parent, levels, forms)
                .await?
                .ok_or_else(|| {
                    format!(
                        "no {segment} in {app}; it has: {}",
                        list_names(
                            &here
                                .children
                                .iter()
                                .map(|node| node.name(&here.children))
                                .collect::<Vec<_>>()
                        )
                    )
                })?,
        };
        Ok((found, here))
    }

    /// A resource `name` names somewhere below `within` (or anywhere in the
    /// app), found among the nodes this call has read and then by reading
    /// the containers it has not: a note's body is published beside the
    /// note rather than inside it, and its key says whose it is.
    async fn deep(
        &self,
        provider: &str,
        name: &str,
        within: Option<&Node>,
        levels: &mut Levels,
        forms: Option<&ProgramForms>,
    ) -> Result<Option<Node>, String> {
        for _ in 0..=DEEP_READS {
            let pool: Vec<Node> = levels
                .values()
                .flat_map(|level| level.children.iter())
                .filter(|node| {
                    within.is_none_or(|owner| {
                        node.key.contains(owner.token()) || Self::below(node, owner, levels)
                    }) && rank(name, node, forms).is_some()
                })
                .cloned()
                .collect();
            if let Some(found) = pick(name, &pool, forms)? {
                return Ok(Some(found.clone()));
            }
            let unread = levels
                .values()
                .flat_map(|level| level.children.iter())
                .find(|node| node.container && !levels.contains_key(&node.id))
                .map(|node| node.id.clone());
            let Some(unread) = unread else {
                return Ok(None);
            };
            self.level(provider, &unread, levels).await?;
        }
        Ok(None)
    }

    /// Whether `node`'s parent chain, as far as this call has read it,
    /// reaches `owner`.
    fn below(node: &Node, owner: &Node, levels: &Levels) -> bool {
        let mut parent = node.parent.clone();
        for _ in 0..8 {
            if parent.is_empty() {
                return false;
            }
            if parent == owner.id {
                return true;
            }
            let Some(next) = levels
                .values()
                .flat_map(|level| level.children.iter())
                .find(|candidate| candidate.id == parent)
            else {
                return false;
            };
            parent.clone_from(&next.parent);
        }
        false
    }

    /// Resolve a whole path. A segment written the way a program types a type (`tasks.Task`)
    /// is read as that type first: it names the resource whose published kind the listing
    /// prints that name for. Only a segment written that way is asked of the task's programs,
    /// so a path of titles (`notes/Todo`) does not start a task's host to read a name already
    /// said; a form no app declares falls back to the plain reading, where it may be a title.
    ///
    /// With the resolution come the forms it was read by (`None` when it was read plainly), so
    /// a caller that shows what it found names the type the same way without asking again.
    async fn resolve(
        &self,
        parsed: &ParsedPath,
    ) -> Result<(Result<Resolved, String>, Option<ProgramForms>), String> {
        if parsed
            .segments
            .iter()
            .any(|segment| is_program_form(segment))
            && let Some(forms) = self.program_forms().await
        {
            let by_type = self.resolve_with(parsed, Some(&forms)).await?;
            // A path of segments resolves to a resource only when it walked every one: an
            // empty chain is a name no resource answered.
            if by_type.as_ref().is_ok_and(|found| !found.chain.is_empty()) {
                return Ok((by_type, Some(forms)));
            }
        }
        Ok((self.resolve_with(parsed, None).await?, None))
    }

    /// [`Self::resolve`] reading the names a path segment may go by as `forms` says they are
    /// written: a segment names a type by the name a listing prints for it (`tasks.Task`),
    /// which `forms` reads back as its kind.
    async fn resolve_with(
        &self,
        parsed: &ParsedPath,
        forms: Option<&ProgramForms>,
    ) -> Result<Result<Resolved, String>, String> {
        match self.app_ref(&parsed.app).await? {
            AppRef::Running {
                app_id,
                provider,
                pinned: true,
            } => Ok(Ok(self
                .resolve_in(parsed, app_id, provider, true, forms)
                .await?)),
            AppRef::Running {
                app_id, provider, ..
            } => Ok(Ok(self
                .resolve_published(parsed, app_id, provider, true, forms)
                .await?)),
            AppRef::Installed { opens, .. } => Ok(Err(Self::not_running_note(&parsed.app, opens))),
        }
    }

    /// [`Self::resolve_in`] the newest instance `newest` of `app_id`, or, when
    /// it has published nothing for the path, the app's older instances in
    /// turn. A path names the app, not one of its processes: an instance
    /// lists as running a moment before it publishes its directory, and must
    /// not hide an older one that has (an agent saw an app's actions, and
    /// the next call found none).
    async fn resolve_published(
        &self,
        parsed: &ParsedPath,
        app_id: String,
        newest: String,
        list: bool,
        forms: Option<&ProgramForms>,
    ) -> Result<Resolved, String> {
        let published = |found: &Resolved| {
            !found.chain.is_empty() || !found.actions.is_empty() || !found.children.is_empty()
        };
        let first = self
            .resolve_in(parsed, app_id.clone(), newest.clone(), list, forms)
            .await;
        if first.as_ref().is_ok_and(published) {
            return first;
        }
        // Only then are the running apps asked again, for the older instances.
        let older: Vec<String> = self
            .providers()
            .await?
            .1
            .iter()
            .rev()
            .filter(|(_, app)| app["app_id"].as_str() == Some(app_id.as_str()))
            .map(|(peer, _)| peer.clone())
            .filter(|peer| *peer != newest)
            .collect();
        for provider in older {
            let resolved = self
                .resolve_in(parsed, app_id.clone(), provider, list, forms)
                .await;
            if resolved.as_ref().is_ok_and(published) {
                return resolved;
            }
        }
        first
    }

    /// Resolve the segments of `parsed` in the running app `app_id`, whose
    /// instance is `provider`. With `list`, what the target holds and the
    /// actions on it are read too; without, a resource on the path is only
    /// found (an action named on it is resolved by its address). `forms` is how
    /// a path segment may name a type the way a program types it
    /// (`tasks.Task`), as a listing names it.
    async fn resolve_in(
        &self,
        parsed: &ParsedPath,
        app_id: String,
        provider: String,
        list: bool,
        forms: Option<&ProgramForms>,
    ) -> Result<Resolved, String> {
        let app = parsed.app.to_lowercase();
        let mut levels = Levels::new();
        let mut chain: Vec<Node> = Vec::new();
        // What each resource on the path was named among, and the revision
        // the last of them was found at.
        let mut among: Vec<Vec<Node>> = Vec::new();
        let mut revision = String::new();
        for segment in &parsed.segments {
            let (found, siblings) = self
                .step(&provider, &app, chain.last(), segment, &mut levels, forms)
                .await?;
            chain.push(found);
            among.push(siblings.children);
            revision = siblings.revision;
        }
        let mut here = match chain.last() {
            Some(_) if !list => Level::default(),
            Some(node) => self.level(&provider, &node.id, &mut levels).await?,
            None => {
                let mut root = self.app_children(&provider, &mut levels).await?;
                // A host service (the desktop's layout, on `desktop:session`)
                // publishes everything on its one resource: its actions are
                // the service's own, `desktop#list-windows`.
                if provider.parse::<u64>().is_err()
                    && root.own.is_empty()
                    && let [only] = root.children.as_slice()
                {
                    root.own = only.actions.clone();
                }
                root
            }
        };
        let mut written = app;
        if let Some(owner) = chain.last() {
            // Published beside the resource, not inside it, but its key says
            // whose it is: listed as one of its children.
            let token = owner.token().to_owned();
            let related: Vec<Node> = levels
                .values()
                .flat_map(|level| level.children.iter())
                .filter(|node| {
                    token.len() >= 8
                        && node.id != owner.id
                        && node.key.contains(&token)
                        && !here.children.iter().any(|child| child.id == node.id)
                })
                .cloned()
                .collect();
            here.children.extend(related);
        }
        let mut previous: Option<Node> = None;
        for (node, siblings) in chain.iter().zip(&among) {
            // Named against what it sits among and the resource it sits
            // under, whose title it may share.
            let mut named = siblings.clone();
            named.extend(previous.clone());
            written = format!("{written}/{}", node.name(&named));
            previous = Some(node.clone());
        }
        Ok(Resolved {
            app_id,
            provider,
            path: written,
            node: chain.last().cloned(),
            // A directory that hoists a resource's actions into its parent's
            // listing lists them there, not under the resource.
            actions: if here.own.is_empty() {
                chain
                    .last()
                    .map(|node| node.actions.clone())
                    .unwrap_or_default()
            } else {
                here.own
            },
            children: here.children,
            chain,
            levels,
            revision,
        })
    }

    fn not_running_note(name: &str, opens: bool) -> String {
        if opens {
            format!(
                "{name} is installed but not running; start it with tilo_do {{path: \"{}#open\"}}, or run one of its actions and it starts first",
                path_name(name)
            )
        } else {
            format!(
                "{name} is installed but not running, and {}",
                Self::may_not_open(name)
            )
        }
    }

    /// Why an installed app this agent may not open stays closed, and who
    /// changes that.
    fn may_not_open(name: &str) -> String {
        format!(
            "this agent may not open {name}: the person decides which apps an agent may use in this workspace"
        )
    }
}

/// The ids of the apps `name` names among those `running` and `installed`:
/// its whole id or the name `tilo_ls` lists it by, any case. An error naming
/// the apps there are when it names none, so a search narrowed to an app
/// that is not there does not answer that nothing matches. `unread` is why
/// the installed apps could not be read, when they could not: an app that
/// is not running may then be installed, and the error says why the agent
/// cannot know rather than that there is no such app.
fn searched_apps(
    name: &str,
    running: &[String],
    installed: &[InstalledApp],
    unread: Option<&str>,
) -> Result<Vec<String>, String> {
    let known = running
        .iter()
        .chain(installed.iter().map(|app| &app.app_id));
    let mut meant: Vec<String> = Vec::new();
    let mut all: Vec<String> = Vec::new();
    for app_id in known {
        all.push(app_id.clone());
        let short = app_names(std::slice::from_ref(app_id)).join("");
        if (app_id.eq_ignore_ascii_case(name) || short.eq_ignore_ascii_case(name))
            && !meant.contains(app_id)
        {
            meant.push(app_id.clone());
        }
    }
    if !meant.is_empty() {
        return Ok(meant);
    }
    if let Some(reason) = unread {
        return Err(format!(
            "no running app is named {name}, and the installed apps could not be read: {reason}"
        ));
    }
    let listed = app_names(&all);
    Err(if listed.is_empty() {
        format!("no app {name}: no app is running or installed")
    } else {
        format!("no app {name}; the apps: {}", list_names(&listed))
    })
}

fn app_names(ids: &[String]) -> Vec<String> {
    let mut names: Vec<String> = ids
        .iter()
        .map(|id| id.rsplit('.').next().unwrap_or(id).to_lowercase())
        .collect();
    names.sort();
    names.dedup();
    names
}

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// Answer one agent tool call.
    /// `structured` when the caller asked for typed answers
    /// (`_meta["so.tilo.mcp"].structured`): a read answers its
    /// [`ResourceRead`].
    pub(super) async fn agent_call(&self, name: &str, args: Value, structured: bool) -> Value {
        let result = match name {
            "tilo_ls" => match parse(args) {
                Ok(args) => self.agent_ls(args, structured).await,
                Err(error) => Err(error),
            },
            "tilo_read" => match parse(args) {
                Ok(args) => self.agent_read(args, structured).await,
                Err(error) => Err(error),
            },
            "tilo_do" => match parse(args) {
                Ok(args) => self.agent_do(args, structured).await,
                Err(error) => Err(error),
            },
            "tilo_find" => match parse(args) {
                Ok(args) => self.agent_find(args, structured).await,
                Err(error) => Err(error),
            },
            "tilo_status" => match parse(args) {
                Ok(args) => self.agent_status(&args, structured).await,
                Err(error) => Err(error),
            },
            "tilo" => match parse(args) {
                Ok(args) => Box::pin(self.agent_tilo(args, structured)).await,
                Err(error) => Err(error),
            },
            "tilo_watch" => match parse(args) {
                Ok(args) => self.agent_watch(args).await,
                Err(error) => Err(error),
            },
            _ => Err("Unknown tool".to_owned()),
        };
        text_result(result, structured)
    }

    /// Note the record a call resolved `in`: the one its provider names when
    /// the workspace serves it as a record (`record:<app>:<record>`). A call
    /// whose resource no record provider answers leaves the last one named:
    /// the agent still acts there.
    fn acted_in(&self, resolved: &Resolved) {
        let Some((app, record)) = tilo_actions::remote::record_parts(&resolved.provider) else {
            return;
        };
        if app != resolved.app_id {
            return;
        }
        *self.acted.lock_recover() = Some(super::ActedRecord {
            app: app.to_owned(),
            record: record.to_owned(),
        });
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one listing, its text lines and its typed rows in the order it writes them"
    )]
    async fn agent_ls(&self, args: LsArgs, structured: bool) -> Result<Reply, String> {
        let offset = match args.cursor.as_deref() {
            None | Some("") => 0,
            Some(cursor) => cursor
                .parse::<usize>()
                .map_err(|_| "cursor is not one tilo_ls issued".to_owned())?,
        };
        let limit = args.limit.unwrap_or(LS_LINES).clamp(1, 200);
        let (want_resources, want_actions) = match args.kind.as_deref() {
            None | Some("") => (true, true),
            Some("resources") => (true, false),
            Some("actions") => (false, true),
            Some(other) => return Err(format!("kind is resources or actions, not {other}")),
        };
        let path = args.path.as_deref().map(str::trim).unwrap_or_default();
        if path.is_empty() {
            return self.list_apps(offset, limit, structured).await;
        }
        let parsed = parse_path(path)?;
        // How a program writes what this lists: the listing is the one a program reads, and a
        // path names what it lists the same way, so the path is resolved by it too.
        let forms = self.program_forms().await.unwrap_or_default();
        let resolved = match self.resolve_with(&parsed, Some(&forms)).await? {
            Ok(resolved) => resolved,
            Err(note) => return Ok(Reply::ok(note)),
        };
        self.acted_in(&resolved);
        let called = |action: &ActionDescriptor| forms.called(action, &resolved.app_id);
        if let Some(action) = &parsed.action {
            let (named_at, found) = self.pick_action_in(action, &resolved).await?;
            let mut reply = Reply::ok(action_page(&named_at, &found, called(&found)));
            if structured {
                reply.structured = Some(json!({
                    "path": named_at,
                    "kind": resolved
                        .node
                        .as_ref()
                        .map_or(resolved.app_id.as_str(), |node| {
                            forms.kind(&node.kind).unwrap_or(&node.kind)
                        }),
                    "actions": [action_row(&found)],
                    "rows": [],
                }));
            }
            return Ok(reply);
        }
        let mut lines: Vec<String> = Vec::new();
        let mut rows: Vec<Value> = Vec::new();
        if want_resources {
            let mut named: Vec<Node> = resolved.children.clone();
            named.extend(resolved.node.clone());
            for child in &resolved.children {
                // A resource a program passes as a value goes by a reference, as an answer's.
                let mut line = self
                    .listed_reference(&child.kind, &child.id)
                    .map(|reference| format!("{reference}  "))
                    .unwrap_or_default();
                line.push_str(&child.name(&named));
                if child.container {
                    line.push('/');
                }
                // What the resource is, by the name a program types its type with
                // (`tasks.Task`): the one name a listing prints and a path resolves by, so a
                // row read off the listing walks back to this very resource. Never the
                // published kind (`tl.<app>.<Type>`), which no program writes.
                let _ = write!(line, "  {}", forms.kind(&child.kind).unwrap_or(&child.kind));
                if !child.actions.is_empty() {
                    let _ = write!(line, "  {}", actions_note(&child.actions, &called));
                }
                let name = child.name(&named);
                rows.push(json!({
                    "path": format!("{}/{name}", resolved.path),
                    "name": name,
                    "label": child.label,
                    "kind": forms.kind(&child.kind).unwrap_or(&child.kind),
                    "has_children": child.container,
                    "actions": action_rows(&child.actions),
                }));
                lines.push(line);
            }
        }
        if want_actions {
            lines.extend(
                resolved
                    .actions
                    .iter()
                    .map(|action| action_line(action, called(action))),
            );
        }
        let total = lines.len();
        let mut text = resolved.path.clone();
        // What this is, by the same name its own row here would give it.
        if let Some(node) = &resolved.node {
            let _ = write!(text, "  ({})", forms.kind(&node.kind).unwrap_or(&node.kind));
        } else {
            let _ = write!(text, "  ({})", resolved.app_id);
        }
        for line in lines.into_iter().skip(offset).take(limit) {
            text.push('\n');
            text.push_str(&line);
        }
        let end = offset.saturating_add(limit);
        let next = (end < total).then(|| end.to_string());
        if let Some(next) = &next {
            let _ = write!(
                text,
                "\n… {} more: cursor \"{next}\"",
                total.saturating_sub(end)
            );
        } else if total == 0 {
            text.push_str("\n(nothing here)");
        }
        let mut reply = Reply::ok(text);
        if structured {
            let kind = resolved
                .node
                .as_ref()
                .map_or(resolved.app_id.as_str(), |node| {
                    forms.kind(&node.kind).unwrap_or(&node.kind)
                })
                .to_owned();
            let mut answer = json!({
                "path": resolved.path,
                "kind": kind,
                "actions": action_rows(&resolved.actions),
                "rows": rows,
            });
            if let (Some(fields), Some(next)) = (answer.as_object_mut(), next) {
                fields.insert("next_cursor".into(), json!(next));
            }
            reply.structured = Some(answer);
        }
        Ok(reply)
    }

    /// The apps: running ones first, then the installed ones to open. An
    /// installed list that could not be read is said, with why: read as
    /// empty it would tell the agent there are no apps.
    async fn list_apps(
        &self,
        offset: usize,
        limit: usize,
        structured: bool,
    ) -> Result<Reply, String> {
        let (_, running) = self.providers().await?;
        let mut lines: Vec<String> = Vec::new();
        let mut rows: Vec<Value> = Vec::new();
        let mut running_ids: Vec<String> = Vec::new();
        for (_, app) in &running {
            let Some(app_id) = app["app_id"].as_str() else {
                continue;
            };
            if !running_ids.iter().any(|seen| seen == app_id) {
                running_ids.push(app_id.to_owned());
            }
        }
        let (installed, unread) = match self.installed().await {
            Ok(installed) => (installed, None),
            Err(reason) => (Vec::new(), Some(reason)),
        };
        for app_id in &running_ids {
            let label = installed
                .iter()
                .find(|app| app.app_id == *app_id)
                .map_or("", |app| app.label.as_str());
            // A host service (the desktop's layout) runs with the session,
            // and has no window.
            let windowed = running.iter().any(|(provider, app)| {
                (provider.parse::<u64>().is_ok()
                    || tilo_actions::remote::is_record_provider(provider))
                    && app["app_id"].as_str() == Some(app_id)
            });
            let name = app_names(std::slice::from_ref(app_id)).join("");
            let state = if windowed { "running" } else { "service" };
            lines.push(format!("{name}  {label}  {state}"));
            rows.push(json!({
                "path": name,
                "name": name,
                "label": label,
                "kind": "app",
                "state": state,
                "has_children": true,
                "actions": [],
            }));
        }
        let mut unopened = false;
        for app in &installed {
            if !running_ids.contains(&app.app_id) {
                let state = if app.opens {
                    "installed"
                } else {
                    unopened = true;
                    "installed, not yours to open"
                };
                let name = app_names(std::slice::from_ref(&app.app_id)).join("");
                lines.push(format!("{name}  {}  {state}", app.label));
                rows.push(json!({
                    "path": name,
                    "name": name,
                    "label": app.label,
                    "kind": "app",
                    "state": state,
                    "has_children": true,
                    "actions": [],
                }));
            }
        }
        let total = lines.len();
        let mut text = String::from("apps");
        for line in lines.into_iter().skip(offset).take(limit) {
            text.push('\n');
            text.push_str(&line);
        }
        let end = offset.saturating_add(limit);
        let next = (end < total).then(|| end.to_string());
        if let Some(next) = &next {
            let _ = write!(
                text,
                "\n… {} more: cursor \"{next}\"",
                total.saturating_sub(end)
            );
        }
        if let Some(reason) = unread {
            let _ = write!(text, "\nthe installed apps could not be read: {reason}");
        } else if total == 0 {
            text.push_str("\nnone: no app runs in this workspace and none is installed");
        }
        if unopened {
            text.push_str("\nthe person decides which apps an agent may open in this workspace");
        }
        let mut reply = Reply::ok(text);
        if structured {
            let mut answer = json!({"path":"apps","kind":"apps","actions":[],"rows":rows});
            if let (Some(fields), Some(next)) = (answer.as_object_mut(), next) {
                fields.insert("next_cursor".into(), json!(next));
            }
            reply.structured = Some(answer);
        }
        Ok(reply)
    }

    async fn agent_read(&self, args: ReadArgs, structured: bool) -> Result<Reply, String> {
        let parsed = parse_path(&args.path)?;
        if parsed.action.is_some() {
            return Err("tilo_read reads a resource; run an action with tilo_do".into());
        }
        let (mut resolved, forms) = match self.resolve(&parsed).await? {
            (Ok(resolved), forms) => (resolved, forms),
            (Err(note), _) => return Ok(Reply::ok(note)),
        };
        self.acted_in(&resolved);
        let provider = resolved.provider.clone();
        let mut levels = std::mem::take(&mut resolved.levels);
        let Some((owner, reader)) = self.reader(&resolved, &mut levels).await? else {
            let mut text = format!(
                "{} has no read action; its actions: {}",
                resolved.path,
                list_names(
                    &resolved
                        .actions
                        .iter()
                        .map(|action| action.address.action.clone())
                        .collect::<Vec<_>>()
                )
            );
            if !resolved.children.is_empty() {
                text.push_str("; tilo_ls lists what is inside");
            }
            return Ok(Reply::failed(text));
        };
        // A cursor this tool issued for the rest of a cut answer reads the
        // same page again and answers from where the cut was; any other is
        // the app's own.
        let (already, app_cursor) = match args.cursor.filter(|cursor| !cursor.is_empty()) {
            None => (0, None),
            Some(cursor) => parse_read_cursor(&cursor).unwrap_or((0, Some(cursor))),
        };
        let arguments = read_arguments(&resolved, &owner, &reader, app_cursor.as_deref())?;
        let value = Box::pin(self.run(
            RemoteActionTarget {
                provider,
                address: reader.address.clone(),
            },
            arguments,
            tilo_actions::InvocationId::new(),
            None,
            std::time::Duration::from_millis(DEFAULT_WAIT_MS),
            &mut None,
        ))
        .await?;
        let answered = outcome(&value);
        if answered.failed {
            return Ok(answered);
        }
        let output = value
            .pointer("/update/output/value")
            .cloned()
            .unwrap_or_default();
        let mut read = ResourceRead::of(&output).at(
            &resolved.path,
            resolved.node.as_ref().map(|node| node.kind.as_str()),
        );
        // A program reads what a listing showed, so the type it reads is the one the listing
        // named (`tasks.Task`), whether the app's own answer gave it or the node's kind did.
        if let (Some(forms), Some(kind)) = (forms.as_ref(), read.kind.clone())
            && let Some(form) = forms.kind(&kind)
        {
            read.kind = Some(form.to_owned());
        }
        if structured {
            // Typed, and paged by the app alone: the characters a text read
            // is cut at mean nothing to a program.
            let mut reply = Reply::ok(String::new());
            reply.structured =
                Some(serde_json::to_value(&read).map_err(|error| error.to_string())?);
            return Ok(reply);
        }
        Ok(Reply::ok(read_answer(
            &read,
            already,
            app_cursor.as_deref(),
            args.max_chars,
        )))
    }

    /// The action that reads `resolved`, and the id of the resource that
    /// offers it: the target's own, or its nearest ancestor's, which takes
    /// the target as its `item`.
    async fn reader(
        &self,
        resolved: &Resolved,
        levels: &mut Levels,
    ) -> Result<Option<(String, ActionDescriptor)>, String> {
        if let Some(found) = reads_action(&resolved.actions) {
            let owner = resolved
                .node
                .as_ref()
                .map_or_else(|| found.address.resource.clone(), |node| node.id.clone());
            return Ok(Some((owner, found)));
        }
        for ancestor in resolved.chain.iter().rev().skip(1) {
            let level = self.level(&resolved.provider, &ancestor.id, levels).await?;
            if let Some(found) = reads_action(&level.own) {
                return Ok(Some((ancestor.id.clone(), found)));
            }
        }
        // The app's root resources: a notebook reads its pages.
        let root = self.level(&resolved.provider, "", levels).await?;
        for node in &root.children {
            let level = self.level(&resolved.provider, &node.id, levels).await?;
            if let Some(found) = reads_action(&level.own).or_else(|| reads_action(&node.actions)) {
                return Ok(Some((node.id.clone(), found)));
            }
        }
        Ok(None)
    }

    async fn agent_do(&self, args: DoArgs, structured: bool) -> Result<Reply, String> {
        let wait_ms = args.wait_ms;
        // A typed caller follows an invocation that waits for the person by
        // its id, so the call names one even when the caller did not: the same
        // id asks the same invocation again, and never runs it twice.
        let invocation_id = match (structured, args.invocation_id) {
            (true, None) => Some(tilo_actions::InvocationId::new().0.to_string()),
            (_, invocation_id) => invocation_id,
        };
        match (args.path, args.calls) {
            (Some(path), None) => {
                let _running = self.hold_invocation(invocation_id.as_deref()).await;
                Box::pin(self.do_one(&path, args.args, invocation_id, wait_ms, structured)).await
            }
            (None, Some(calls)) => {
                if calls.is_empty() || calls.len() > MAX_CALLS {
                    return Err(format!("calls must hold 1 to {MAX_CALLS} steps"));
                }
                let _running = self.hold_invocation(invocation_id.as_deref()).await;
                let total = calls.len();
                let mut text = String::new();
                let mut steps: Vec<Value> = Vec::new();
                for (index, step) in calls.into_iter().enumerate() {
                    let invocation = batch_invocation(invocation_id.as_deref(), index)?;
                    let step_id = invocation.clone();
                    let reply = Box::pin(
                        self.do_one(&step.path, step.args, invocation, wait_ms, structured),
                    )
                    .await
                    .unwrap_or_else(|error| Reply::failed(format!("failed: {error}")));
                    let mut typed = reply.structured.clone().unwrap_or_else(
                        || json!({"status": if reply.failed {"failed"} else {"ok"}}),
                    );
                    if let (Some(fields), Some(id)) = (typed.as_object_mut(), step_id.as_ref()) {
                        fields
                            .entry("invocation_id")
                            .or_insert_with(|| json!(id.as_str()));
                    }
                    if let Some(fields) = typed.as_object_mut() {
                        fields.insert("path".into(), json!(step.path));
                    }
                    steps.push(typed);
                    let _ = write!(
                        text,
                        "{}[{}] {}: {}",
                        if text.is_empty() { "" } else { "\n" },
                        index.saturating_add(1),
                        step.path,
                        reply.text
                    );
                    if reply.failed {
                        let _ = write!(
                            text,
                            "\nstopped at {} of {total}; earlier steps stay applied",
                            index.saturating_add(1)
                        );
                        let mut reply = Reply::failed(text);
                        if structured {
                            reply.structured = Some(json!({
                                "status": "failed",
                                "stopped_at": index,
                                "steps": steps,
                            }));
                        }
                        return Ok(reply);
                    }
                }
                let mut reply = Reply::ok(text);
                if structured {
                    reply.structured = Some(json!({"status":"ok","steps":steps}));
                }
                Ok(reply)
            }
            _ => Err("name a path (with #action) or calls".into()),
        }
    }

    /// Run the one action `path` names.
    async fn do_one(
        &self,
        path: &str,
        arguments: serde_json::Map<String, Value>,
        invocation: Option<String>,
        wait_ms: Option<u64>,
        structured: bool,
    ) -> Result<Reply, String> {
        let asked = invocation.clone();
        if let Some(id) = asked.as_deref() {
            self.watch_run_started(id, path);
        }
        let answer = Box::pin(self.do_one_value(path, arguments, invocation, wait_ms)).await;
        let answer = match answer {
            Err(message)
                if structured
                    && path.contains('#')
                    && super::confirm::needs_the_person(&message) =>
            {
                // The person is being asked; the typed caller is told so rather
                // than left waiting, and the same id answers it once they do.
                let id = asked.as_deref().unwrap_or_default();
                let mut reply = Reply::ok(message);
                reply.failed = false;
                reply.structured = Some(json!({
                    "status": "pending",
                    "invocation_id": id,
                    "ask": {"invocation_id": id},
                }));
                return Ok(reply);
            }
            answer => answer?,
        };
        match answer {
            Done::Launched { value, mut reply } => {
                if let Some(id) = asked.as_deref() {
                    self.watch_run_ended(
                        id,
                        value
                            .pointer("/update/state")
                            .and_then(Value::as_str)
                            .unwrap_or("failed"),
                    );
                }
                if structured {
                    reply.structured = Some(answer_of(&value));
                }
                Ok(reply)
            }
            Done::Invoked {
                value,
                path,
                descriptor,
                app,
            } => {
                let mut reply = outcome(&value);
                if reply.failed && reply.text.contains("invalid_arguments") {
                    let _ = write!(reply.text, "\n{}", action_page(&path, &descriptor, None));
                }
                if let Some(id) = asked.as_deref() {
                    self.watch_run_ended(
                        id,
                        value
                            .pointer("/update/state")
                            .and_then(Value::as_str)
                            .unwrap_or("failed"),
                    );
                }
                if structured {
                    let mut typed = answer_of(&value);
                    if let Some(fields) = typed.as_object_mut() {
                        // The path this call ran, as a caller writes one.
                        fields.insert(
                            "path".into(),
                            json!(format!("{path}#{}", descriptor.address.action)),
                        );
                        // What the action takes, beside why running it did not
                        // work: the same page the text appends.
                        if reply.failed {
                            fields.insert("action".into(), action_row(&descriptor));
                            if let Some(error) =
                                fields.get_mut("error").and_then(Value::as_object_mut)
                            {
                                error.insert(
                                    "next_steps".into(),
                                    json!([format!("tilo_ls {path} lists its parameters")]),
                                );
                            }
                        }
                    }
                    reply.structured = Some(typed);
                }
                // A call naming the run it is a step of makes that run's step:
                // `undo task` takes it back in every app it changed, as it
                // takes back a program's.
                if !reply.failed
                    && descriptor.contract.kind != tilo_actions::ActionKind::Read
                    && let Some((gesture, label)) = Self::grouped_run()
                {
                    self.programs
                        .lock_recover()
                        .note_run(gesture, &label, &app, 1);
                }
                Ok(reply)
            }
        }
    }

    /// The app `parsed` names and the provider its `action` runs in, and
    /// whether the path pinned it; `None` for starting the app itself
    /// ([`Self::launch`]). One of an installed app's own actions
    /// (`terminal#run`) starts the app first and runs in what it opened, so
    /// the caller asks once.
    async fn acting_in(
        &self,
        parsed: &ParsedPath,
        action: &str,
        wait_ms: Option<u64>,
    ) -> Result<Option<(String, String, bool)>, String> {
        match self.app_ref(&parsed.app).await? {
            AppRef::Installed { opens: false, .. } => {
                Err(Self::not_running_note(&parsed.app, false))
            }
            AppRef::Installed { .. }
                if action == tilo_actions::remote::APPLICATION_OPEN_ACTION
                    || !parsed.segments.is_empty() =>
            {
                Ok(None)
            }
            AppRef::Installed { app_id, .. } => {
                let provider = self.opened_provider(&app_id, wait_ms).await?;
                Ok(Some((app_id, provider, true)))
            }
            AppRef::Running {
                app_id,
                provider,
                pinned,
            } => Ok(Some((app_id, provider, pinned))),
        }
    }

    /// Run the one action `path` names, answering what the host answered as it is.
    async fn do_one_value(
        &self,
        path: &str,
        arguments: serde_json::Map<String, Value>,
        invocation: Option<String>,
        wait_ms: Option<u64>,
    ) -> Result<Done, String> {
        let parsed = parse_path(path)?;
        let action = parsed.action.clone().ok_or_else(|| {
            format!("name the action: {path}#<action>; tilo_ls {path} lists them")
        })?;
        // The app is looked up once: resolving the path in it does not ask
        // again which apps run.
        let Some((app_id, provider, pinned)) = self.acting_in(&parsed, &action, wait_ms).await?
        else {
            let AppRef::Installed { app_id, .. } = self.app_ref(&parsed.app).await? else {
                return Err(format!("{} started while it was asked to", parsed.app));
            };
            let (value, reply) = self
                .launch(&parsed, &app_id, &action, arguments, invocation, wait_ms)
                .await?;
            return Ok(Done::Launched { value, reply });
        };
        // A path names a type the way a listing does (`tasks.Task`), so an action is found on
        // the resource a listing showed: such a segment is read by the program form first, and
        // a form no app declares is read plainly, as a title may be.
        let forms = if parsed
            .segments
            .iter()
            .any(|segment| is_program_form(segment))
        {
            self.program_forms().await
        } else {
            None
        };
        let mut resolved = if pinned {
            self.resolve_in(
                &parsed,
                app_id.clone(),
                provider.clone(),
                false,
                forms.as_ref(),
            )
            .await?
        } else {
            self.resolve_published(
                &parsed,
                app_id.clone(),
                provider.clone(),
                false,
                forms.as_ref(),
            )
            .await?
        };
        if forms.is_some() && resolved.chain.is_empty() {
            resolved = if pinned {
                self.resolve_in(&parsed, app_id, provider, false, None)
                    .await?
            } else {
                self.resolve_published(&parsed, app_id, provider, false, None)
                    .await?
            };
        }
        self.acted_in(&resolved);
        let (named_at, descriptor) =
            if let Some(descriptor) = self.action_named(&resolved, &action).await? {
                (resolved.path.clone(), descriptor)
            } else {
                // Not one of the target's actions by its id: what the target
                // offers decides (a label, the start of an id), as listing it
                // shows. A path naming the app alone means the app's own
                // actions and those it publishes on its resources.
                if let Some(node) = resolved.node.clone() {
                    let mut levels = std::mem::take(&mut resolved.levels);
                    let here = self
                        .level(&resolved.provider, &node.id, &mut levels)
                        .await?;
                    resolved.actions = if here.own.is_empty() {
                        node.actions.clone()
                    } else {
                        here.own
                    };
                }
                let (named_at, descriptor) = self
                    .pick_action_in(&action, &resolved)
                    .await
                    .map_err(|error| format!("{error}; tilo_ls {} lists them", resolved.path))?;
                (named_at, descriptor)
            };
        let value = Box::pin(self.invoke_one(OneCall {
            provider: Addressed {
                provider: resolved.provider.clone(),
                app_id: Some(resolved.app_id.clone()),
            },
            workspace: false,
            revision: None,
            resource: descriptor.address.resource.clone(),
            action: descriptor.address.action.clone(),
            arguments,
            invocation_id: invocation,
            wait_ms,
        }))
        .await?;
        Ok(Done::Invoked {
            value,
            path: named_at,
            descriptor: Box::new(descriptor),
            app: resolved.app_id.clone(),
        })
    }

    /// The action `name` means in what `resolved` names, with the path to
    /// the resource it is published on: the target's own actions, else — a
    /// path naming the app — the actions the app publishes on the resources
    /// it lists, one bounded read deep (a Settings root, a notebook's
    /// notes), never a walk of the app.
    async fn pick_action_in(
        &self,
        name: &str,
        resolved: &Resolved,
    ) -> Result<(String, ActionDescriptor), String> {
        // A path naming the app means what the app publishes on its
        // resources: its root's, and those of the resources one level down
        // in the containers it lists (a notebook's notes are the app's), as
        // far as one listing reads, never a walk of the app. The target's own
        // actions are asked first: they need no listing.
        if let Ok(action) = pick_action(name, &resolved.actions, &resolved.path) {
            return Ok((resolved.path.clone(), action.clone()));
        }
        let mut levels = resolved.levels.clone();
        let mut published: Vec<(String, ActionDescriptor)> = Vec::new();
        if resolved.node.is_none() {
            let root = self.app_children(&resolved.provider, &mut levels).await?;
            let siblings = &root.children;
            for node in siblings {
                let path = path_of(node, &levels).unwrap_or_else(|| node.name(siblings));
                for action in &node.actions {
                    published.push((path.clone(), action.clone()));
                }
                if !node.container {
                    continue;
                }
                let inside = self
                    .level(&resolved.provider, &node.id, &mut levels)
                    .await?;
                let held = &inside.children;
                for child in held {
                    let at = format!("{path}/{}", child.name(held));
                    for action in &child.actions {
                        published.push((at.clone(), action.clone()));
                    }
                }
            }
        }
        let ranked: Vec<(u8, String, &ActionDescriptor)> = published
            .iter()
            .filter_map(|(path, action)| Some((action_rank(name, action)?, path.clone(), action)))
            .collect();
        let (path, action) = pick_action_anywhere(name, resolved, &ranked)?;
        // The path to the resource it is published on; the action follows
        // its `#`, as every path's does.
        let named_at = match path {
            Some(path) => format!("{}/{}", resolved.path, path),
            None => resolved.path.clone(),
        };
        Ok((named_at, action.clone()))
    }

    /// The action `action` on the resolved target, by its id, asked of the
    /// app by its address at the revision the target was found at: an
    /// action on a notebook is found without a page of the notebook's notes.
    /// `None` when the target offers no action by that id (or the directory
    /// moved on), for the caller to list it.
    async fn action_named(
        &self,
        resolved: &Resolved,
        action: &str,
    ) -> Result<Option<ActionDescriptor>, String> {
        let (Some(node), false) = (&resolved.node, resolved.revision.is_empty()) else {
            return Ok(None);
        };
        let Ok(page) = self
            .ask(CatalogueQuery {
                provider: Some(resolved.provider.clone()),
                target: Some(tilo_actions::ActionAddress::new(node.id.clone(), action)),
                revision: Some(resolved.revision.clone()),
                max_results: 1,
                ..CatalogueQuery::default()
            })
            .await
        else {
            return Ok(None);
        };
        Ok(page
            .rows
            .into_iter()
            .filter_map(|row| row.action)
            .find(|found| found.address.resource == node.id && found.address.action == action))
    }

    /// `notes#open`: start an installed app, then say what is in it.
    async fn launch(
        &self,
        parsed: &ParsedPath,
        app_id: &str,
        action: &str,
        arguments: serde_json::Map<String, Value>,
        invocation: Option<String>,
        wait_ms: Option<u64>,
    ) -> Result<(Value, Reply), String> {
        if !parsed.segments.is_empty() || action != tilo_actions::remote::APPLICATION_OPEN_ACTION {
            return Err(Self::not_running_note(&parsed.app, true));
        }
        let value = self
            .open_in_workspace(app_id, arguments, invocation, wait_ms)
            .await?;
        let mut reply = outcome(&value);
        if reply.failed {
            return Ok((value, reply));
        }
        let name = app_names(&[app_id.to_owned()]).join("");
        reply.text = format!("ok: {name} is running");
        if let Some(error) = value.pointer("/opened/error").and_then(Value::as_str) {
            let _ = write!(reply.text, "\nnot opened: {error}");
        }
        if let Ok(listing) = self
            .agent_ls(
                LsArgs {
                    path: Some(name),
                    ..LsArgs::default()
                },
                false,
            )
            .await
        {
            reply.text.push('\n');
            reply.text.push_str(&listing.text);
        }
        Ok((value, reply))
    }

    /// The workspace's `open` of the app `app_id`, with `arguments` for the files it opens
    /// there: the workspace's answer, which names the instance it started at [`super::OPENED_PEER`].
    async fn open_in_workspace(
        &self,
        app_id: &str,
        arguments: serde_json::Map<String, Value>,
        invocation: Option<String>,
        wait_ms: Option<u64>,
    ) -> Result<Value, String> {
        Box::pin(self.invoke_one(OneCall {
            provider: Addressed {
                provider: tilo_actions::remote::workspace_provider(&self.workspace),
                app_id: None,
            },
            workspace: true,
            revision: None,
            resource: format!(
                "{}{app_id}",
                tilo_actions::remote::APPLICATION_RESOURCE_PREFIX
            ),
            action: tilo_actions::remote::APPLICATION_OPEN_ACTION.to_owned(),
            arguments,
            invocation_id: invocation,
            wait_ms,
        }))
        .await
    }

    /// The provider of a running instance of the installed app `app_id`, starting the app in
    /// the workspace when none runs yet: where an invocation of the app's own door goes.
    async fn opened_provider(&self, app_id: &str, wait_ms: Option<u64>) -> Result<String, String> {
        match self.app_ref(app_id).await? {
            AppRef::Running { provider, .. } => Ok(provider),
            AppRef::Installed { app_id, opens } => {
                if !opens {
                    return Err(Self::may_not_open(&app_id));
                }
                let value = self
                    .open_in_workspace(&app_id, serde_json::Map::new(), None, wait_ms)
                    .await?;
                if value.pointer("/update/state").and_then(Value::as_str) != Some("succeeded") {
                    return Err(format!("{app_id} did not open: {}", outcome(&value).text));
                }
                if let Some(error) = value.pointer("/opened/error").and_then(Value::as_str) {
                    return Err(format!("{app_id} did not open: {error}"));
                }
                // The instance the workspace started, as it names it; a workspace naming none
                // lets the caller find it among the running ones.
                if let Some(peer) = value.pointer(super::OPENED_PEER).and_then(Value::as_u64) {
                    return Ok(peer.to_string());
                }
                // A host serving the workspace's records names the record it
                // opened the app on.
                if let Some(provider) = value
                    .pointer("/update/output/value/provider")
                    .and_then(Value::as_str)
                    .filter(|provider| tilo_actions::remote::is_record_provider(provider))
                {
                    return Ok(provider.to_owned());
                }
                let (_, running) = self.providers().await?;
                running
                    .iter()
                    .rev()
                    .find(|(_, instance)| instance["app_id"].as_str() == Some(app_id.as_str()))
                    .map(|(peer, _)| peer.clone())
                    .ok_or_else(|| format!("{app_id} opened, but no instance of it runs"))
            }
        }
    }

    async fn agent_status(
        &self,
        args: &AgentStatusArgs,
        structured: bool,
    ) -> Result<Reply, String> {
        let invocation = invocation_id(&args.invocation_id)?;
        if args.cancel {
            self.host()
                .cancel(&self.caller(), &invocation)
                .await
                .ok_or("the host dropped the request")?
                .map_err(|error| error.to_string())?;
        }
        let value = self.status(invocation)?;
        let mut reply = outcome(&value);
        if structured {
            reply.structured = Some(answer_of(&value));
        }
        Ok(reply)
    }
}

/// The one window `instance` (a provider an `open` or `tilo_ls` named) among `running`, which
/// must be of the app `name`: the agent means that window, never another of the app's.
fn pinned_window(
    running: &[(String, Value)],
    name: &str,
    instance: &str,
) -> Result<AppRef, String> {
    let (provider, entry) = running
        .iter()
        .find(|(provider, _)| provider == instance)
        .ok_or_else(|| format!("no running window {instance}; tilo_ls lists the running apps"))?;
    let app_id = entry["app_id"]
        .as_str()
        .ok_or_else(|| format!("window {instance} names no app"))?
        .to_owned();
    if page::app_named(name, &[app_id.as_str()]).is_err() {
        return Err(format!(
            "window {instance} is {}, not {name}",
            app_names(std::slice::from_ref(&app_id)).join("")
        ));
    }
    Ok(AppRef::Running {
        app_id,
        provider: provider.clone(),
        pinned: true,
    })
}

#[cfg(test)]
#[path = "mcp_agent_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "mcp_agent_publisher_tests.rs"]
mod publisher_tests;
