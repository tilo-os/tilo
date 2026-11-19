//! The agent surface: the tools an AI member can drive any app with.
//!
//! `tilo_ls`, `tilo_read`, `tilo_do`, `tilo_find`, `tilo_link` and `tilo_status` address
//! everything by **path**: `notes/Todo/body#append-paragraph` is the app
//! (`notes`), the resource by its title, kind or key (`Todo`, `body`) and the
//! action after `#`. `browser@4711/page#read-page` names one window of an
//! app by its provider (what the desktop's `open` answers, what `tilo_ls`
//! lists); without it the app's newest window answers. The host's own
//! services answer to their app too: `desktop` is the desktop's layout, on a
//! desktop that serves it. A path names things as a person would and survives a
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
    ActionProvider, Addressed, CatalogueLimits, CatalogueQuery, CatalogueRow, DEFAULT_WAIT_MS,
    Deserialize, Failure, MAX_CALLS, McpServer, OneCall, RemoteActionTarget, Value,
    batch_invocation, invocation_id, json, page, parse,
};
use std::collections::HashMap;
use std::fmt::Write as _;
use tilo_actions::remote::Found;
use tilo_actions::{ActionDescriptor, OperationKind};
use tilo_lock::MutexRecover as _;

#[path = "mcp_answer.rs"]
mod answer;
#[path = "mcp_ask.rs"]
mod ask;
#[path = "mcp_agent_find.rs"]
mod find;
#[path = "mcp_agent_launch.rs"]
mod launch;
use launch::{names_files, opens_a_file, starts_app};
#[path = "mcp_agent_link.rs"]
mod link;
#[path = "mcp_agent_list.rs"]
mod list;
#[path = "mcp_agent_names.rs"]
mod names;
#[path = "mcp_agent_record.rs"]
mod record;
#[path = "mcp_render.rs"]
mod render;
#[path = "mcp_status.rs"]
mod status;
#[path = "mcp_tilo.rs"]
mod tilo;
#[path = "mcp_undo_invocation.rs"]
mod undo_invocation;
#[path = "mcp_agent_view.rs"]
mod view;
#[path = "mcp_watch.rs"]
pub(super) mod watch;
use answer::{action_named, action_row, actions_named, answer_of, outcome, text_result};
use level::{LevelPages, LsCursor, PageMark};
use render::{
    action_line, action_page, block_line, parse_read_cursor, read_answer, restart_pause,
    strip_blank,
};
use tilo::actions_note;
/// A person's own line in a run's conversation, run in the task's host as they are
/// (shell.md §8.1): what the API's `person_line` hands on.
pub use tilo::{PersonStep, literals_in};
pub(super) use tilo::{ProgramForms, Programs, TaskLink};
pub use tilo::{TaskHosts, TaskProgram};

/// Resource rows one directory level reads at most, across its pages.
const LEVEL_PAGES: usize = 4;
/// Times a name not found is looked for again, after pauses that double from 5 ms to 200 ms:
/// about seven tenths of a second in all.
const LOOKS_AGAIN: u32 = 8;
/// The moment those looks come to. A level that was read on past its bound is looked through
/// again only within it: one that takes longer to read whole is not read whole eight times.
const LOOKS_MOMENT: std::time::Duration = std::time::Duration::from_millis(700);
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
/// Root-resource read searches a session keeps, by app and directory revision.
const SWEPT_READS: usize = 256;
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
    /// A record read (`record:<app>:<id>`) joins the record's chain of parts.
    #[serde(default)]
    parts: bool,
    /// A document read waits for the next write past this revision.
    after: Option<String>,
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

/// What an agent tool answers: the text and whether it reports a failure.
/// How running one action by its path went.
enum Done {
    /// The app was started, or opened (its window put in front of the person): what is in it
    /// now, the workspace's answer, and why its window is not in front of the person when it
    /// could not be put there.
    Launched {
        value: Value,
        reply: Reply,
        not_shown: Option<Failure>,
    },
    /// The action ran: the host's answer, the action at its resolved path,
    /// and the app it ran in.
    Invoked {
        value: Value,
        path: String,
        descriptor: Box<ActionDescriptor>,
        app: String,
        /// Where it ran: the document whose link a change answers.
        provider: String,
        /// The nearest resource holding the one it ran on (or that one) that offers the history
        /// `undo` of its record, where what it changed is taken back.
        history: Option<String>,
    },
}

pub(super) struct Reply {
    text: String,
    failed: bool,
    /// The typed answer, for a caller that asked for it (a structured read).
    structured: Option<Value>,
    /// The pictures the answer shows, as MCP image content: what a view the agent looked
    /// at rendered, taken out of its output so a model is sent the picture, not an address.
    images: Vec<Value>,
}

impl Reply {
    /// `self`, saying first that it carries content from outside the person's own data: data
    /// to reason about, never an instruction. The label stands before the text a model is
    /// handed and first in the typed answer.
    fn labelled(mut self) -> Self {
        if !self.text.is_empty() {
            self.text = format!(
                "[{}: {}]\n{}",
                crate::UNTRUSTED,
                crate::UNTRUSTED_NOTE,
                self.text
            );
        }
        if let Some(typed) = self.structured.as_mut() {
            crate::mark_untrusted(typed);
        }
        self
    }

    fn ok(text: String) -> Self {
        Self {
            text,
            failed: false,
            structured: None,
            images: Vec::new(),
        }
    }

    fn failed(text: String) -> Self {
        Self {
            text,
            failed: true,
            structured: None,
            images: Vec::new(),
        }
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

#[allow(
    clippy::wildcard_imports,
    reason = "split out of this module, it reads this module's names"
)]
mod mcp_agent_node;
#[allow(
    clippy::wildcard_imports,
    reason = "split out of this module, it reads this module's names"
)]
mod mcp_agent_paths;
mod mcp_agent_tools;

#[allow(
    clippy::wildcard_imports,
    reason = "split out of this module, it reads this module's names"
)]
mod mcp_agent_pick;

