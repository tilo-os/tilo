//! The agent surface: five tools an AI collaborator can drive any app with.
//!
//! `tilo_ls`, `tilo_read`, `tilo_do`, `tilo_find` and `tilo_status` address
//! everything by **path**: `notes/Todo/body#append-paragraph` is the app
//! (`notes`), the resource by its title, kind or key (`Todo`, `body`) and the
//! action after `#`. A path names things as a person would and survives a
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
use tilo_actions::{ActionDescriptor, OperationKind};

/// Resource rows one directory level reads at most, across its pages.
const LEVEL_PAGES: usize = 4;
/// Rows asked per page.
const PAGE_ROWS: u32 = 128;
/// Containers at an app's top level whose contents the app's root also
/// offers: a notebook's pages are the app's own.
const ROOT_PEEK: usize = 3;
/// Pages a provider-wide needle search reads before giving up on one name.
const SEARCH_PAGES: usize = 3;
/// Containers a name that is not where it was looked for reads on, one by
/// one, before it is called missing.
const DEEP_READS: usize = 16;
/// Lines `tilo_ls` answers before a cursor.
const LS_LINES: usize = 40;
/// Hits `tilo_find` answers.
const FIND_HITS: usize = 15;
/// Characters of a read answered before the rest is cut.
const READ_CHARS: usize = 12_000;
/// Longest label shown whole in a listing line.
const LABEL_CHARS: usize = 60;
/// Longest first sentence of an action's description in an action's page.
const DESCRIPTION_CHARS: usize = 200;
/// How many names an error lists before it stops.
const NAMES_LISTED: usize = 12;
/// What a bound UI resource's id starts with (ui-core's publication prefix).
const BOUND_PREFIX: &str = "tilo-ui-bound:v1:";

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
            "List apps, then what is in them. No path: the apps. `notes`: that app's resources and actions. `notes/Todo`: a note's children and actions. `notes/Todo#rename`: one action's parameters. A path is app/resource names (a title, kind or key, any case), stable across restarts; an action follows `#`. kind narrows to resources or actions; limit and cursor page.",
            json!({"path":string,"kind":{"enum":["resources","actions"]},"limit":integer,"cursor":string}),
            json!([]),
            read.clone(),
        ),
        (
            "tilo_read",
            "Read a resource as compact text. `notes/Todo` answers the note as markdown, each block ending in its id (`^c01e`), the id block actions take. Long reads end with a cursor line; pass it back as cursor.",
            json!({"path":string,"cursor":string,"max_chars":integer}),
            json!(["path"]),
            read.clone(),
        ),
        (
            "tilo_do",
            "Run an action and get its result: `{path: \"notes/Todo#append-paragraph\", args: {text: \"Buy milk\"}}`. Several in order, stopping at the first failure: `calls: [{path, args}, …]`. Start an app: `{path: \"notes#open\"}`. Answers `ok` with the result, or `failed: why`; still running after wait_ms (default 5000) it says `pending <id>` for tilo_status. Same invocation_id never runs twice.",
            json!({"path":string,"args":{"type":"object"},"calls":{"type":"array","minItems":1,"maxItems":MAX_CALLS,"items":call},"wait_ms":integer,"invocation_id":string}),
            json!([]),
            json!({"readOnlyHint":false,"destructiveHint":true,"idempotentHint":false,"openWorldHint":true}),
        ),
        (
            "tilo_find",
            "Search apps, resources and actions by words, best first: `{query: \"checklist add\"}`, optionally app: \"notes\". One line per hit, path#action(params) and its label, ready for tilo_do. Files are not searched.",
            json!({"query":string,"app":string,"limit":integer}),
            json!(["query"]),
            read,
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
pub(super) struct Reply {
    text: String,
    failed: bool,
}

impl Reply {
    fn ok(text: String) -> Self {
        Self {
            text,
            failed: false,
        }
    }
}

/// The MCP result of an agent tool: its text alone. There is no structured
/// copy beside it; the text is the answer and a second rendering of it would
/// be read twice.
pub(super) fn text_result(result: Result<Reply, String>) -> Value {
    let (text, failed) = match result {
        Ok(reply) => (reply.text, reply.failed),
        Err(message) => (message, true),
    };
    json!({"content":[{"type":"text","text":text}],"isError":failed})
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

    /// Whether `other` is this resource's own: its key names this one's
    /// distinguishing part (a note owns the body published beside it).
    fn owns(&self, other: &Node) -> bool {
        let token = self.token();
        token.len() >= 8 && other.key != self.key && other.key.contains(token)
    }

    /// How well `name` names this node: 0 is its label, 1 its key (whole or
    /// its last part), 2 its kind, 3 the start of its label or of its key's
    /// distinguishing part. `None` when it does not.
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

    /// The name that goes in a path: its label when that tells it apart from
    /// `siblings`, else the last part of its key (`body`), else its key.
    fn name(&self, siblings: &[Node]) -> String {
        // What belongs to this resource never competes with it: the name
        // goes to the owner.
        let owned = |other: &Node| self.owns(other);
        let apart = |name: &str| {
            !name.trim().is_empty()
                && siblings
                    .iter()
                    .filter(|other| other.id != self.id && !owned(other))
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

/// Pick the node `name` means among `nodes`: the best-ranked one, when one
/// is. Several equally good is an error naming them.
fn pick<'a>(name: &str, nodes: &'a [Node]) -> Result<Option<&'a Node>, String> {
    let ranked: Vec<(u8, &Node)> = nodes
        .iter()
        .filter_map(|node| Some((node.answers_to(name)?, node)))
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

fn first_sentence(text: &str) -> String {
    let text = text.trim();
    let sentence = text
        .split_once(". ")
        .map_or(text, |(first, _)| first)
        .trim_end_matches('.');
    if sentence.chars().count() <= DESCRIPTION_CHARS {
        sentence.to_owned()
    } else {
        let head: String = sentence.chars().take(DESCRIPTION_CHARS).collect();
        format!("{}…", head.trim_end())
    }
}

fn short(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_owned();
    }
    let head: String = text.chars().take(chars).collect();
    format!("{}…", head.trim_end())
}

/// `(p, q?, kind:a|b)`, from the action's parameters.
fn params_line(action: &ActionDescriptor) -> String {
    if action.params.is_empty() {
        return String::new();
    }
    let params = action
        .params
        .iter()
        .map(page::param)
        .collect::<Vec<_>>()
        .join(", ");
    format!("({params})")
}

fn risk_note(action: &ActionDescriptor) -> String {
    match page::risk_note(&action.contract) {
        Some(note) => format!(" [{note}]"),
        None => String::new(),
    }
}

/// One action in a listing: `#rename(value)  Rename note`.
fn action_line(action: &ActionDescriptor) -> String {
    let mut line = format!("#{}{}", action.address.action, params_line(action));
    let label = action.action_label.trim();
    if !label.is_empty() && !label.eq_ignore_ascii_case(&action.address.action.replace('-', " ")) {
        let _ = write!(line, "  {}", short(label, LABEL_CHARS));
    }
    line.push_str(&risk_note(action));
    line
}

/// The whole of one action: what it does and what it takes.
fn action_page(path: &str, action: &ActionDescriptor) -> String {
    let mut page = format!("{path}#{}", action.address.action);
    let label = action.action_label.trim();
    if !label.is_empty() {
        let _ = write!(page, "  {label}");
    }
    page.push_str(&risk_note(action));
    if let Some(description) = action
        .description
        .as_deref()
        .filter(|text| !text.is_empty())
    {
        let _ = write!(page, "\n{}", first_sentence(description));
    }
    if action.params.is_empty() {
        page.push_str("\nno parameters");
    }
    for param in &action.params {
        let _ = write!(page, "\n  {}", page::param(param));
        let description = param.description.trim();
        if !description.is_empty() && !description.eq_ignore_ascii_case(param.name.as_ref()) {
            let _ = write!(page, " — {}", first_sentence(description));
        }
    }
    page
}

/// Whether `value` is empty of meaning: null, an empty string, array or
/// object.
fn is_blank(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(fields) => fields.is_empty(),
        _ => false,
    }
}

/// `value` as compact JSON, without the nulls and empty containers that say
/// nothing.
fn strip_blank(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, item)| (key.clone(), strip_blank(item)))
                .filter(|(_, item)| !is_blank(item))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(strip_blank).collect()),
        other => other.clone(),
    }
}

