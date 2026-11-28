//! The agent surface: the tools an AI member can drive any app with.
//!
//! `tilo_ls`, `tilo_read`, `tilo_do`, `tilo_find`, `tilo_link` and `tilo_status` address
//! everything by **path**: `notes/Todo/body#append-paragraph` is the app
//! (`notes`), the resource by its title, kind or key (`Todo`, `body`) and the
//! action after `#`. `notes@4711/Todo#append-paragraph` names one window of an
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
    ActionProvider, Addressed, CatalogueQuery, DEFAULT_WAIT_MS, Deserialize, Failure, MAX_CALLS,
    McpServer, OneCall, RemoteActionTarget, Shell, Value, batch_invocation, invocation_id, json,
    page, parse,
};
use std::collections::HashMap;
use std::fmt::Write as _;
use tilo_actions::ActionDescriptor;
use tilo_lock::MutexRecover as _;

#[path = "mcp_answer.rs"]
mod answer;
#[path = "mcp_ask.rs"]
mod ask;
#[path = "mcp_agent_find.rs"]
mod find;
#[path = "mcp_agent_launch.rs"]
mod launch;
use launch::{opens_a_file, starts_app};
use tilo_actions::tree::{DEEP_READS, kept_by, names_files};
#[path = "mcp_agent_link.rs"]
mod link;
#[path = "mcp_agent_list.rs"]
mod list;
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
use render::{action_line, action_page, block_line, parse_read_cursor, read_answer, strip_blank};
pub use tilo::TaskHosts;
pub(super) use tilo::TaskLink;
use tilo::actions_note;
pub use tilo::{Go, TaskProgram};
/// A person's own line in a run's conversation, run in the task's host as they are
/// (shell.md §8.1): what the API's `person_line` hands on.
pub use tilo::{PersonStep, literals_in};
pub(super) use tilo::{ProgramForms, Programs};

#[cfg(test)]
use super::CatalogueRow;
use tilo_actions::tree::ROOT_PEEK;
/// Lines `tilo_ls` answers before a cursor: as many as the tree reads a level near for.
const LS_LINES: usize = tilo_actions::tree::LISTED_NEAR;
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
/// The most rows one page of a listing answers: a bound on what one call holds and sends,
/// and enough that a tracker of a company's issues is listed in a couple of calls.
const LS_PAGE_MAX: usize = 1_000;
/// Longest label shown whole in a listing line.
const LABEL_CHARS: usize = 60;
/// Longest first sentence of an action's description in an action's page.
const DESCRIPTION_CHARS: usize = 200;

/// The verbs of the workspace's own shell: what the device that keeps the workspace's machine
/// runs on a session record, at `record:so.tilo.terminal:<record>`.
const SHELL_VERBS: [&str; 3] = ["run", "stop", OPEN_TERMINAL];

/// The verb that opens the workspace's shell on a terminal for the device that asks.
const OPEN_TERMINAL: &str = "open-terminal";

/// Whether `name` names Terminal, whose sessions' shell the device that keeps the workspace's
/// machine serves: its id, or the last part of it, in any case.
fn names_terminal(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case("terminal")
        || name
            .trim()
            .eq_ignore_ascii_case(tilo_app_id::apps::TERMINAL)
}