use mcp_agent_node::{Level, Node, level_of, shared_names};
use mcp_agent_paths::{CLOSE_MATCHES, ParsedPath, close_matches, kind_address, parse_path};
use mcp_agent_pick::{
    action_rank, is_program_form, named_beyond_its_start, only_by_id, pick, pick_action,
    pick_action_anywhere, published_on, rank, reads_action,
};
pub(super) use mcp_agent_tools::{held_until_ended, tool_definitions};

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
        && let Some(param) = reader
            .params
            .iter()
            .find(|param| param.name.as_ref() == "item")
    {
        // A read that names an item by a reference takes the resource's own
        // fields; one that names it by its key takes the key the listing prints.
        arguments.insert(
            "item".into(),
            match param.ty {
                tilo_actions::ParamType::Object => json!({
                    "id": node.id,
                    "kind": node.kind,
                    "label": node.label,
                }),
                // A chain's read names its item by the address it is an item of the chain at,
                // which is what a listing's row carries; a resource published under a bound id
                // goes by the key its id carries.
                _ => Value::String(if node.key == node.id {
                    node.id.clone()
                } else {
                    node.key.clone()
                }),
            },
        );
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
    /// whose every entry names an item and a title, or holds a collection's
    /// row (`{id, value}`), is children, the rest is fields.
    fn of(value: &Value) -> Self {
        let kind = value.get("kind").and_then(Value::as_str).map(str::to_owned);
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
                    .filter_map(|entry| child(list, entry, kind.as_deref()))
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
            kind,
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

/// `entry`, an entry of the list `list` a read answers, as the child it
/// names: a listing's row (`{item, title}`), or a collection's row
/// (`{id, value}`, the value as the model writes it), whose id is the item's
/// own and whose name is the title, name, label or text its value carries,
/// else the id's last step. A collection's item is addressed by the resource
/// id its collection publishes it under (`chat.messages/messages/m1`), which
/// is what a read or an action takes.
fn child(list: &str, entry: &Value, kind: Option<&str>) -> Option<ChildRef> {
    let row = |item: String, name: String| ChildRef {
        list: list.to_owned(),
        name,
        item,
        path: String::new(),
    };
    // A collection's entry holds its item's id and value and nothing else,
    // and the read names the collection's kind, which the item's address is
    // built from.
    let collection_row = entry.as_object().is_some_and(|fields| {
        fields.len() == 2 && fields.contains_key("id") && fields.contains_key("value")
    });
    if let (true, Some(kind), Some(id), Some(value)) = (
        collection_row,
        kind,
        entry.get("id").and_then(Value::as_str),
        entry.get("value"),
    ) {
        let named = ["title", "name", "label", "text"]
            .iter()
            .find_map(|key| value.get(*key).and_then(Value::as_str))
            .map(|text| text.lines().next().unwrap_or(text).trim())
            .filter(|name| !name.is_empty());
        let tail = id.rsplit('/').next().unwrap_or(id).to_owned();
        return Some(row(
            format!("{kind}/{list}/{id}"),
            named.unwrap_or(&tail).to_owned(),
        ));
    }
    let item = entry.get("item").and_then(Value::as_str)?;
    let name = ["title", "name", "label"]
        .iter()
        .find_map(|key| entry.get(*key).and_then(Value::as_str))?;
    Some(row(item.to_owned(), name.to_owned()))
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
pub(super) struct InstalledApp {
    app_id: String,
    label: String,
    /// Whether this caller may open it: the workspace names an app it may
    /// not open without the action.
    opens: bool,
    /// What its signed package publishes (`surface.toml`), as the workspace's
    /// listing carries it: the types an agent reads before the app opens.
    surface: Option<String>,
}

impl InstalledApp {
    /// The app as naming it answers: its newest window `window` while it
    /// runs, else the app to open.
    fn named(&self, window: Option<String>) -> AppRef {
        let app_id = self.app_id.clone();
        match window {
            Some(provider) => AppRef::Running {
                app_id,
                provider,
                pinned: false,
            },
            None => AppRef::Installed {
                app_id,
                opens: self.opens,
            },
        }
    }
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

/// Whether `page` says of itself that it is not all there is and cannot be gone on from: it
/// stopped at a limit, is not the end and offers no cursor. An app answers so when its
/// directory cannot be read now and when a page of it broke off.
fn cannot_be_read_now(page: &tilo_actions::remote::CataloguePage) -> bool {
    page.budget_exhausted && !page.exhausted && page.next_cursor.is_empty()
}

impl Resolved {
    /// Where a change of the target is taken back: its document's own history, on the nearest
    /// resource that holds the target (or the target itself) and offers `undo`; for an app
    /// named itself (a document whose root is the app's), where its root actions offer it.
    fn history(&self) -> Option<String> {
        let offers = |node: &&Node| {
            node.actions
                .iter()
                .any(|action| action.address.action == "undo")
        };
        let nearest = self.node.iter().chain(self.chain.iter().rev()).find(offers);
        nearest.map(|node| node.id.clone()).or_else(|| {
            self.node
                .is_none()
                .then(|| {
                    self.actions
                        .iter()
                        .find(|action| action.address.action == "undo")
                        .map(|action| action.address.resource.clone())
                })
                .flatten()
        })
    }
}

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
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
        let read = self
            .read_level(provider, parent, None, usize::MAX)
            .await?
            .ok_or("the app's directory kept changing while it was read; try again")?;
        levels.insert(parent.to_owned(), read.clone());
        Ok(read)
    }

    /// One directory level, read as far as one page of a listing holds:
    /// enough to name a resource and what it holds on its first page, and
    /// extended by whoever pages past it ([`Self::level_reaching`],
    /// [`Self::level_holding`]). A listing reads one page of a directory of
    /// thousands per call, not a fixed stretch of it.
    async fn level_near(
        &self,
        provider: &str,
        parent: &str,
        levels: &mut Levels,
    ) -> Result<Level, String> {
        if let Some(read) = levels.get(parent) {
            return Ok(read.clone());
        }
        let read = self
            .read_level(provider, parent, None, LS_LINES)
            .await?
            .ok_or("the app's directory kept changing while it was read; try again")?;
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
        // Files is every workspace's own folder, served by the framework
        // rather than installed into it: `files#open` reaches it wherever the
        // workspace is, and puts a file into its tree.
        if names_files(name) {
            return Ok(AppRef::Installed {
                app_id: fs_protocol::tree::FILES_APP.to_owned(),
                opens: true,
            });
        }
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
            provider.parse::<u64>().is_ok() || tilo_actions::remote::is_resource_provider(provider)
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
        // The name the listing shows an app by (`Files`) is that app.
        if let Some(app) = installed
            .as_ref()
            .ok()
            .and_then(|apps| listed_as(apps, name))
        {
            return Ok(app.named(newest(&app.app_id, true)));
        }
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
                "no running app is named {name}, and the installed apps could not be read: {}",
                answer::words(&reason)
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
            // No app is called exactly that: the name is read once more as a model writes one.
            Err(_) => freely_named(name, &running, &installed),
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
                    Some(app) => {
                        app.opens |= opens;
                        if row.surface.is_some() {
                            app.surface.clone_from(&row.surface);
                        }
                    }
                    None => apps.push(InstalledApp {
                        app_id: app_id.to_owned(),
                        label: row.resource.label.clone(),
                        opens,
                        surface: row.surface.clone(),
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
        // A name the app's own level carries is asked of the app by name
        // first: a chain it keeps is answered from the head part publishing
        // it, never a page of the listing built from every item the app
        // shows. A name no one resource there answers is listed as before.
        if let Some(among) = self.named(provider, "", name).await?
            && let [only] = among.children.as_slice()
        {
            return Ok(Some(Level {
                own: Vec::new(),
                children: vec![only.clone()],
                revision: among.revision,
                pages: std::sync::Arc::default(),
            }));
        }
        let root = self.level(provider, "", levels).await?;
        let mut revision = root.revision.clone();
        // A name the app's own level carries exactly is answered there: its id
        // first, then its label. The containers hold the resource's items,
        // and asking each of them for a name it is not (the resource's own
        // name, most often) lists every item it holds.
        let by_id: Vec<&Node> = root
            .children
            .iter()
            .filter(|node| node.names_id(name))
            .collect();
        let by_label: Vec<&Node> = root
            .children
            .iter()
            .filter(|node| node.answers_to(name) == Some(0))
            .collect();
        for named in [by_id, by_label] {
            if let [only] = named.as_slice() {
                return Ok(Some(Level {
                    own: Vec::new(),
                    children: vec![(**only).clone()],
                    revision,
                    pages: std::sync::Arc::default(),
                }));
            }
        }
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
            pages: std::sync::Arc::default(),
        }))
    }

    /// The resource `segment` names under `parent` (the app itself for
    /// `None`), and the resources it was named among. A name only one
    /// resource there carries is asked of the app by name; anything else (a
    /// key, the start of a name, a name several carry, one that is not
    /// there) is decided on the whole level, as listing it shows. With them
    /// comes the directory revision it was found at. `at` is the address the
    /// level is listed at, which a name not found is answered with.
    async fn step(
        &self,
        provider: &str,
        at: &str,
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
            // What the app answered is the resource the segment names: one
            // whose id it is, before any label, else one its label names
            // exactly. Either costs the answer, not a read of the level.
            let mut named = among.children.iter().filter(|node| node.names_id(segment));
            if let (Some(only), None) = (named.next(), named.next()) {
                let only = only.clone();
                return Ok((only, among));
            }
            let mut named = among
                .children
                .iter()
                .filter(|node| node.answers_to(segment) == Some(0));
            if let (Some(only), None) = (named.next(), named.next()) {
                let only = only.clone();
                return Ok((only, among));
            }
        }
        // An app's directory is rebuilt whenever its document changes, and for an instant it
        // lists less than it has. A name not found is looked for again for a moment, the
        // levels read anew, before the app is said not to have it.
        let began = tokio::time::Instant::now();
        let mut looks: u32 = 0;
        loop {
            // A level is read as far as its bound, and a directory that holds more goes on
            // past it. A name the pages read first do not carry is looked for on the pages
            // after them, to the directory's end, before it is looked for anywhere else: what
            // a resource holds is found under it wherever it falls in the directory, and
            // only a name no page carries is not there.
            let (picked, here) = if let Some(owner) = parent {
                self.level_holding(provider, &owner.id, levels, |level| {
                    Ok(pick(segment, level.whole_children(), forms)?.cloned())
                })
                .await?
            } else {
                let here = self.app_children(provider, levels).await?;
                (pick(segment, &here.children, forms)?.cloned(), here)
            };
            let found = match picked {
                Some(node) => Some(node),
                None => self.deep(provider, segment, parent, levels, forms).await?,
            };
            if let Some(found) = found {
                return Ok((found, here));
            }
            if looks >= LOOKS_AGAIN || (here.read_on() && began.elapsed() >= LOOKS_MOMENT) {
                // Each name as the address that takes it, not the word alone:
                // a caller reading where it went wrong writes the next call
                // from what it is told, and a word it cannot place is no
                // address at all.
                let addresses: Vec<_> = (here.children.iter())
                    .map(|node| format!("{at}/{}", node.name(&here.children)))
                    .collect();
                return if addresses.is_empty() {
                    Err(format!("no {segment} in {at}; it lists nothing here"))
                } else {
                    Err(format!(
                        "no {segment} in {at}; it has: {}",
                        list_names(&addresses)
                    ))
                };
            }
            looks = looks.saturating_add(1);
            tokio::time::sleep(restart_pause(looks.saturating_add(1))).await;
            levels.clear();
        }
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
                .flat_map(|level| level.whole_children().iter())
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
        let reference = match self.app_ref(&parsed.app).await {
            Ok(reference) => reference,
            Err(why) => match self.kind_app(&parsed.app).await {
                Some((app_id, provider, _)) => {
                    return Ok(Ok(self
                        .resolve_in(parsed, app_id, provider, true, forms)
                        .await?));
                }
                None => return Err(why),
            },
        };
        match reference {
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
            // Reading what an installed app holds starts it without a window, as an action on
            // it does: a path names the app's resources, not one of its processes.
            AppRef::Installed {
                app_id,
                opens: true,
            } => {
                let provider = self.started_provider(&app_id, None).await?;
                Ok(Ok(self
                    .resolve_published(parsed, app_id, provider, true, forms)
                    .await?))
            }
            AppRef::Installed { opens: false, .. } => {
                Ok(Err(Self::not_running_note(&parsed.app, false)))
            }
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
        // What an action answered is a path the next call takes as it is:
        // `tasks.tracker/issues/7` names one resource whole, where its parts
        // named one at a time would name every kind that shares a word.
        if kind_address(&parsed.app).is_some() {
            let address = parsed.address();
            if let Some(found) = self
                .by_item_address(&app_id, &provider, &address, &mut levels)
                .await?
            {
                return Ok(found);
            }
        }
        let mut chain: Vec<Node> = Vec::new();
        // What each resource on the path was named among, and the revision
        // the last of them was found at.
        let mut among: Vec<Vec<Node>> = Vec::new();
        let mut revision = String::new();
        // The address each level is listed at, as the app's own listing writes
        // it: what a name not found there is answered with.
        let mut at = app.clone();
        for segment in &parsed.segments {
            let (found, siblings) = self
                .step(&provider, &at, chain.last(), segment, &mut levels, forms)
                .await?;
            at = format!("{at}/{}", found.name(&siblings.children));
            chain.push(found);
            among.push(siblings.children);
            revision = siblings.revision;
        }
        let mut here = match chain.last() {
            Some(_) if !list => Level::default(),
            Some(node) => self.level_near(&provider, &node.id, &mut levels).await?,
            None => {
                // What the app's containers hold (a notebook's notes) is read for a listing.
                // An action named on the app is found on the app's own level first
                // ([`Self::pick_action_in`]): `notes#create-page` is the notebook's, and
                // reading every note to find it would grow with the notebook.
                let mut root = if list {
                    self.app_children(&provider, &mut levels).await?
                } else {
                    self.level(&provider, "", &mut levels).await?
                };
                // A host service (the desktop's layout, on `desktop:session`)
                // publishes everything on its one resource: its actions are
                // the service's own, `desktop#<action>`. A resource's one
                // container (Tasks' tracker) holds what the app offers more.
                if provider.parse::<u64>().is_err()
                    && root.own.is_empty()
                    && let [only] = root.children.as_slice()
                    && !only.container
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
                "not_found: {name} is installed but not running; start it with tilo_do {}#open, or run one of its actions and it starts first",
                path_name(name)
            )
        } else {
            format!(
                "not_found: {name} is installed but not running, and {}",
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

/// Why `name` names no app: how a running app is addressed, and the apps
/// running and installed.
fn no_app_named(name: &str, running: &[(String, Value)], installed: &[InstalledApp]) -> String {
    let running_ids = running
        .iter()
        .filter_map(|(_, app)| app["app_id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let installed_ids = installed
        .iter()
        .map(|app| app.app_id.clone())
        .collect::<Vec<_>>();
    format!(
        "unknown_app: no app {name}; name a running app by its id or last part, e.g. {}. Running {}; installed {}",
        running_ids.first().map_or("<app_id>", String::as_str),
        list_or_none(&app_names(&running_ids)),
        list_or_none(&app_names(&installed_ids)),
    )
}

/// The one installed app the listing shows by the name `name` (`Files` for
/// `file-explorer`), any case.
fn listed_as<'a>(installed: &'a [InstalledApp], name: &str) -> Option<&'a InstalledApp> {
    let mut named = installed
        .iter()
        .filter(|app| app.label.eq_ignore_ascii_case(name));
    named.next().filter(|_| named.next().is_none())
}

/// The app `name` means where no app is called exactly that: the name read once more as a
/// model writes one (`word_processor`, `WordProcessor`, `Word`), among the installed apps as
/// this caller is listed them and the apps of the providers it may use, and no others. What it
/// finds is opened and acted in as the listing says this caller may. A word that still names
/// none (`host` for the Browser, `so.tilo.browser`) is a guess, so the running apps are named
/// as they are to be addressed.
fn freely_named(
    name: &str,
    running: &[(String, Value)],
    installed: &[InstalledApp],
) -> Result<AppRef, String> {
    let listed: Vec<(&str, &str)> = installed
        .iter()
        .map(|app| (app.app_id.as_str(), app.label.as_str()))
        .chain(
            running
                .iter()
                .filter_map(|(_, app)| Some((app["app_id"].as_str()?, ""))),
        )
        .collect();
    let Some(app_id) = names::app_meant(name, &listed)? else {
        return Err(no_app_named(name, running, installed));
    };
    // Its newest window, or else whatever answers for it without one.
    let newest = |window: bool| {
        running
            .iter()
            .rev()
            .filter(|(provider, _)| {
                (provider.parse::<u64>().is_ok()
                    || tilo_actions::remote::is_resource_provider(provider))
                    == window
            })
            .find(|(_, app)| app["app_id"].as_str() == Some(app_id.as_str()))
            .map(|(provider, _)| provider.clone())
    };
    if let Some(app) = installed.iter().find(|app| app.app_id == app_id) {
        return Ok(app.named(newest(true)));
    }
    let provider = newest(true).or_else(|| newest(false));
    match provider {
        Some(provider) => Ok(AppRef::Running {
            app_id,
            provider,
            pinned: false,
        }),
        None => Err(no_app_named(name, running, installed)),
    }
}

pub(super) fn app_names(ids: &[String]) -> Vec<String> {
    let mut names: Vec<String> = ids
        .iter()
        .map(|id| id.rsplit('.').next().unwrap_or(id).to_lowercase())
        .collect();
    names.sort();
    names.dedup();
    names
}

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// Answer one agent tool call.
    /// `structured` when the caller asked for typed answers
    /// (`_meta["so.tilo.mcp"].structured`): a read answers its
    /// [`ResourceRead`].
    pub(super) async fn agent_call(&self, name: &str, mut args: Value, structured: bool) -> Value {
        // A path written as what a person types to name a resource (`#12`) is
        // read as the address the tools list it at before the call resolves
        // it, so an agent going on from a prompt's words reaches the resource.
        if let Some(path) = args.get("path").and_then(Value::as_str).map(str::to_owned) {
            match self.expand_reference(&path).await {
                Ok(expanded) => {
                    if let Some(fields) = args.as_object_mut() {
                        fields.insert("path".into(), Value::String(expanded));
                    }
                }
                Err(error) => return text_result(Err(error), structured, true),
            }
        }
        let result = match name {
            "tilo_ls" => match parse(args) {
                Ok(args) => self.agent_ls(args, structured).await,
                Err(error) => Err(error.into()),
            },
            "tilo_read" => match parse(args) {
                Ok(args) => Box::pin(self.agent_read(args, structured)).await,
                Err(error) => Err(error.into()),
            },
            "tilo_do" => match parse(args) {
                Ok(args) => self.agent_do(args, structured).await,
                Err(error) => Err(error.into()),
            },
            "tilo_link" => match parse(args) {
                Ok(args) => self.agent_link(args, structured).await,
                Err(error) => Err(error.into()),
            },
            "tilo_view" => match parse(args) {
                Ok(args) => Box::pin(self.agent_view(args, structured)).await,
                Err(error) => Err(error.into()),
            },
            "tilo_find" => match parse(args) {
                Ok(args) => self.agent_find(args, structured).await,
                Err(error) => Err(error.into()),
            },
            "tilo_status" => match parse(args) {
                Ok(args) => self.agent_status(&args, structured).await,
                Err(error) => Err(error.into()),
            },
            "tilo" => match parse(args) {
                Ok(args) => Box::pin(self.agent_tilo(args, structured)).await,
                Err(error) => Err(error.into()),
            },
            "tilo_ask" => match parse(args) {
                Ok(args) => self.agent_ask(args, structured).await,
                Err(error) => Err(error.into()),
            },
            "tilo_watch" => match parse(args) {
                Ok(args) => self.agent_watch(args).await,
                Err(error) => Err(error.into()),
            },
            _ => Err("Unknown tool".into()),
        };
        // A program may have run steps before the one refused: running it
        // again would run them twice.
        text_result(result, structured, name != "tilo")
    }

    /// Note the record a call resolved `in`: the one its provider names when
    /// the workspace serves it as a record (`record:<app>:<record>`). A call
    /// whose resource no record provider answers leaves the last one named:
    /// the agent still acts there.
    fn acted_in(&self, provider: &str, app_id: &str) {
        let Some((app, document)) = tilo_actions::remote::resource_parts(provider) else {
            return;
        };
        if app != app_id {
            return;
        }
        *self.acted.lock_recover() = Some(super::ActedRecord {
            app: app.to_owned(),
            record: document.to_owned(),
        });
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one listing, its text lines and its typed rows in the order it writes them"
    )]
    async fn agent_ls(&self, args: LsArgs, structured: bool) -> Result<Reply, Failure> {
        let cursor = match args.cursor.as_deref() {
            None | Some("") => LsCursor {
                offset: 0,
                resume: None,
            },
            Some(cursor) => LsCursor::parse(cursor)
                .ok_or_else(|| "cursor is not one tilo_ls issued".to_owned())?,
        };
        let offset = cursor.offset;
        let limit = args.limit.unwrap_or(LS_LINES).clamp(1, 200);
        let (want_resources, want_actions) = match args.kind.as_deref() {
            None | Some("") => (true, true),
            Some("resources") => (true, false),
            Some("actions") => (false, true),
            Some(other) => {
                return Err(format!("kind is resources or actions, not {other}").into());
            }
        };
        let path = args.path.as_deref().map(str::trim).unwrap_or_default();
        if path.is_empty() {
            return self.list_apps(offset, limit, structured).await;
        }
        let parsed = parse_path(path)?;
        // How a program writes what this lists: the listing is the one a program reads, and a
        // path names what it lists the same way, so the path is resolved by it too.
        let forms_timed = super::timing_start();
        let forms = self.program_forms().await.unwrap_or_default();
        super::timing_done("ls.forms", forms_timed);
        let resolving = super::timing_start();
        let resolved =
            if names_files(&parsed.app) && parsed.segments.is_empty() && parsed.action.is_none() {
                self.files_resolved(&parsed, &forms).await?
            } else {
                match self.resolve_with(&parsed, Some(&forms)).await? {
                    Ok(resolved) => resolved,
                    Err(note) => {
                        super::timing_done("ls.resolve", resolving);
                        return Ok(Reply::ok(note));
                    }
                }
            };
        super::timing_done("ls.resolve", resolving);
        self.acted_in(&resolved.provider, &resolved.app_id);
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
        // What the resource holds, read as far as this page reaches: a
        // directory read stops at its bound, and a listing paging past it
        // reads on, so every resource is reachable and the cursor says
        // whether more follow.
        let end = offset.saturating_add(limit);
        let mut children = resolved.children.clone();
        // The index of `children`'s first among everything listed: a page
        // read on from where the last one ended starts past the start.
        let mut first = 0usize;
        let mut read: Option<(Level, usize)> = None;
        if want_resources
            && let Some(node) = &resolved.node
            && let Some(level) = resolved.levels.get(&node.id).cloned()
        {
            let resumed = match cursor.resume {
                Some((mark, skip)) if offset > 0 => self
                    .level_from(
                        &resolved.provider,
                        &node.id,
                        mark,
                        skip.saturating_add(limit),
                    )
                    .await?
                    .filter(|level| level.children.len() >= skip)
                    .map(|level| (level, skip)),
                _ => None,
            };
            let (level, skipped, from_start) = if let Some((level, skip)) = resumed {
                (level, skip, false)
            } else {
                let reading = super::timing_start();
                let level = self
                    .level_reaching(&resolved.provider, &node.id, level, end)
                    .await?;
                super::timing_done("ls.level", reading);
                (level, offset, true)
            };
            first = offset.saturating_sub(skipped);
            let related: Vec<Node> = resolved
                .children
                .iter()
                .filter(|child| !level.children.iter().any(|read| read.id == child.id))
                .cloned()
                .collect();
            children.clone_from(&level.children);
            // What the resource names beside its directory follows the
            // directory's last page, once: a page read on from a later page
            // holds none of the first, which are no more related for that.
            if !level.unread() && from_start {
                children.extend(related);
            }
            read = Some((level, first));
        }
        let unread = read.as_ref().is_some_and(|(level, _)| level.unread());
        // Each entry is a resource's line, and its row; then the resource's
        // own actions, a line each. A page is a window of them.
        let mut entries: Vec<(String, Option<Value>)> = Vec::new();
        if want_resources {
            // Whether a read answers the rows listed here without their
            // offering one: the resource they are listed under reads one of
            // its items at a time, as a notebook reads its notes. A row
            // neither that nor its own actions read is only a place inside
            // what lists it (an option, a choice), which the row says.
            let inherited_read = read
                .as_ref()
                .map(|(level, _)| &level.own)
                .or_else(|| resolved.node.is_none().then_some(&resolved.actions))
                .and_then(|own| reads_action(own))
                .is_some_and(|read| {
                    read.params
                        .iter()
                        .any(|param| param.name.as_ref() == "item")
                });
            let mut named: Vec<Node> = children.clone();
            named.extend(resolved.node.clone());
            let shared = shared_names(&children, &named);
            entries.resize(first, (String::new(), None));
            for (index, child) in children.iter().enumerate() {
                let index = first.saturating_add(index);
                if index < offset || index >= end {
                    // Off this page: only counted.
                    entries.push((String::new(), None));
                    continue;
                }
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
                let name = child.name(&named);
                // A title two rows share does not say which is which: the row's
                // own id does, beside it.
                if shared.contains(&name.trim().to_lowercase()) {
                    let _ = write!(line, "  {}", child.id);
                }
                let listed: Vec<ActionDescriptor> = child.listed_actions().cloned().collect();
                if !listed.is_empty() {
                    let called = |action| forms.called(action, &resolved.app_id);
                    let _ = write!(line, "  {}", actions_note(&listed, &called));
                }
                let row = child.listing_row(
                    &resolved.path,
                    &name,
                    forms.kind(&child.kind).unwrap_or(&child.kind),
                    inherited_read || reads_action(&child.actions).is_some(),
                );
                entries.push((line, Some(row)));
            }
            let (mut lines, mut rows) = (Vec::new(), Vec::new());
            (self.other_resources(&parsed, &resolved, (&mut lines, &mut rows))).await;
            entries.extend(lines.into_iter().zip(rows.into_iter().map(Some)));
        }
        // What acts on an item this resource holds is listed on that item,
        // bound to it; a call naming the item still finds it here.
        let own: Vec<ActionDescriptor> = resolved
            .actions
            .iter()
            .filter(|action| !action.contract.needs_an_item())
            .cloned()
            .collect();
        if want_actions {
            entries.extend(
                own.iter()
                    .map(|action| (action_line(action, called(action)), None)),
            );
        }
        let total = entries.len();
        // The actions of each kind of resource on the page, once: a row names
        // the ones it offers, by id. Each is named here; what one takes in full is
        // answered where that one action is asked for.
        let mut kinds = serde_json::Map::new();
        for child in children
            .iter()
            .skip(offset.saturating_sub(first))
            .take(limit)
        {
            let kind = forms.kind(&child.kind).unwrap_or(&child.kind).to_owned();
            let listed = kinds.entry(kind).or_insert_with(|| json!([]));
            if let Value::Array(listed) = listed {
                for action in child.listed_actions() {
                    if !listed
                        .iter()
                        .any(|row| row["id"] == action.address.action.as_str())
                    {
                        listed.push(action_named(action));
                    }
                }
            }
        }
        let mut text = resolved.path.clone();
        // What this is, by the same name its own row here would give it.
        if let Some(node) = &resolved.node {
            let _ = write!(text, "  ({})", forms.kind(&node.kind).unwrap_or(&node.kind));
        } else {
            let _ = write!(text, "  ({})", resolved.app_id);
        }
        let mut rows: Vec<Value> = Vec::new();
        for (line, row) in entries.into_iter().skip(offset).take(limit) {
            text.push('\n');
            text.push_str(&line);
            rows.extend(row);
        }
        // The next page starts at the directory page its first resource
        // was read in, when one was.
        let next = (end < total || unread).then(|| {
            let resume = read.as_ref().and_then(|(level, first)| {
                let node = resolved.node.as_ref()?;
                level
                    .row_mark(&node.id, end.checked_sub(*first)?)
                    .map(|(cursor, revision)| {
                        (
                            PageMark {
                                row: 0,
                                cursor,
                                revision,
                            },
                            0,
                        )
                    })
            });
            LsCursor {
                offset: end,
                resume,
            }
            .write()
        });
        if let Some(next) = &next {
            if unread {
                let _ = write!(text, "\n… more: cursor \"{next}\"");
            } else {
                let _ = write!(
                    text,
                    "\n… {} more: cursor \"{next}\"",
                    total.saturating_sub(end)
                );
            }
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
                "rows": rows,
                "actions": actions_named(&own),
                "kinds": kinds,
            });
            if let (Some(fields), Some(next)) = (answer.as_object_mut(), next) {
                fields.insert("next_cursor".into(), json!(next));
            }
            reply.structured = Some(answer);
        }
        Ok(reply)
    }

    /// The surface `app_id` publishes: the one this process runs it with, or
    /// the signed package the workspace has installed.
    async fn surface_bytes(&self, app_id: &str) -> Option<Vec<u8>> {
        if let Some(bytes) = collab_types::app_surface::bytes_of(app_id) {
            return Some(bytes.to_vec());
        }
        if let Ok(installed) = self.installed().await
            && let Some(surface) = installed
                .iter()
                .find(|app| app.app_id == app_id)
                .and_then(|app| app.surface.clone())
        {
            return Some(surface.into_bytes());
        }
        // An app of the fleet answers what its signed package publishes even
        // when this computer has neither started it nor kept its package: the
        // fleet compiles each app's surface in, so every installed app's types
        // are readable on a fresh workspace.
        if let Some(surface) = app_fleet::surface(app_id) {
            return Some(surface.as_bytes().to_vec());
        }
        self.host.installed_surface(app_id).await.flatten()
    }

    /// What a typed read of an app answers when no resource is named: the
    /// surface the app publishes, which a generated SDK's types are read
    /// from.
    fn app_surface(path: &str, app_id: &str, surface: &[u8]) -> Reply {
        let mut reply = Reply::ok(String::new());
        reply.structured = Some(json!({
            "path": path,
            "kind": "app",
            "app": app_id,
            "surface": String::from_utf8_lossy(surface),
        }));
        reply
    }

    async fn agent_read(&self, args: ReadArgs, structured: bool) -> Result<Reply, Failure> {
        if let Some((app, record)) = tilo_actions::remote::resource_parts(&args.path) {
            // Boxed: the wait it holds is no part of every agent call's future.
            return Box::pin(self.agent_record((app, record), args.parts, args.after)).await;
        }
        let parsed = parse_path(&args.path)?;
        if parsed.action.is_some() {
            return Err("tilo_read reads a resource; run an action with tilo_do".into());
        }
        // A bare app answers what it publishes whether or not it runs: the
        // signed package's surface is what its types are read from, and what
        // its records are decided against, before its first window.
        if structured
            && parsed.segments.is_empty()
            && let Ok(AppRef::Installed { app_id, .. } | AppRef::Running { app_id, .. }) =
                self.app_ref(&parsed.app).await
            && let Some(surface) = self.surface_bytes(&app_id).await
        {
            return Ok(Self::app_surface(&parsed.app, &app_id, &surface));
        }
        let (mut resolved, forms) = match self.resolve(&parsed).await? {
            (Ok(resolved), forms) => (resolved, forms),
            (Err(note), _) => return Ok(Reply::ok(note)),
        };
        self.acted_in(&resolved.provider, &resolved.app_id);
        // A typed read of an app, with no resource named, answers what the app publishes: the
        // surface its resources and actions are declared in, which a generated SDK's types are
        // read from (`tilo sdk gen -w`).
        if structured
            && resolved.node.is_none()
            && let Some(surface) = self.surface_bytes(&resolved.app_id).await
        {
            return Ok(Self::app_surface(
                &resolved.path,
                &resolved.app_id,
                &surface,
            ));
        }
        let provider = resolved.provider.clone();
        let mut levels = std::mem::take(&mut resolved.levels);
        let Some((owner, reader)) = self.reader(&resolved, &mut levels).await? else {
            // A read its caller's rules keep off their directory is theirs to be refused: the
            // app still publishes one, and the host answers whether this turn may read it.
            if let Some(refused) = self.refused_read_answer(&resolved).await? {
                return Ok(refused);
            }
            return Ok(no_read_action(&resolved));
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
            self.held_for(None),
            &mut None,
        ))
        .await?;
        let answered = outcome(&value);
        if answered.failed {
            return Ok(answered);
        }
        // What a document says is whoever wrote it's, and what its provider says carries
        // content from outside is that when it is read by a path too: either reaches a
        // model behind the label, every page of it.
        let outside = value.get(crate::UNTRUSTED).is_some() || reader.contract.reads_document();
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
            return Ok(if outside { reply.labelled() } else { reply });
        }
        let reply = Reply::ok(read_answer(
            &read,
            already,
            app_cursor.as_deref(),
            args.max_chars,
        ));
        Ok(if outside { reply.labelled() } else { reply })
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
        // The app's root resources: a notebook reads its pages. A read that
        // names no item answers a document whole, and a resource inside it is
        // that read's to answer: a listing that takes no item would answer
        // every item with the whole document.
        //
        // Which root resource that is depends on the app's directory alone, so
        // it is looked for once an app's directory stands still: a path that no
        // root resource reads otherwise reads every one of them, and a walk
        // over an app's empty places would pay that for each.
        let sweep = (resolved.provider.clone(), resolved.revision.clone());
        if !sweep.1.is_empty()
            && let Some(known) = self.swept_reads.lock_recover().get(&sweep).cloned()
        {
            return Ok(known);
        }
        let root = self.level(&resolved.provider, "", levels).await?;
        let reading_item = |found: &ActionDescriptor| {
            resolved.node.is_none()
                || found
                    .params
                    .iter()
                    .any(|param| param.name.as_ref() == "item")
        };
        let mut found = None;
        for node in &root.children {
            let level = self.level(&resolved.provider, &node.id, levels).await?;
            if let Some(reading) = reads_action(&level.own)
                .or_else(|| reads_action(&node.actions))
                .filter(reading_item)
            {
                found = Some((node.id.clone(), reading));
                break;
            }
        }
        if !sweep.1.is_empty() {
            let mut swept = self.swept_reads.lock_recover();
            if swept.len() >= SWEPT_READS {
                swept.clear();
            }
            swept.insert(sweep, found.clone());
        }
        Ok(found)
    }

    async fn agent_do(&self, args: DoArgs, structured: bool) -> Result<Reply, Failure> {
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
                    return Err(format!("calls must hold 1 to {MAX_CALLS} steps").into());
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
                                "links": link::step_links(&steps),
                                "steps": steps,
                            }));
                        }
                        return Ok(reply);
                    }
                }
                let mut reply = Reply::ok(text);
                if structured {
                    // Every record the steps changed, each once: what a
                    // person is handed to review them.
                    let links = link::step_links(&steps);
                    reply.structured = Some(json!({"status":"ok","steps":steps,"links":links}));
                }
                Ok(reply)
            }
            _ => Err("name a path (with #action) or calls".into()),
        }
    }

    /// Run the one action `path` names.
    /// The answer to a call that did not run to its end: held for the person, who allows it
    /// or not in their tab (a watch is told so at once, the typed caller is answered pending,
    /// and the same id answers how it ended once they did), or refused.
    fn not_done(
        &self,
        failure: Failure,
        path: &str,
        asked: Option<&str>,
        structured: bool,
    ) -> Result<Reply, Failure> {
        let id = asked.unwrap_or_default();
        if !(path.contains('#') && super::confirm::needs_the_person(&failure)) {
            if asked.is_some() {
                self.watch_invocation_ended(id, "refused");
            }
            return Err(failure);
        }
        self.watch_invocation_held(id);
        if !structured {
            return Err(failure);
        }
        let mut reply = Reply::ok(String::from(failure));
        reply.failed = false;
        reply.structured = Some(json!({
            "status": "pending",
            "invocation_id": id,
            "path": path,
            "ask": {"invocation_id": id},
        }));
        Ok(reply)
    }

    async fn do_one(
        &self,
        path: &str,
        arguments: serde_json::Map<String, Value>,
        invocation: Option<String>,
        wait_ms: Option<u64>,
        structured: bool,
    ) -> Result<Reply, Failure> {
        // Every call is an invocation of the session's own, named here when
        // the caller named none, so `tilo_status` and a watch know it too.
        let invocation =
            Some(invocation.unwrap_or_else(|| tilo_actions::InvocationId::new().0.to_string()));
        let asked = invocation.clone();
        if let Some(id) = asked.as_deref() {
            self.watch_invocation_started(id, path);
        }
        let answer = Box::pin(self.do_one_value(path, arguments, invocation, wait_ms)).await;
        let answer = match answer {
            Err(failure) => return self.not_done(failure, path, asked.as_deref(), structured),
            Ok(answer) => answer,
        };
        match answer {
            Done::Launched {
                value,
                mut reply,
                not_shown,
            } => {
                if let Some(id) = asked.as_deref() {
                    self.watch_invocation_ended(
                        id,
                        value
                            .pointer("/update/state")
                            .and_then(Value::as_str)
                            .unwrap_or("failed"),
                    );
                }
                let mut shown = self.opened_link(&value).await;
                shown.not_shown = not_shown;
                let typed = structured.then(|| answer_of(&value));
                reply.structured = link::linked(&mut reply, typed, shown);
                Ok(reply)
            }
            Done::Invoked {
                value,
                path,
                descriptor,
                app,
                provider,
                history,
            } => {
                let mut reply = outcome(&value);
                if reply.failed && reply.text.contains("invalid_arguments") {
                    let _ = write!(reply.text, "\n{}", action_page(&path, &descriptor, None));
                }
                if let Some(id) = asked.as_deref() {
                    self.watch_invocation_ended(
                        id,
                        value
                            .pointer("/update/state")
                            .and_then(Value::as_str)
                            .unwrap_or("failed"),
                    );
                }
                let shown = self
                    .invoked_link(asked.as_deref(), &value, &provider, &descriptor)
                    .await;
                link::tell(&mut reply, &shown);
                if structured {
                    let mut typed = answer_of(&value);
                    link::put(&mut typed, shown);
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
                if !reply.failed && descriptor.contract.kind != tilo_actions::ActionKind::Read {
                    self.note_change(&app, history);
                }
                Ok(reply)
            }
        }
    }

    /// The app a kind names (`tasks.tracker` is the Tasks app's tracker), as
    /// the app id and the provider serving it: a kind begins with the app it
    /// belongs to. `None` when the name is no kind or its app does not run.
    async fn kind_app(&self, name: &str) -> Option<(String, String, bool)> {
        let app = kind_address(name)?;
        let AppRef::Running {
            app_id,
            provider,
            pinned,
        } = self.app_ref(app).await.ok()?
        else {
            return None;
        };
        if pinned {
            return Some((app_id, provider, pinned));
        }
        // What the kind names, never one of the app's other documents that happens to be first:
        // a kind's directory lists a resource of it (`tasks.issues` is the issues' chain, and
        // `tasks.tracker` the tracker's own document). The app's documents are asked in turn,
        // newest first, and the first that lists the kind answers.
        let kind = name
            .strip_prefix(app)
            .and_then(|rest| rest.strip_prefix('.'))?
            .to_owned();
        let naming = |node: &Node| node.kind == kind || node.kind.starts_with(&format!("{kind}."));
        let mut candidates = vec![provider.clone()];
        if let Ok((_, running)) = self.providers().await {
            candidates.extend(
                running
                    .iter()
                    .rev()
                    .filter(|(_, app)| app["app_id"].as_str() == Some(app_id.as_str()))
                    .map(|(provider, _)| provider.clone()),
            );
        }
        candidates.dedup();
        for provider in candidates {
            let mut levels = Levels::new();
            let Ok(level) = self.level(&provider, "", &mut levels).await else {
                continue;
            };
            if level.children.iter().any(naming) {
                return Some((app_id, provider, false));
            }
        }
        Some((app_id, provider, false))
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
        let reference = match self.app_ref(&parsed.app).await {
            Ok(reference) => reference,
            // An action's answer names the resource it made by its kind
            // (`tasks.tracker/issues/7`), which is a path the next command
            // takes as it is.
            Err(why) => match self.kind_app(&parsed.app).await {
                Some((app_id, provider, pinned)) => {
                    return Ok(Some((app_id, provider, pinned)));
                }
                None => return Err(why),
            },
        };
        match reference {
            AppRef::Installed { opens: false, .. } => {
                Err(Self::not_running_note(&parsed.app, false))
            }
            AppRef::Installed { .. } if starts_app(action) && parsed.segments.is_empty() => {
                Ok(None)
            }
            // Any other action, on the app or on a resource inside it, starts the app without a
            // window first and is then done where its path points: a fresh workspace's first
            // `notes/Notes#new-note` works the way the refusal for a read says it will.
            AppRef::Installed { app_id, .. } => {
                let provider = self.started_provider(&app_id, wait_ms).await?;
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
    /// The program forms an app declares, when `parsed` names a segment by one.
    async fn forms_named(&self, parsed: &ParsedPath) -> Option<ProgramForms> {
        if parsed
            .segments
            .iter()
            .any(|segment| is_program_form(segment))
        {
            self.program_forms().await
        } else {
            None
        }
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
            // The app's own level first: an action one of its top resources
            // offers by that id (`notes#create-page` is the notebook's) is
            // found without reading what the containers hold, so finding it
            // costs the same however many notes there are.
            let top = self.level(&resolved.provider, "", &mut levels).await?;
            if let Some((path, action)) =
                only_by_id(name, &published_on(&top.children, &[], &levels))
            {
                return Ok((format!("{}/{path}", resolved.path), action.clone()));
            }
            let root = self.app_children(&resolved.provider, &mut levels).await?;
            // What a container publishes of its own, read with what it holds,
            // is the app's as its root's own actions are.
            if let Ok(action) = pick_action(name, &root.own, &resolved.path) {
                return Ok((resolved.path.clone(), action.clone()));
            }
            // The containers at the app's top hold what is one level down;
            // what they hold is listed beside them already, and what that
            // holds in turn is a walk of the app.
            let mut held = Vec::new();
            for container in root
                .children
                .iter()
                .filter(|node| node.container && node.parent.is_empty())
            {
                let inside = self
                    .level(&resolved.provider, &container.id, &mut levels)
                    .await?;
                held.push((container.clone(), inside.children));
            }
            published = published_on(&root.children, &held, &levels);
        }
        let ranked: Vec<(u8, String, &ActionDescriptor)> = published
            .iter()
            .filter_map(|(path, action)| Some((action_rank(name, action)?, path.clone(), action)))
            .collect();
        let (path, action) = match pick_action_anywhere(name, resolved, &ranked) {
            Ok(found) => found,
            Err(error) => {
                return Err(self.kept_here(name, resolved).await.unwrap_or(error));
            }
        };
        // The path to the resource it is published on; the action follows
        // its `#`, as every path's does.
        let named_at = match path {
            Some(path) => format!("{}/{}", resolved.path, path),
            None => resolved.path.clone(),
        };
        Ok((named_at, action.clone()))
    }

    /// Why `name` is refused when it names, by its id, an action the app keeps for the person at
    /// its device, wherever in the app it stands: such an action is never listed to an agent,
    /// and a call of it is told whose it is rather than that the app has no such action. `None`
    /// when the path names a resource (its own actions were asked by id already) or the app
    /// keeps none by that id.
    async fn kept_here(&self, name: &str, resolved: &Resolved) -> Option<String> {
        if resolved.node.is_some() {
            return None;
        }
        let page = self
            .ask(CatalogueQuery {
                provider: Some(resolved.provider.clone()),
                action: Some(name.to_owned()),
                ..CatalogueQuery::default()
            })
            .await
            .ok()?;
        page.rows
            .into_iter()
            .filter_map(|row| row.action)
            .find_map(|found| super::refusal::not_allowed(found).err())
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
        page.rows
            .into_iter()
            .filter_map(|row| row.action)
            .find(|found| found.address.resource == node.id && found.address.action == action)
            .map(super::refusal::not_allowed)
            .transpose()
    }
    /// The provider of a running instance of the installed app `app_id`, starting the app in
    /// the workspace when none runs yet, without a window: where an invocation of the app's own
    /// API goes. The step shows nothing, so nothing is opened for it.
    async fn started_provider(&self, app_id: &str, wait_ms: Option<u64>) -> Result<String, String> {
        match self.app_ref(app_id).await? {
            AppRef::Running { provider, .. } => Ok(provider),
            AppRef::Installed { app_id, opens } => {
                if !opens {
                    return Err(Self::may_not_open(&app_id));
                }
                // A start still pending is followed under its own id, which
                // answers how it went rather than starting the app again: a
                // workspace starting many apps at once (hundreds of fresh ones
                // on a host, each its first) takes longer than one wait, and
                // the call waits for its app rather than failing.
                let start = uuid::Uuid::new_v4().to_string();
                let deadline = std::time::Instant::now()
                    .checked_add(std::time::Duration::from_millis(super::MAX_WAIT_MS));
                let mut value = Value::Null;
                for first in [true].into_iter().chain(std::iter::repeat(false)) {
                    value = self
                        .app_in_workspace(
                            &app_id,
                            tilo_actions::remote::APPLICATION_START_ACTION,
                            serde_json::Map::new(),
                            Some(start.clone()),
                            if first { wait_ms } else { None },
                        )
                        .await?;
                    let pending = !matches!(
                        value.pointer("/update/state").and_then(Value::as_str),
                        Some("succeeded" | "failed" | "cancelled")
                    );
                    if !pending || deadline.is_none_or(|end| std::time::Instant::now() >= end) {
                        break;
                    }
                }
                if value.pointer("/update/state").and_then(Value::as_str) != Some("succeeded") {
                    return Err(format!("{app_id} did not start: {}", outcome(&value).text));
                }
                // The instance the workspace started, as it names it; a host serving the
                // workspace's records names the record it started the app on.
                value
                    .pointer(super::OPENED_PEER)
                    .and_then(Value::as_u64)
                    .map(|peer| peer.to_string())
                    .or_else(|| {
                        value
                            .pointer("/update/output/value/provider")
                            .and_then(Value::as_str)
                            .filter(|provider| tilo_actions::remote::is_resource_provider(provider))
                            .map(str::to_owned)
                    })
                    .ok_or_else(|| format!("{app_id} started, but named no instance of it"))
            }
        }
    }
}

/// Whether `action`, named on an app itself, starts it: `open` shows it in a window, `start`
/// runs it without one.
/// `why` naming `code`, unless it already names the documented code it was
/// refused with.
fn refused_as(code: &str, why: String) -> String {
    if answer::coded(&why).0 == "failed" {
        format!("{code}: {why}")
    } else {
        why
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

#[path = "mcp_agent_level.rs"]
mod level;

#[path = "mcp_agent_resolved.rs"]
pub(super) mod resolved;
#[path = "mcp_agent_unoffered.rs"]
mod unoffered;

/// The answer to a read of `resolved` when it publishes no read action: the actions it has, and
/// where to look inside it.
fn no_read_action(resolved: &Resolved) -> Reply {
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
    Reply::failed(text)
}

#[cfg(test)]
#[path = "mcp_agent_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "mcp_agent_read_tests.rs"]
mod read_tests;

#[cfg(test)]
#[path = "mcp_agent_surface_tests.rs"]
mod surface_tests;

#[cfg(test)]
#[path = "mcp_agent_list_tests.rs"]
mod list_tests;

#[cfg(test)]
#[path = "mcp_agent_link_tests.rs"]
mod link_tests;

#[cfg(test)]
#[path = "mcp_watch_tests.rs"]
mod watch_tests;

#[cfg(test)]
#[path = "mcp_agent_publisher_tests.rs"]
mod publisher_tests;

#[cfg(test)]
#[path = "mcp_lost_answer_tests.rs"]
mod lost_answer_tests;