/// One block of a document as a markdown line ending in its `{#id}`.
fn block_line(block: &Value) -> Option<String> {
    let text = block
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let id = block.get("id").and_then(Value::as_str)?;
    let kind = block.get("kind");
    let level = |name: &str| {
        kind.and_then(|kind| kind.get(name))
            .and_then(Value::as_u64)
            .map(|level| usize::try_from(level).unwrap_or(0).min(12))
    };
    let indent = |level: usize| "  ".repeat(level);
    let line = if let Some(heading) = level("heading") {
        format!("{} {text} ^{id}", "#".repeat(heading.clamp(1, 6)))
    } else if let Some(depth) = level("bullet") {
        format!("{}- {text} ^{id}", indent(depth))
    } else if let Some(depth) = level("numbered") {
        format!("{}1. {text} ^{id}", indent(depth))
    } else if let Some(depth) = level("checklist") {
        let done = kind
            .and_then(|kind| kind.get("done"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        format!(
            "{}- [{}] {text} ^{id}",
            indent(depth),
            if done { 'x' } else { ' ' }
        )
    } else if kind.is_some_and(|kind| kind.get("code").is_some()) {
        let language = kind
            .and_then(|kind| kind.get("code"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        format!("```{language} ^{id}\n{text}\n```")
    } else {
        match kind.and_then(Value::as_str) {
            Some("quote") => format!("> {text} ^{id}"),
            Some("divider") => format!("--- ^{id}"),
            _ => format!("{text} ^{id}"),
        }
    };
    Some(line)
}

/// What an app's read or an action's output is, as the least text that says
/// it: markdown stays markdown, a document's blocks become markdown with
/// their ids, a collection becomes a list, anything else compact JSON.
fn render_value(value: &Value) -> String {
    match value {
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
            if let Some(list) = collection(fields) {
                return list;
            }
            strip_blank(value).to_string()
        }
        Value::Null => String::new(),
        other => strip_blank(other).to_string(),
    }
}

/// A read that lists things (a notebook's pages): one line each, the title
/// and the item to read it by.
fn collection(fields: &serde_json::Map<String, Value>) -> Option<String> {
    let mut text = String::new();
    for (name, list) in fields {
        let Value::Array(items) = list else {
            continue;
        };
        let lines: Vec<String> = items
            .iter()
            .filter_map(|item| {
                let item_id = item.get("item").and_then(Value::as_str)?;
                let title = ["title", "name", "label"]
                    .iter()
                    .find_map(|key| item.get(*key).and_then(Value::as_str))?;
                Some(format!("{title}  ({item_id})"))
            })
            .collect();
        if lines.is_empty() {
            continue;
        }
        let _ = writeln!(text, "{name}:");
        for line in lines {
            let _ = writeln!(text, "  {line}");
        }
    }
    (!text.is_empty()).then(|| text.trim_end().to_owned())
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
    Running { app_id: String, provider: String },
    Installed { app_id: String },
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
        // as long as an action is waited for.
        let deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_millis(DEFAULT_WAIT_MS));
        let mut rows = Vec::new();
        'snapshot: loop {
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
                    if deadline.is_some_and(|deadline| std::time::Instant::now() < deadline) {
                        continue 'snapshot;
                    }
                    return Err(
                        "the app's directory kept changing while it was read; try again".into(),
                    );
                }
                rows.extend(page.rows);
                if page.exhausted || page.next_cursor.is_empty() {
                    break 'snapshot;
                }
                revision = Some(page.revision);
                cursor = page.next_cursor;
            }
            break;
        }
        let read = level_of(&rows, parent);
        levels.insert(parent.to_owned(), read.clone());
        Ok(read)
    }

    /// The app `name` means: a running one by its id or last part, else an
    /// installed one the workspace offers to open.
    async fn app_ref(&self, name: &str) -> Result<AppRef, String> {
        let (_, running) = self.instances().await?;
        let ids = running
            .iter()
            .filter_map(|(_, app)| app["app_id"].as_str())
            .collect::<Vec<_>>();
        match page::app_named(name, &ids) {
            Ok(app_id) => {
                let provider = running
                    .iter()
                    .rev()
                    .find(|(_, app)| app["app_id"].as_str() == Some(app_id.as_str()))
                    .map(|(peer, _)| peer.to_string())
                    .ok_or_else(|| page::not_running(&app_id, &ids))?;
                return Ok(AppRef::Running { app_id, provider });
            }
            Err(error) if error.contains("names several") => return Err(error),
            Err(_) => {}
        }
        let installed = self.installed().await.unwrap_or_default();
        let ids = installed
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>();
        match page::app_named(name, &ids) {
            Ok(app_id) => Ok(AppRef::Installed { app_id }),
            Err(error) if error.contains("names several") => Err(error),
            Err(_) => Err(format!(
                "no app {name}: running {}; installed {}",
                list_names(&app_names(
                    &running
                        .iter()
                        .filter_map(|(_, app)| app["app_id"].as_str().map(str::to_owned))
                        .collect::<Vec<_>>()
                )),
                list_names(&app_names(
                    &installed
                        .iter()
                        .map(|(id, _)| id.clone())
                        .collect::<Vec<_>>()
                )),
            )),
        }
    }

    /// The apps the workspace offers to open, with their labels.
    async fn installed(&self) -> Result<Vec<(String, String)>, String> {
        let provider = tilo_actions::remote::workspace_provider(&self.room);
        let rows = self.top_resources(&provider).await?;
        let mut apps: Vec<(String, String)> = Vec::new();
        for row in rows {
            let Some(app_id) = row
                .resource
                .id
                .strip_prefix(tilo_actions::remote::APPLICATION_RESOURCE_PREFIX)
                .filter(|app_id| !app_id.starts_with("host:"))
            else {
                continue;
            };
            if !apps.iter().any(|(id, _)| id == app_id) {
                apps.push((app_id.to_owned(), row.resource.label.clone()));
            }
        }
        Ok(apps)
    }

    /// The resources an app's path starts from: its top-level resources and
    /// the resources inside its containers (a notebook's pages are the app's
    /// own), with the containers' own actions (`notes#new-page`). What
    /// another resource owns is listed under it, not here.
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
            .retain(|node| !all.iter().any(|owner| owner.owns(node)));
        Ok(root)
    }

    /// Resolve `segments` below the app `provider`, one name at a time.
    async fn resolve_segments(
        &self,
        provider: &str,
        app: &str,
        segments: &[String],
        levels: &mut Levels,
    ) -> Result<(Vec<Node>, Level), String> {
        let mut here = self.app_children(provider, levels).await?;
        let mut chain: Vec<Node> = Vec::new();
        let written = app;
        for segment in segments {
            let found = match pick(segment, &here.children)? {
                Some(node) => node.clone(),
                None => self
                    .deep(provider, segment, chain.last(), levels)
                    .await?
                    .ok_or_else(|| {
                        format!(
                            "no {segment} in {written}; it has: {}",
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
            here = self.level(provider, &found.id, levels).await?;
            chain.push(found);
        }
        Ok((chain, here))
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
    ) -> Result<Option<Node>, String> {
        for _ in 0..=DEEP_READS {
            let pool: Vec<Node> = levels
                .values()
                .flat_map(|level| level.children.iter())
                .filter(|node| {
                    within.is_none_or(|owner| {
                        node.key.contains(owner.token()) || Self::below(node, owner, levels)
                    }) && node.answers_to(name).is_some()
                })
                .cloned()
                .collect();
            if let Some(found) = pick(name, &pool)? {
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

    /// Resolve a whole path.
    async fn resolve(&self, parsed: &ParsedPath) -> Result<Result<Resolved, String>, String> {
        match self.app_ref(&parsed.app).await? {
            AppRef::Running { app_id, provider } => {
                Ok(Ok(self.resolve_in(parsed, app_id, provider).await?))
            }
            AppRef::Installed { .. } => Ok(Err(Self::not_running_note(&parsed.app))),
        }
    }

    /// Resolve the segments of `parsed` in the running app `app_id`, whose
    /// instance is `provider`.
    async fn resolve_in(
        &self,
        parsed: &ParsedPath,
        app_id: String,
        provider: String,
    ) -> Result<Resolved, String> {
        let app = parsed.app.to_lowercase();
        let mut levels = Levels::new();
        let (chain, here) = self
            .resolve_segments(&provider, &app, &parsed.segments, &mut levels)
            .await?;
        let mut written = app;
        let mut here = here;
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
        let mut siblings = self.app_children(&provider, &mut levels).await?.children;
        let mut previous: Option<Node> = None;
        for node in &chain {
            // Named against what it sits among and the resource it sits
            // under, whose title it may share.
            let mut among = siblings.clone();
            among.extend(previous.clone());
            written = format!("{written}/{}", node.name(&among));
            siblings = self.level(&provider, &node.id, &mut levels).await?.children;
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
        })
    }

    fn not_running_note(name: &str) -> String {
        format!(
            "{name} is installed but not running; start it with tilo_do {{path: \"{}#open\"}}",
            path_name(name)
        )
    }
}

/// `node`'s path below its app: under the resource that owns it when one is
/// known (`Todo/body`), else among `pool`.
fn path_in(node: &Node, pool: &[Node]) -> String {
    match pool
        .iter()
        .filter(|owner| owner.owns(node))
        .max_by_key(|owner| owner.token().len())
    {
        Some(owner) => format!(
            "{}/{}",
            path_in(owner, pool),
            node.name(std::slice::from_ref(owner))
        ),
        None => node.name(pool),
    }
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
    pub(super) async fn agent_call(&self, name: &str, args: Value) -> Value {
        let result = match name {
            "tilo_ls" => match parse(args) {
                Ok(args) => self.agent_ls(args).await,
                Err(error) => Err(error),
            },
            "tilo_read" => match parse(args) {
                Ok(args) => self.agent_read(args).await,
                Err(error) => Err(error),
            },
            "tilo_do" => match parse(args) {
                Ok(args) => self.agent_do(args).await,
                Err(error) => Err(error),
            },
            "tilo_find" => match parse(args) {
                Ok(args) => self.agent_find(args).await,
                Err(error) => Err(error),
            },
            "tilo_status" => match parse(args) {
                Ok(args) => self.agent_status(&args),
                Err(error) => Err(error),
            },
            _ => Err("Unknown tool".to_owned()),
        };
        text_result(result)
    }

    async fn agent_ls(&self, args: LsArgs) -> Result<Reply, String> {
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
            return self.list_apps(offset, limit).await;
        }
        let parsed = parse_path(path)?;
        let resolved = match self.resolve(&parsed).await? {
            Ok(resolved) => resolved,
            Err(note) => return Ok(Reply::ok(note)),
        };
        if let Some(action) = &parsed.action {
            let found = pick_action(action, &resolved.actions, &resolved.path)?;
            return Ok(Reply::ok(action_page(&resolved.path, found)));
        }
        let mut lines: Vec<String> = Vec::new();
        if want_resources {
            let mut named: Vec<Node> = resolved.children.clone();
            named.extend(resolved.node.clone());
            for child in &resolved.children {
                let mut line = child.name(&named);
                if child.container {
                    line.push('/');
                }
                let _ = write!(line, "  {}", child.kind);
                if !child.actions.is_empty() {
                    let _ = write!(line, "  [{} actions]", child.actions.len());
                }
                lines.push(line);
            }
        }
        if want_actions {
            lines.extend(resolved.actions.iter().map(action_line));
        }
        let total = lines.len();
        let mut text = resolved.path.clone();
        if let Some(node) = &resolved.node {
            let _ = write!(text, "  ({})", node.kind);
        } else {
            let _ = write!(text, "  ({})", resolved.app_id);
        }
        for line in lines.into_iter().skip(offset).take(limit) {
            text.push('\n');
            text.push_str(&line);
        }
        let end = offset.saturating_add(limit);
        if end < total {
            let _ = write!(
                text,
                "\n… {} more: cursor \"{end}\"",
                total.saturating_sub(end)
            );
        } else if total == 0 {
            text.push_str("\n(nothing here)");
        }
        Ok(Reply::ok(text))
    }

    /// The apps: running ones first, then the installed ones to open.
    async fn list_apps(&self, offset: usize, limit: usize) -> Result<Reply, String> {
        let (_, running) = self.instances().await?;
        let mut lines: Vec<String> = Vec::new();
        let mut running_ids: Vec<String> = Vec::new();
        for (_, app) in &running {
            let Some(app_id) = app["app_id"].as_str() else {
                continue;
            };
            if !running_ids.iter().any(|seen| seen == app_id) {
                running_ids.push(app_id.to_owned());
            }
        }
        let installed = self.installed().await.unwrap_or_default();
        for app_id in &running_ids {
            let label = installed
                .iter()
                .find(|(id, _)| id == app_id)
                .map_or("", |(_, label)| label.as_str());
            lines.push(format!(
                "{}  {label}  running",
                app_names(std::slice::from_ref(app_id)).join("")
            ));
        }
        for (app_id, label) in &installed {
            if !running_ids.contains(app_id) {
                lines.push(format!(
                    "{}  {label}  installed",
                    app_names(std::slice::from_ref(app_id)).join("")
                ));
            }
        }
        let total = lines.len();
        let mut text = String::from("apps");
        for line in lines.into_iter().skip(offset).take(limit) {
            text.push('\n');
            text.push_str(&line);
        }
        let end = offset.saturating_add(limit);
        if end < total {
            let _ = write!(
                text,
                "\n… {} more: cursor \"{end}\"",
                total.saturating_sub(end)
            );
        }
        Ok(Reply::ok(text))
    }

    async fn agent_read(&self, args: ReadArgs) -> Result<Reply, String> {
        let parsed = parse_path(&args.path)?;
        if parsed.action.is_some() {
            return Err("tilo_read reads a resource; run an action with tilo_do".into());
        }
        let mut resolved = match self.resolve(&parsed).await? {
            Ok(resolved) => resolved,
            Err(note) => return Ok(Reply::ok(note)),
        };
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
            return Ok(Reply { text, failed: true });
        };
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
        if let Some(cursor) = args.cursor.filter(|cursor| !cursor.is_empty()) {
            if has("cursor") {
                arguments.insert("cursor".into(), cursor.into());
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
        let value = Box::pin(self.run(
            RemoteActionTarget {
                provider,
                address: reader.address.clone(),
            },
            arguments,
            tilo_actions::InvocationId::new(),
            None,
            std::time::Duration::from_millis(DEFAULT_WAIT_MS),
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
        let mut text = render_value(&output);
        let limit = args.max_chars.unwrap_or(READ_CHARS).clamp(200, 200_000);
        let cut = cut_at_line(&mut text, limit);
        if let Some(left) = cut {
            let _ = write!(
                text,
                "\n… {left} more characters; pass max_chars to read further"
            );
        }
        if let Some(cursor) = next_cursor(&output).filter(|_| cut.is_none()) {
            let _ = write!(text, "\n… more: cursor \"{cursor}\"");
        }
        if text.is_empty() {
            text.push_str("(empty)");
        }
        Ok(Reply::ok(text))
    }

    /// The action that reads `resolved`, and the id of the resource that
    /// offers it: the target's own, or its nearest ancestor's, which takes
    /// the target as its `item`.
    async fn reader(
        &self,
        resolved: &Resolved,
        levels: &mut Levels,
    ) -> Result<Option<(String, ActionDescriptor)>, String> {
        let reads = |actions: &[ActionDescriptor]| {
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
                        action.kind == OperationKind::View
                            && action.address.action.starts_with("read")
                    })
                })
                .cloned()
        };
        if let Some(found) = reads(&resolved.actions) {
            let owner = resolved
                .node
                .as_ref()
                .map_or_else(|| found.address.resource.clone(), |node| node.id.clone());
            return Ok(Some((owner, found)));
        }
        for ancestor in resolved.chain.iter().rev().skip(1) {
            let level = self.level(&resolved.provider, &ancestor.id, levels).await?;
            if let Some(found) = reads(&level.own) {
                return Ok(Some((ancestor.id.clone(), found)));
            }
        }
        // The app's root resources: a notebook reads its pages.
        let root = self.level(&resolved.provider, "", levels).await?;
        for node in &root.children {
            let level = self.level(&resolved.provider, &node.id, levels).await?;
            if let Some(found) = reads(&level.own).or_else(|| reads(&node.actions)) {
                return Ok(Some((node.id.clone(), found)));
            }
        }
        Ok(None)
    }

    async fn agent_do(&self, args: DoArgs) -> Result<Reply, String> {
        let wait_ms = args.wait_ms;
        match (args.path, args.calls) {
            (Some(path), None) => {
                let _running = self.hold_invocation(args.invocation_id.as_deref()).await;
                Box::pin(self.do_one(&path, args.args, args.invocation_id, wait_ms)).await
            }
            (None, Some(calls)) => {
                if calls.is_empty() || calls.len() > MAX_CALLS {
                    return Err(format!("calls must hold 1 to {MAX_CALLS} steps"));
                }
                let _running = self.hold_invocation(args.invocation_id.as_deref()).await;
                let total = calls.len();
                let mut text = String::new();
                for (index, step) in calls.into_iter().enumerate() {
                    let invocation = batch_invocation(args.invocation_id.as_deref(), index)?;
                    let reply = Box::pin(self.do_one(&step.path, step.args, invocation, wait_ms))
                        .await
                        .unwrap_or_else(|error| Reply {
                            text: format!("failed: {error}"),
                            failed: true,
                        });
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
                        return Ok(Reply { text, failed: true });
                    }
                }
                Ok(Reply::ok(text))
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
    ) -> Result<Reply, String> {
        let parsed = parse_path(path)?;
        let action = parsed.action.clone().ok_or_else(|| {
            format!("name the action: {path}#<action>; tilo_ls {path} lists them")
        })?;
        // The app is looked up once: resolving the path in it does not ask
        // again which apps run.
        let (app_id, provider) = match self.app_ref(&parsed.app).await? {
            AppRef::Installed { app_id } => {
                return self
                    .launch(&parsed, &app_id, &action, arguments, invocation, wait_ms)
                    .await;
            }
            AppRef::Running { app_id, provider } => (app_id, provider),
        };
        let resolved = self.resolve_in(&parsed, app_id, provider).await?;
        let descriptor = pick_action(&action, &resolved.actions, &resolved.path)
            .map_err(|error| format!("{error}; tilo_ls {} lists them", resolved.path))?
            .clone();
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
        let mut reply = outcome(&value);
        if reply.failed && reply.text.contains("invalid_arguments") {
            let _ = write!(reply.text, "\n{}", action_page(&resolved.path, &descriptor));
        }
        Ok(reply)
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
    ) -> Result<Reply, String> {
        if !parsed.segments.is_empty() || action != tilo_actions::remote::APPLICATION_OPEN_ACTION {
            return Err(Self::not_running_note(&parsed.app));
        }
        let value = Box::pin(self.invoke_one(OneCall {
            provider: Addressed {
                provider: tilo_actions::remote::workspace_provider(&self.room),
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
        .await?;
        let mut reply = outcome(&value);
        if reply.failed {
            return Ok(reply);
        }
        let name = app_names(&[app_id.to_owned()]).join("");
        reply.text = format!("ok: {name} is running");
        if let Some(error) = value.pointer("/opened/error").and_then(Value::as_str) {
            let _ = write!(reply.text, "\nnot opened: {error}");
        }
        if let Ok(listing) = self
            .agent_ls(LsArgs {
                path: Some(name),
                ..LsArgs::default()
            })
            .await
        {
            reply.text.push('\n');
            reply.text.push_str(&listing.text);
        }
        Ok(reply)
    }

    fn agent_status(&self, args: &AgentStatusArgs) -> Result<Reply, String> {
        let invocation = invocation_id(&args.invocation_id)?;
        if args.cancel {
            self.host()
                .cancel(&self.caller(), &invocation)
                .map_err(|error| error.to_string())?;
        }
        let value = self.status(invocation)?;
        Ok(outcome(&value))
    }

    async fn agent_find(&self, args: FindArgs) -> Result<Reply, String> {
        let words = page::needle_words(&args.query);
        if words.is_empty() {
            return Err("query needs a word to look for".into());
        }
        let limit = args.limit.unwrap_or(FIND_HITS).clamp(1, 100);
        let (_, running) = self.instances().await?;
        let installed = self.installed().await.unwrap_or_default();
        let mut hits: Vec<Hit> = Vec::new();
        let wanted = args.app.as_deref().map(str::to_lowercase);
        // Apps by name.
        let mut seen: Vec<String> = Vec::new();
        for (_, app) in &running {
            if let Some(app_id) = app["app_id"].as_str()
                && !seen.iter().any(|id| id == app_id)
            {
                seen.push(app_id.to_owned());
            }
        }
        for (app_id, label) in &installed {
            let name = app_names(std::slice::from_ref(app_id)).join("");
            let matches = words.iter().all(|word| {
                name.starts_with(word.as_str()) || label.to_lowercase().starts_with(word.as_str())
            });
            if matches && wanted.as_ref().is_none_or(|wanted| *wanted == name) {
                let state = if seen.contains(app_id) {
                    "running"
                } else {
                    "installed"
                };
                hits.push(Hit {
                    tier: 0,
                    line: format!("{name}  {label}  {state}"),
                });
            }
        }
        // Resources and actions inside each running app, newest instance,
        // every app asked at once: one that does not answer costs one wait,
        // not one wait per app, and leaves the hits of those that did.
        let mut searched: Vec<(String, String)> = Vec::new();
        for app_id in &seen {
            let name = app_names(std::slice::from_ref(app_id)).join("");
            if wanted.as_ref().is_some_and(|wanted| *wanted != name) {
                continue;
            }
            if let Some(provider) = running
                .iter()
                .rev()
                .find(|(_, app)| app["app_id"].as_str() == Some(app_id.as_str()))
                .map(|(peer, _)| peer.to_string())
            {
                searched.push((name, provider));
            }
        }
        let answers = join_all(
            searched
                .iter()
                .map(|(name, provider)| self.search_app(name, provider, &words))
                .collect(),
        )
        .await;
        let answers_asked = answers.len();
        let mut silent: Vec<String> = Vec::new();
        for ((name, _), answer) in searched.into_iter().zip(answers) {
            match answer {
                Ok(found) => hits.extend(found),
                Err(_) => silent.push(name),
            }
        }
        // Nothing found because no app was searched is not "nothing
        // matches": an agent would take the resource for absent.
        if hits.is_empty() && !silent.is_empty() && silent.len() == answers_asked {
            return Ok(Reply {
                text: format!(
                    "not searched, the app did not answer: {}; try again",
                    list_names(&silent)
                ),
                failed: true,
            });
        }
        hits.sort_by_key(|hit| hit.tier);
        let total = hits.len();
        let mut text = String::new();
        for hit in hits.into_iter().take(limit) {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&hit.line);
        }
        if total > limit {
            let _ = write!(
                text,
                "\n… {} more; narrow the query or raise limit",
                total.saturating_sub(limit)
            );
        }
        if text.is_empty() {
            text = format!(
                "nothing matches {}; tilo_ls shows the apps and what is in them",
                args.query.trim()
            );
        }
        if !silent.is_empty() {
            let _ = write!(
                text,
                "\nnot searched, the app did not answer: {}",
                list_names(&silent)
            );
        }
        Ok(Reply::ok(text))
    }

    /// The hits inside one running app.
    async fn search_app(
        &self,
        app: &str,
        provider: &str,
        words: &[String],
    ) -> Result<Vec<Hit>, String> {
        let longest = words
            .iter()
            .max_by_key(|word| word.len())
            .cloned()
            .unwrap_or_default();
        let mut rows = Vec::new();
        let mut cursor = Vec::new();
        let mut revision = None;
        for _ in 0..SEARCH_PAGES {
            let page = self
                .ask(CatalogueQuery {
                    provider: Some(provider.to_owned()),
                    needle: longest.clone(),
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
                break;
            }
            rows.extend(page.rows);
            if page.exhausted || page.next_cursor.is_empty() {
                break;
            }
            revision = Some(page.revision);
            cursor = page.next_cursor;
        }
        let nodes = level_of(&rows, "").children;
        let mut levels = Levels::new();
        let mut pool = self.app_children(provider, &mut levels).await?.children;
        for node in &nodes {
            if !pool.iter().any(|seen| seen.id == node.id) {
                pool.push(node.clone());
            }
        }
        let mut hits = Vec::new();
        for row in &rows {
            if !page::matches_words(row, words) {
                continue;
            }
            let Some(node) = nodes.iter().find(|node| node.id == row.resource.id) else {
                continue;
            };
            let path = format!("{app}/{}", path_in(node, &pool));
            let resource_words = |text: &str| {
                let lower = text.to_lowercase();
                words.iter().all(|word| {
                    lower
                        .split(|c: char| !c.is_alphanumeric())
                        .any(|token| token.starts_with(word.as_str()))
                })
            };
            let (tier, line) = match &row.action {
                None => (1, format!("{path}  {}", node.kind)),
                Some(action) => {
                    let named = resource_words(&format!(
                        "{} {}",
                        action.address.action, action.action_label
                    ));
                    (
                        if named { 2 } else { 3 },
                        format!("{path}{}", action_line(action)),
                    )
                }
            };
            if !hits.iter().any(|hit: &Hit| hit.line == line) {
                hits.push(Hit { tier, line });
            }
        }
        Ok(hits)
    }
}

/// Every one of `futures`, run together, their outputs in their order.
async fn join_all<F: std::future::Future>(futures: Vec<F>) -> Vec<F::Output> {
    let mut running: Vec<std::pin::Pin<Box<F>>> = futures.into_iter().map(Box::pin).collect();
    let mut outputs: Vec<Option<F::Output>> = running.iter().map(|_| None).collect();
    std::future::poll_fn(|context| {
        let mut finished = true;
        for (future, output) in running.iter_mut().zip(outputs.iter_mut()) {
            if output.is_some() {
                continue;
            }
            match future.as_mut().poll(context) {
                std::task::Poll::Ready(answer) => *output = Some(answer),
                std::task::Poll::Pending => finished = false,
            }
        }
        if finished {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    outputs.into_iter().flatten().collect()
}

/// One line of a search, with how well it matched: 0 an app, 1 a resource,
/// 2 an action named as searched, 3 an action described so.
struct Hit {
    tier: u8,
    line: String,
}

#[cfg(test)]
#[path = "mcp_agent_tests.rs"]
mod tests;