#[path = "mcp_agent_arguments.rs"]
mod arguments;
use arguments::{DoArgs, FindArgs, LsArgs, ReadArgs};

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
    /// The action ran: the host's answer and the action at its resolved path.
    Invoked {
        value: Value,
        path: String,
        descriptor: Box<ActionDescriptor>,
        /// Where it ran: the document whose link a change answers.
        provider: String,
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

use mcp_agent_pick::{pick_action_anywhere, published_on};
pub(super) use mcp_agent_tools::{held_until_ended, tool_definitions};
use tilo_actions::tree::{
    Address, AppRef, CLOSE_MATCHES, InstalledApp, Level, Levels, LsCursor, Names, Node, PageMark,
    Resolved, TypeNames, action_rank, app_names, app_of, below_app, bound_key, is_program_form,
    list_names, listed_as, named_beyond_its_start, only_by_id, path_name, pick_action,
    reads_action, shared_names,
};

/// `forms` as the names a pick reads types by.
fn typed(forms: Option<&ProgramForms>) -> Option<&dyn TypeNames> {
    forms.map(|forms| forms as &dyn TypeNames)
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
                    "kind": node.resource_type,
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
    /// `value`, an app's read or an action's output, taken apart: a list    /// whose every entry names an item and a title, or holds a collection's
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

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
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
            .tree
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
            .tree
            .level(&provider.provider, resource, &mut levels)
            .await?;
        Ok(level
            .children
            .iter()
            .map(|node| {
                json!({
                    "id": node.id,
                    "kind": node.resource_type,
                    "key": node.key,
                    "name": node.label,
                })
            })
            .collect())
    }
}

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// Answer one agent tool call.
    /// `structured` when the caller asked for typed answers
    /// (`_meta["so.tilo.mcp"].structured`): a read answers its
    /// [`ResourceRead`].
    pub(super) async fn agent_call(&self, name: &str, args: Value, structured: bool) -> Value {
        // A program may have run steps before the one refused: running it
        // again would run them twice.
        text_result(
            self.agent_reply(name, args, structured).await,
            structured,
            name != "tilo",
        )
    }

    /// One agent tool call's typed result, or its typed refusal: what a program reads, never
    /// the words an agent reads. A program's run is its result however it ended.
    pub(super) async fn agent_typed(
        &self,
        name: &str,
        args: Value,
    ) -> Result<Value, super::Refused> {
        let program = name == "tilo";
        match self.agent_reply(name, args, true).await {
            Ok(Reply {
                structured: Some(typed),
                ..
            }) if program || typed.get("status").and_then(Value::as_str) != Some("failed") => {
                Ok(typed)
            }
            Ok(Reply {
                structured: Some(typed),
                ..
            }) => Err(super::Refused {
                error: typed.get("error").cloned().unwrap_or_default(),
                action: typed.get("action").cloned(),
            }),
            // Said in words alone: what the path names holds nothing to type (an app installed
            // and not running), or the call failed before anything ran.
            Ok(reply) => Err(super::Refused::new(
                if reply.failed { "failed" } else { "not_found" },
                reply.text,
            )),
            Err(failure) if program => Ok(answer::refused(&failure, false)),
            Err(failure) => Err(super::Refused {
                error: answer::refused(&failure, true)
                    .get("error")
                    .cloned()
                    .unwrap_or_default(),
                action: None,
            }),
        }
    }

    /// What one agent tool call did: its words, and its typed result when `structured`.
    pub(super) async fn agent_reply(
        &self,
        name: &str,
        mut args: Value,
        structured: bool,
    ) -> Result<Reply, Failure> {
        // A path written as what a person types to name a resource (`#12`) is
        // read as the address the tools list it at before the call resolves
        // it, so an agent going on from a prompt's words reaches the resource.
        if let Some(path) = args.get("path").and_then(Value::as_str).map(str::to_owned) {
            let expanded = self.expand_reference(&path).await?;
            if let Some(fields) = args.as_object_mut() {
                fields.insert("path".into(), Value::String(expanded));
            }
        }
        match name {
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
        }
    }

    /// Note the record a call resolved `in`: the app's own record, when the workspace serves the
    /// app's records under the app's id. A call whose resource no record provider answers leaves
    /// the last one named: the agent still acts there.
    fn acted_in(&self, provider: &str, app_id: &str) {
        // The workspace's shell is the device that serves it answering, not an instance of the
        // app: the app's name still finds its own directory, and its calls here move nothing.
        if app_id != tilo_app_id::apps::TERMINAL {
            // The app answered where it runs: the calls after this one reach it there, rather than
            // looking up again whether it runs and starting it when it does not.
            self.tree.saw_running(app_id, provider);
        }
        if provider != app_id {
            return;
        }
        *self.acted.lock_recover() = Some(super::ActedRecord {
            app: app_id.to_owned(),
            record: String::new(),
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
                after: None,
            },
            Some(cursor) => LsCursor::parse(cursor)
                .ok_or_else(|| "cursor is not one tilo_ls issued".to_owned())?,
        };
        let offset = cursor.offset;
        let limit = args.limit.unwrap_or(LS_LINES).clamp(1, LS_PAGE_MAX);
        // What this page lists up to: how far an app's own page is read for it.
        let end = offset.saturating_add(limit);
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
        if let Some(app) = path
            .strip_prefix("record:")
            .filter(|app| !app.contains(':'))
        {
            return Box::pin(self.agent_records(app, structured)).await;
        }
        let parsed = Address::parse(path)?;
        // How a program writes what this lists: the listing is the one a program reads, and a
        // path names what it lists the same way, so the path is resolved by it too.
        let forms_timed = super::timing_start();
        let forms = self.program_forms().await.unwrap_or_default();
        super::timing_done("ls.forms", forms_timed);
        let resolving = super::timing_start();
        // A page read on from one before it lists what the app's path named when that page
        // was read, by the resolution kept for the directory revision its cursor names: the
        // app's directory has not moved on since, or its pages would not be read from it. The
        // path is resolved again only when no page of that revision was listed.
        let mut reused = cursor
            .resume
            .as_ref()
            .and_then(|(mark, _)| self.tree.listing_resolution(path, &mark.revision));
        let cached = reused.is_some();
        let mut resolved = match reused.take() {
            Some(resolved) => resolved,
            None => std::sync::Arc::new(
                if names_files(&app_of(&parsed)) && parsed.action.is_none() {
                    self.files_resolved(&parsed, &forms, end).await?
                } else {
                    match self.resolve_with(&parsed, Some(&forms), end).await? {
                        Ok(resolved) => resolved,
                        Err(note) => {
                            super::timing_done("ls.resolve", resolving);
                            return Ok(Reply::ok(note));
                        }
                    }
                },
            ),
        };
        super::timing_done("ls.resolve", resolving);
        self.acted_in(&resolved.provider, &resolved.app_id);
        // By value: a page that reads the directory as it moved on resolves the path again
        // below, and what it lists is still named under the same app.
        let listed_app = resolved.app_id.clone();
        let called = |action: &ActionDescriptor| forms.called(action, &listed_app);
        if let Some(action) = &parsed.action {
            let (named_at, found) = match self.pick_action_in(action, &resolved).await {
                Ok(found) => found,
                Err(error) if error.contains(&format!(" has no action {action}")) => {
                    return Err(self
                        .not_offered_here(&resolved.app_id, action)
                        .await
                        .unwrap_or_else(|| format!("unknown_action: {error}"))
                        .into());
                }
                Err(error) => return Err(error.into()),
            };
            let mut reply = Reply::ok(action_page(&named_at, &found, called(&found)));
            if structured {
                reply.structured = Some(json!({
                    "path": named_at,
                    "kind": resolved
                        .node
                        .as_ref()
                        .map_or(resolved.app_id.as_str(), |node| {
                            forms.kind(&node.resource_type).unwrap_or(&node.resource_type)
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
        let mut children = resolved.children.clone();
        // The index of `children`'s first among everything listed: a page
        // read on from where the last one ended starts past the start.
        let mut first = 0usize;
        let mut read: Option<(Level, usize)> = None;
        // Whether this page had to read the directory again from its first
        // page because the app published a change since the page before it.
        let mut restarted = false;
        // The absolute index this page starts at: the cursor's offset, or the
        // row after the one the page before ended at when a change moved the
        // rows before it.
        let mut page_start = offset;
        if want_resources && resolved.node.is_some() {
            let resumed = match (&cursor.resume, &resolved.node) {
                (Some((mark, skip)), Some(node)) if offset > 0 => self
                    .tree
                    .level_from(
                        &resolved.provider,
                        &node.id,
                        mark.clone(),
                        skip.saturating_add(limit),
                    )
                    .await?
                    .map(|level| {
                        // Where writes put resources before the row it reads on from, the
                        // resource the page before ended at is found again: this page starts
                        // after it.
                        let past = cursor.after.as_deref().and_then(|after| {
                            level.children.iter().position(|child| child.id == after)
                        });
                        let skip = past.map_or(*skip, |at| at.saturating_add(1).max(*skip));
                        (level, skip)
                    })
                    .filter(|(level, skip)| level.children.len() >= *skip),
                _ => None,
            };
            if resumed.is_none() && cached && cursor.resume.is_some() {
                // The app's directory moved on since that page was read, so this page lists
                // what the path names now: resolved again, and read from its first page.
                restarted = true;
                match self.resolve_with(&parsed, Some(&forms), end).await? {
                    Ok(fresh) => resolved = std::sync::Arc::new(fresh),
                    Err(note) => return Ok(Reply::ok(note)),
                }
            }
            if let Some(node) = resolved.node.clone()
                && let Some(level) = resolved.levels.get(&node.id).cloned()
            {
                let (level, skipped, from_start) = if let Some((level, skip)) = resumed {
                    (level, skip, false)
                } else {
                    let reading = super::timing_start();
                    let mut level = self
                        .tree
                        .level_reaching(&resolved.provider, &node.id, level, end)
                        .await?;
                    // This page carries on after the row the one before ended at, wherever a
                    // change since put it; only if that row is gone does the offset stand.
                    let skipped = cursor
                        .after
                        .as_deref()
                        .and_then(|after| {
                            level
                                .children
                                .iter()
                                .position(|child| child.id == after)
                                .map(|at| at.saturating_add(1))
                        })
                        .unwrap_or(offset);
                    if skipped.saturating_add(limit) > level.children.len() && level.unread() {
                        level = self
                            .tree
                            .level_reaching(
                                &resolved.provider,
                                &node.id,
                                level,
                                skipped.saturating_add(limit),
                            )
                            .await?;
                    }
                    super::timing_done("ls.level", reading);
                    page_start = skipped;
                    (level, skipped, true)
                };
                first = page_start.saturating_sub(skipped);
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
        }
        let unread = read.as_ref().is_some_and(|(level, _)| level.unread());
        let page_end = page_start.saturating_add(limit);
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
            let titled = Names::of(children.iter().chain(&resolved.node));
            let shared = shared_names(&children, &titled);
            entries.resize(first, (String::new(), None));
            for (index, child) in children.iter().enumerate() {
                let index = first.saturating_add(index);
                if index < page_start || index >= page_end {
                    // Off this page: only counted.
                    entries.push((String::new(), None));
                    continue;
                }
                // A resource a program passes as a value goes by a reference, as an answer's.
                let mut line = self
                    .listed_reference(&child.resource_type, &child.id)
                    .map(|reference| format!("{reference}  "))
                    .unwrap_or_default();
                let name = child.name_in(&titled);
                line.push_str(&name);
                if child.container {
                    line.push('/');
                }
                // What the resource is, by the name a program types its type with
                // (`tasks.Task`): the one name a listing prints and a path resolves by, so a
                // row read off the listing walks back to this very resource. Never the
                // published kind (`tl.<app>.<Type>`), which no program writes.
                let _ = write!(
                    line,
                    "  {}",
                    forms
                        .kind(&child.resource_type)
                        .unwrap_or(&child.resource_type)
                );
                // A title two rows share does not say which is which: the row's
                // own id does, beside it.
                if shared.contains(&name.trim().to_lowercase()) {
                    let _ = write!(line, "  {}", child.id);
                }
                let listed: Vec<&ActionDescriptor> = child.listed_actions().collect();
                if !listed.is_empty() {
                    let called = |action| forms.called(action, &resolved.app_id);
                    let _ = write!(line, "  {}", actions_note(&listed, &called));
                }
                let row = child.listing_row(
                    &resolved.path,
                    &name,
                    forms
                        .kind(&child.resource_type)
                        .unwrap_or(&child.resource_type),
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
        let mut named_kinds: std::collections::HashSet<(&str, &str)> =
            std::collections::HashSet::new();
        for child in children
            .iter()
            .skip(page_start.saturating_sub(first))
            .take(limit)
        {
            let kind = forms
                .kind(&child.resource_type)
                .unwrap_or(&child.resource_type);
            let listed = kinds.entry(kind).or_insert_with(|| json!([]));
            if let Value::Array(listed) = listed {
                for action in child.listed_actions() {
                    if named_kinds.insert((kind, action.address.action.as_str())) {
                        listed.push(action_named(action));
                    }
                }
            }
        }
        let mut text = resolved.path.clone();
        // What this is, by the same name its own row here would give it.
        if let Some(node) = &resolved.node {
            let _ = write!(
                text,
                "  ({})",
                forms
                    .kind(&node.resource_type)
                    .unwrap_or(&node.resource_type)
            );
        } else {
            let _ = write!(text, "  ({})", resolved.app_id);
        }
        let mut rows: Vec<Value> = Vec::new();
        for (line, row) in entries.into_iter().skip(page_start).take(limit) {
            text.push('\n');
            text.push_str(&line);
            rows.extend(row);
        }
        // The next page starts at the directory page its first resource
        // was read in, when one was.
        let next = (page_end < total || unread).then(|| {
            let resume = read.as_ref().and_then(|(level, first)| {
                let node = resolved.node.as_ref()?;
                level.row_mark(&node.id, page_end.checked_sub(*first)?).map(
                    |(cursor, revision, skip)| {
                        (
                            PageMark {
                                row: 0,
                                cursor,
                                revision,
                            },
                            skip,
                        )
                    },
                )
            });
            // The resource this page ends at, when it ends in the directory read.
            let after = read.as_ref().and_then(|(level, first)| {
                let last = end.checked_sub(first.saturating_add(1))?;
                Some(level.children.get(last)?.id.clone())
            });
            LsCursor {
                offset: page_end,
                after: resume.as_ref().and(after),
                resume,
            }
        });
        // The page after this one lists by this path's resolution at the directory revision
        // its cursor names, rather than resolving the path again.
        if let Some(cursor) = &next
            && let Some((mark, _)) = &cursor.resume
        {
            self.tree
                .remember_listing_resolution(path, &mark.revision, &resolved);
        }
        let next = next.map(|cursor| cursor.write());
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
        if restarted {
            text.push_str("\nthe directory changed; this page reads on from the last row it read");
        }
        let mut reply = Reply::ok(text);
        if structured {
            let kind = resolved
                .node
                .as_ref()
                .map_or(resolved.app_id.as_str(), |node| {
                    forms
                        .kind(&node.resource_type)
                        .unwrap_or(&node.resource_type)
                })
                .to_owned();
            let mut answer = json!({
                "path": resolved.path,
                "kind": kind,
                "rows": rows,
                "actions": actions_named(&own),
                "kinds": kinds,
            });
            if restarted && let Some(fields) = answer.as_object_mut() {
                fields.insert("restarted".into(), json!(true));
            }
            if let (Some(fields), Some(next)) = (answer.as_object_mut(), next) {
                fields.insert("next_cursor".into(), json!(next));
            }
            reply.structured = Some(answer);
        }
        Ok(reply)
    }

    /// The surface `app_id` publishes, in the form it is kept by
    /// ([`collab_types::surface::canonical`]): the one this process runs it
    /// with, or the signed package the workspace has installed.
    async fn surface_bytes(&self, app_id: &str) -> Option<Vec<u8>> {
        let known = self.surfaces.lock_recover().get(app_id).cloned();
        if let Some(surface) = known {
            return Some((*surface).clone());
        }
        let found = match crate::offered::installed_surface(&self.tree, app_id).await {
            Some(surface) => Some(surface),
            None => self.host.installed_surface(app_id).await.flatten(),
        }?;
        {
            let mut surfaces = self.surfaces.lock_recover();
            if surfaces.len() >= tilo_actions::tree::RESOLVED_PATHS {
                surfaces.clear();
            }
            surfaces.insert(app_id.to_owned(), std::sync::Arc::new(found.clone()));
        }
        Some(found)
    }

    /// The refusal of `name` in `app_id` when the app declares it and this caller, which may
    /// list the app, is not offered it ([`crate::offered::not_offered`]); `None` when the
    /// refusal is the one for an action that does not exist.
    pub(crate) async fn not_offered_here(&self, app_id: &str, name: &str) -> Option<String> {
        let session = self.group().and_then(|group| group.session);
        let caller = crate::offered::caller_named(&self.caller, session.as_deref());
        crate::offered::not_offered(
            &self.tree,
            app_id,
            name,
            &caller,
            self.surface_bytes(app_id),
        )
        .await
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
        if let Some((app, record)) = record::read_path(&args.path) {
            // Boxed: the wait it holds is no part of every agent call's future.
            return Box::pin(self.agent_record((app, record), args.parts, args.after)).await;
        }
        let parsed = Address::parse(&args.path)?;
        if parsed.action.is_some() {
            return Err("tilo_read reads a resource; run an action with tilo_do".into());
        }
        // A bare app answers what it publishes whether or not it runs: the
        // signed package's surface is what its types are read from, and what
        // its records are decided against, before its first window.
        if structured
            && below_app(&parsed).is_empty()
            && let Ok(AppRef::Installed { app_id, .. } | AppRef::Running { app_id, .. }) =
                self.tree.app_ref(&app_of(&parsed)).await
            && let Some(surface) = self.surface_bytes(&app_id).await
        {
            return Ok(Self::app_surface(&app_of(&parsed), &app_id, &surface));
        }
        // A path in Files reads the workspace's own folder, wherever its tree is kept: a file
        // by its path, or what a folder holds. A window of Files shows another device's folder,
        // which a path in the workspace does not name.
        let (mut resolved, forms) = if names_files(&app_of(&parsed)) {
            let forms = self.program_forms().await.unwrap_or_default();
            let resolved = self.folder_resolved(&parsed, &forms, usize::MAX).await?;
            (resolved, Some(forms))
        } else {
            match self.resolve(&parsed).await? {
                (Ok(resolved), forms) => (resolved, forms),
                (Err(note), _) => return Ok(Reply::ok(note)),
            }
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
            resolved
                .node
                .as_ref()
                .map(|node| node.resource_type.as_str()),
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
            let level = self
                .tree
                .level(&resolved.provider, &ancestor.id, levels)
                .await?;
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
        let root = self.tree.level(&resolved.provider, "", levels).await?;
        let reading_item = |found: &ActionDescriptor| {
            resolved.node.is_none()
                || found
                    .params
                    .iter()
                    .any(|param| param.name.as_ref() == "item")
        };
        // The app's own root first: an app's directory publishes the workspace's documents, so
        // a document of a package the app's code uses may stand before the app's own. A read of
        // the app itself answers the app's own root resource.
        let app = resolved.app_id.clone();
        let own = |id: &str| kept_by(id, &app);
        let mut found = None;
        for own_first in [true, false] {
            for node in root
                .children
                .iter()
                .filter(|node| own(&node.id) == own_first)
            {
                let level = self
                    .tree
                    .level(&resolved.provider, &node.id, levels)
                    .await?;
                if let Some(reading) = reads_action(&level.own)
                    .or_else(|| reads_action(&node.actions))
                    .filter(reading_item)
                {
                    found = Some((node.id.clone(), reading));
                    break;
                }
            }
            if found.is_some() {
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
                    // A step that did not run says why in its typed answer as well as in words.
                    let (reply, refusal) = match Box::pin(
                        self.do_one(&step.path, step.args, invocation, wait_ms, structured),
                    )
                    .await
                    {
                        Ok(reply) => (reply, None),
                        Err(error) => {
                            let refusal = answer::refused(&error, false);
                            (Reply::failed(format!("failed: {error}")), Some(refusal))
                        }
                    };
                    let mut typed = reply.structured.clone().or(refusal).unwrap_or_else(|| {
                        if reply.failed {
                            json!({"status": "failed", "error": {
                                "code": answer::coded(&reply.text).0,
                                "message": answer::words(&reply.text),
                            }})
                        } else {
                            json!({"status": "ok"})
                        }
                    });
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
        // Asked again under the same id, a call that went unanswered is answered by what it
        // did: a typed caller is handed that id, as a pending answer hands it.
        if structured && asked.is_some() && answer::asks_again(&failure) {
            self.invocations_moved();
            let mut reply = Reply::failed(String::new());
            reply.structured = Some(answer::refused_under(&failure, id, true));
            return Ok(reply);
        }
        if !(path.contains('#') && super::confirm::needs_the_person(&failure)) {
            self.invocations_moved();
            return Err(failure);
        }
        self.invocations_moved();
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
        self.invocations_moved();
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
                self.invocations_moved();
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
                provider,
            } => {
                let mut reply = outcome(&value);
                if reply.failed && reply.text.contains("invalid_arguments") {
                    let _ = write!(reply.text, "\n{}", action_page(&path, &descriptor, None));
                }
                self.invocations_moved();
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
                Ok(reply)
            }
        }
    }

    /// Whether `action` at `parsed` is one of the workspace's own shell's verbs, which the device
    /// that serves the workspace answers at its Terminal record rather than the app: every verb
    /// for the person's own agent there ([`McpServer::with_device_shell`]), and for any agent a
    /// terminal's open, which the Terminal package declares nothing of and the device judges as
    /// reaching outside, so the person is asked first.
    pub(super) fn on_workspace_shell(&self, parsed: &Address, action: &str) -> bool {
        (self.shell == Shell::Device || action == OPEN_TERMINAL)
            && below_app(parsed).is_empty()
            && SHELL_VERBS.contains(&action)
            && names_terminal(&app_of(parsed))
    }

    /// The app `parsed` names and the provider its `action` runs in, and
    /// whether the path pinned it; `None` for starting the app itself
    /// ([`Self::launch`]). One of an installed app's own actions
    /// (`terminal#run`) starts the app first and runs in what it opened, so
    /// the caller asks once — except the workspace's own shell, which the
    /// device that serves the workspace answers at its Terminal record for the
    /// person's own agent there ([`McpServer::with_device_shell`]).
    async fn acting_in(
        &self,
        parsed: &Address,
        action: &str,
        wait_ms: Option<u64>,
    ) -> Result<Option<(String, String, bool)>, String> {
        // The person's own agent on the computer that serves their workspace runs a command in
        // the workspace's own shell, which that device serves as its Terminal record: not in a
        // windowless copy of the app, whose `run` may reach the device's host shell and so asks
        // its person first. The app need not be installed here: the shell is the device's.
        if self.on_workspace_shell(parsed, action) {
            return Ok(Some((
                tilo_app_id::apps::TERMINAL.to_owned(),
                tilo_app_id::apps::TERMINAL.to_owned(),
                true,
            )));
        }
        let reference = match self.tree.app_ref(&app_of(parsed)).await {
            Ok(reference) => reference,
            // An action's answer names the resource it made by its kind
            // (`tasks.tracker/issues/7`), which is a path the next command
            // takes as it is.
            Err(why) => match self.tree.app_of_type(&app_of(parsed)).await {
                Some((app_id, provider, pinned)) => {
                    return Ok(Some((app_id, provider, pinned)));
                }
                None => return Err(why.into()),
            },
        };
        match reference {
            AppRef::Installed { opens: false, .. } => {
                Err(tilo_actions::tree::Unresolved::NotRunning {
                    name: app_of(parsed),
                    opens: false,
                }
                .to_string())
            }
            AppRef::Installed { .. } if starts_app(action) && below_app(parsed).is_empty() => {
                Ok(None)
            }
            // Any other action, on the app or on a resource inside it, starts the app without a
            // window first and is then done where its path points: a fresh workspace's first
            // `notes/Notes#new_note` works the way the refusal for a read says it will.
            AppRef::Installed { app_id, .. } => {
                let provider = self
                    .tree
                    .started_provider(&app_id, wait_ms.map(std::time::Duration::from_millis))
                    .await?;
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
    async fn forms_named(&self, parsed: &Address) -> Option<ProgramForms> {
        if below_app(parsed)
            .iter()
            .any(|segment| is_program_form(&segment.written()))
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
            let top = self.tree.level(&resolved.provider, "", &mut levels).await?;
            if let Some((path, action)) =
                only_by_id(name, &published_on(&top.children, &[], &levels))
            {
                return Ok((format!("{}/{path}", resolved.path), action.clone()));
            }
            let root = self
                .tree
                .app_children(&resolved.provider, &mut levels)
                .await?;
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
                    .tree
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
    /// The action `action` on the resource `resolved` names, as the app publishes
    /// it now: the exact target asked of the workspace, with the availability it
    /// holds for this caller. `None` where the app's own rows already delivered
    /// it ([`delivered_action`]) or the app has no directory revision to ask at.
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

    /// Resolve a whole path in the tree. A segment written the way a program types a type
    /// (`tasks.Task`) is read as that type first, by the names the task's programs give the
    /// types: only a segment written that way asks them, so a path of titles (`notes/Todo`)
    /// does not start a task's host to read a name already said.
    ///
    /// The resolution, or what the path names said instead (an app that is installed and not
    /// running); with it the forms it was read by (`None` when it was read plainly), so a
    /// caller that shows what it found names the type the same way without asking again.
    async fn resolve(
        &self,
        parsed: &Address,
    ) -> Result<(Result<Resolved, String>, Option<ProgramForms>), String> {
        let forms = self.forms_named(parsed).await;
        match self.tree.resolve(parsed, typed(forms.as_ref())).await {
            Ok((resolved, by_type)) => Ok((Ok(resolved), forms.filter(|_| by_type))),
            Err(note) if note.is_note() => Ok((Err(note.to_string()), None)),
            Err(other) => Err(other.into()),
        }
    }

    /// [`Tree::resolve_with`] as a listing reads it: the resolution, or what the path names
    /// said instead.
    async fn resolve_with(
        &self,
        parsed: &Address,
        forms: Option<&ProgramForms>,
        reaching: usize,
    ) -> Result<Result<Resolved, String>, String> {
        match self.tree.resolve_with(parsed, typed(forms), reaching).await {
            Ok(resolved) => Ok(Ok(resolved)),
            Err(note) if note.is_note() => Ok(Err(note.to_string())),
            Err(other) => Err(other.into()),
        }
    }
}

/// `why` naming `code`, unless it already names the documented code it was
/// refused with.
fn refused_as(code: &str, why: String) -> String {
    if answer::coded(&why).0 == "failed" {
        format!("{code}: {why}")
    } else {
        why
    }
}

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
