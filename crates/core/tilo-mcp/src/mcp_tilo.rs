//! The `tilo` tool: an agent's task writes tilolang programs
//! (`docs/designs/app-language/agents.md`, "How a program runs").
//!
//! A program runs as the task, in the task's own tilolang host: one per task, its code author the
//! task, started by the session ([`TaskHosts`]) on the task's first `tilo` call and living as long
//! as this connection. The host holds the task's `let`s and no records. It compiles each program
//! against every installed app and runs it; what the program reads and changes comes back here on
//! the task's program connection, and this API does it as the task, with the check, attribution
//! and grants of every other call:
//!
//! - a read is the app's own `read`, as the task, in the app; a resource that does not read
//!   itself is read by the resource the app's directory lists it under, which takes it as its
//!   `item` — the read a window and `tilo_read` take too;
//! - a resource the session publishes itself rather than from an app's record (the device's
//!   sound, its power, the session's windows and styles, Settings' sections) is read from the
//!   newest reading its service publishes, through the same record API
//!   ([`tilo_actions::remote::ResourceReads::read`]): the shell's parts read the same ones, so
//!   `styles.installed.offered` is a program's to read;
//! - a change is an invocation of the action's own app: a method its type's declared action, an
//!   assignment the resource's field edit. Each is decided at that action's node, asked about or
//!   refused as the workspace's policy says, never done silently. A program never writes a record.
//!
//! One call is one step: every invocation it makes carries the call's undo step and the task's
//! run, so `undo` takes back the last step, in every app it changed, and `undo task` every step of
//! this task. `history task` answers from the workspace's runs of this agent, so it lists the
//! agent's `tilo_do` calls and its work in earlier sessions too, beside the steps this task can
//! still take back. A program that fails takes back what it changed. An agent's program that
//! would change many resources or destroy something is first previewed, from a provisional run
//! that sends nothing, and runs once its person says yes ([`Go`]); a person's own runs at once.
//!
//! What goes back is tilolang text. Every resource it shows is named by a short reference (`n3`,
//! `t91`) at the start of its line, which later programs of this session take; a reference the
//! session never showed is refused, so none can be invented.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

use tilo_action_services::workspace_invocations::{
    self, Invocation, InvocationStatus, Invocations, InvocationsQuery,
};
use tilo_actions::ActionDescriptor;
use tilo_actions::remote::{InvokeArgument, InvokeRequest, ReceiptStatus};
use tilo_lock::MutexRecover as _;

use super::{ActionProvider, Addressed, Failure, McpServer, OneCall, Reply, app_names};

/// How long a program waits for an action it invoked to settle before it gives up on the
/// action's outcome: as long as any caller of an invoke may ask. A program cannot poll and a trap
/// loses the step it was taking, so it holds on to the action's own answer instead.
const PROGRAM_WAIT_MS: u64 = super::super::MAX_WAIT_MS;

/// Why a task's program was given up on: what was waited for, and for how long.
fn waited_for_the_task(what: &str) -> String {
    format!(
        "the task's programs did not answer within {} s: {what} did not come; a listing reads \
         no programs, and a program's call starts another",
        super::super::QUERY_ANSWER_WAIT.as_secs()
    )
}

/// Where an API asks for the tilolang host of a task: the device running the API, which starts
/// it as it starts every tilolang process, in that process's sandbox, and ends it with the task.
pub trait TaskHosts: Send + Sync {
    /// Start the host of the task `task` (the agent run) in `workspace`: the API's end of the task's
    /// program connection.
    fn start(&self, workspace: &str, task: &str) -> std::io::Result<tilo_socket::Stream>;
}

/// The program connection to a task's host, one JSON object per line: a Unix socket, or a pipe on
/// Windows.
pub(crate) struct TaskLink {
    reader: tokio::io::BufReader<tokio::io::ReadHalf<tilo_socket::tokio::Stream>>,
    writer: tokio::io::WriteHalf<tilo_socket::tokio::Stream>,
}

impl TaskLink {
    fn new(stream: tilo_socket::Stream) -> std::io::Result<Self> {
        let (reader, writer) = tokio::io::split(tilo_socket::tokio::from_std(stream)?);
        Ok(Self {
            reader: tokio::io::BufReader::new(reader),
            writer,
        })
    }

    async fn send(&mut self, message: &Value) -> Result<(), String> {
        let mut line = serde_json::to_vec(message).map_err(|error| error.to_string())?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .await
            .map_err(|error| format!("the task's host went away: {error}"))
    }

    async fn next(&mut self) -> Result<Value, String> {
        let mut line = String::new();
        match self.reader.read_line(&mut line).await {
            Ok(0) => Err("the task's host ended".to_owned()),
            Ok(_) => serde_json::from_str(&line)
                .map_err(|error| format!("the task's host said something unreadable: {error}")),
            Err(error) => Err(format!("the task's host went away: {error}")),
        }
    }
}

/// What the `tilo` tool takes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TiloArgs {
    program: String,
}

pub use tilo_action_services::task_programs::Go;

/// What the `tilo` tool answers for a program that waits for its person's yes: nothing of it ran,
/// and the asked record it waits in (`<workspace>/<record>`) once the device kept one.
fn waiting(record: Option<&str>, structured: bool) -> Reply {
    let text = match record {
        Some(record) => format!(
            "waiting for your person's yes: {record}. Nothing ran yet; it runs once they approve. \
             Go on with what does not need it, and do not send this program again."
        ),
        None => "waiting for your person's yes. Nothing ran yet; it runs once they approve. Go on \
                 with what does not need it, and do not send this program again."
            .to_owned(),
    };
    let mut reply = Reply::ok(text.clone());
    if structured {
        reply.structured = Some(json!({
            "status": "waiting",
            "record": record,
            "output": [],
            "changed": 0,
            "text": text,
        }));
    }
    reply
}

/// One step a program of this task made.
#[derive(Clone, Debug)]
struct Step {
    /// Its number in `history task`, from 1.
    number: usize,
    /// The apps it changed, by id, in the order it first changed each.
    apps: Vec<String>,
    /// The program's first line, as `history task` names the step.
    program: String,
    changed: u64,
    undone: bool,
    /// Its undo was refused or failed: it stays in effect, and `undo` passes over it to the
    /// step before, so one step that cannot be taken back never holds the others.
    kept: bool,
    /// The undo step its calls named, by which the workspace's invocations record keeps them:
    /// taking the step back takes each of them back through its own record.
    gesture: Option<tilo_actions::ActionId>,
}

/// The `tilo` tool's session on one connection.
#[derive(Default)]
pub(crate) struct Programs {
    /// Reference → resource literal (`@notes.Note:root/notes:n2`).
    by_ref: BTreeMap<String, String>,
    by_literal: BTreeMap<String, String>,
    counters: BTreeMap<char, usize>,
    /// The names the task's programs bound.
    lets: Vec<String>,
    steps: Vec<Step>,
    /// What the person pointed at when they asked, as a resource literal: what `this` names.
    this: Option<String>,
}

impl Programs {
    /// The steps of this session's `history task`, typed.
    fn steps_json(&self) -> Vec<Value> {
        self.steps
            .iter()
            .map(|step| {
                json!({
                    "number": step.number,
                    "apps": step.apps,
                    "changed": step.changed,
                    "program": step.program,
                    "undone": step.undone,
                    "kept": step.kept,
                })
            })
            .collect()
    }

    /// The reference of `literal`'s resource (type `ty`), given it the first time it is shown.
    fn name(&mut self, ty: &str, literal: &str) -> String {
        if let Some(known) = self.by_literal.get(literal) {
            return known.clone();
        }
        let letter = ty
            .chars()
            .find(char::is_ascii_alphabetic)
            .map_or('r', |c| c.to_ascii_lowercase());
        let count = self.counters.entry(letter).or_insert(0);
        *count = count.saturating_add(1);
        let shown = format!("{letter}{count}");
        self.by_ref.insert(shown.clone(), literal.to_owned());
        self.by_literal.insert(literal.to_owned(), shown.clone());
        shown
    }
}

/// Whether `id` can stand in a resource literal (`@app.Type:id`): an id, then `/item` steps,
/// each of letters, digits, `_` and `-`.
fn literal_id(id: &str) -> bool {
    !id.is_empty()
        && id.split('/').all(|step| {
            !step.is_empty()
                && step
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

/// How the task's programs write what the apps declare: each action as it is called, and each
/// type by its name. Read from the task host's catalogue, so a listing, a path and a program
/// name a type with one name: [`Self::kind`] says what a listing prints for a resource's kind
/// (`tasks.Task`), and [`Self::answers`] reads that same name back as its published kind, which
/// is how a path walks to the resource a listing showed. One table serves both.
#[derive(Clone, Default)]
pub(crate) struct ProgramForms {
    /// By action id, each app's action of that id.
    actions: BTreeMap<String, Vec<Declared>>,
    /// `(app, type name)` → `notes.Note`: a published kind's `tl.<app>.<Type>` as a program
    /// types it, and the only mapping between the two.
    types: BTreeMap<(String, String), String>,
}

/// One app's action, as a program calls it and what it acts on.
#[derive(Clone)]
struct Declared {
    app: String,
    /// `session.place(resource: host.Resource, x: Option<Int> = .none)`.
    call: String,
    /// The package's own type its first parameter takes: it acts on a resource of that type.
    on: Option<String>,
    /// The resource the session publishes a loose action on.
    at: Option<String>,
}

impl ProgramForms {
    fn of(catalogue: &Value) -> Self {
        let mut forms = Self::default();
        for app in catalogue
            .as_object()
            .into_iter()
            .flat_map(|apps| apps.values())
        {
            let Some(id) = app["app"].as_str() else {
                continue;
            };
            for signature in app["signatures"].as_array().into_iter().flatten() {
                let text = |key: &str| signature[key].as_str().map(str::to_owned);
                let (Some(kind), Some(call)) = (signature["kind"].as_str(), text("call")) else {
                    continue;
                };
                match (kind, text("id"), text("name")) {
                    ("action", Some(action), _) => {
                        forms.actions.entry(action).or_default().push(Declared {
                            app: id.to_owned(),
                            call,
                            on: text("on"),
                            at: text("at"),
                        });
                    }
                    ("type", _, Some(name)) => {
                        forms.types.insert((id.to_owned(), name), call);
                    }
                    _ => {}
                }
            }
        }
        forms
    }

    /// How a program calls the action `action` (its published id) of the resource `resource`
    /// of `kind`, listed under the app `app`: the declaration that acts on that resource, a
    /// resource of its type or the one it is published on. `None` when no app's does.
    pub(crate) fn action(
        &self,
        app: &str,
        action: &str,
        resource: &str,
        kind: &str,
    ) -> Option<&str> {
        let type_names = kind_type_names(kind);
        let acts_here = |declared: &&Declared| match (&declared.on, &declared.at) {
            (Some(on), _) => type_names.iter().any(|name| name == on),
            (None, Some(at)) => at == resource,
            (None, None) => declared.app == app,
        };
        let mut found = self.actions.get(action)?.iter().filter(acts_here);
        let first = found.next()?;
        found.next().is_none().then_some(first.call.as_str())
    }

    /// How a program calls `action`, listed under the app `listed`.
    pub(crate) fn called(&self, action: &ActionDescriptor, listed: &str) -> Option<&str> {
        self.action(
            &action_app(action, listed),
            &action.address.action,
            &action.address.resource,
            &action.resource_kind,
        )
    }

    /// How a program names the type of a resource of `kind`, a tilolang app's
    /// (`tl.<app>.<Type>`), when the task holds that app's package.
    pub(crate) fn kind(&self, kind: &str) -> Option<&str> {
        let (app, ty) = kind.strip_prefix("tl.")?.rsplit_once('.')?;
        self.types
            .get(&(app.to_owned(), ty.to_owned()))
            .map(String::as_str)
    }

    /// Whether `name`, a segment of a path (`tasks.Task`), names the type of a resource of
    /// `kind`, as a listing prints it ([`Self::kind`]): the same table read backwards, so a name
    /// read off a row walks back to that resource. `false` for a kind no program types
    /// (`notes.notebook`), and for one the task holds no package of.
    pub(crate) fn answers(&self, name: &str, kind: &str) -> bool {
        let Some((app, ty)) = kind
            .strip_prefix("tl.")
            .and_then(|rest| rest.rsplit_once('.'))
        else {
            return false;
        };
        self.types
            .get(&(app.to_owned(), ty.to_owned()))
            .is_some_and(|called| called.trim().eq_ignore_ascii_case(name.trim()))
    }
}

impl tilo_actions::tree::TypeNames for ProgramForms {
    fn calls(&self, name: &str, resource_type: &str) -> bool {
        Self::answers(self, name, resource_type)
    }
}

/// The names a package reading a surface gives the type of resources of `kind`
/// (`tilolang_tilo::catalogue::kind_type_name`): a tilolang kind's own type (`tl.<app>.<Type>`),
/// else its last segment in upper camel case, or its last two when that one is taken
/// (`tilo.workspace.emoji` is `Emoji` or `WorkspaceEmoji`).
fn kind_type_names(kind: &str) -> Vec<String> {
    if let Some((_, ty)) = kind
        .strip_prefix("tl.")
        .and_then(|rest| rest.rsplit_once('.'))
    {
        return vec![ty.to_owned()];
    }
    let camel = |segment: &str| -> String {
        segment
            .split(['-', '_'])
            .map(|word| {
                let mut chars = word.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().chain(chars).collect()
                })
            })
            .collect()
    };
    let mut segments = kind.rsplit('.');
    let last = camel(segments.next().unwrap_or(kind));
    let before = segments.next().map(camel).unwrap_or_default();
    vec![last.clone(), format!("{before}{last}")]
}

/// The app an action is declared by: a tilolang app's resource's (`tl.<app>.<Type>`) by its
/// kind, any other's the app the listing is of.
fn action_app(action: &ActionDescriptor, listed: &str) -> String {
    action
        .resource_kind
        .strip_prefix("tl.")
        .and_then(|rest| rest.rsplit_once('.'))
        .map_or_else(|| listed.to_owned(), |(app, _)| app.to_owned())
}

/// What a resource's actions are, in its line of a listing: how many, and how a program calls
/// the first of them (`3 actions: session.open_search, session.step_focus, …`).
pub(super) fn actions_note<'a>(
    actions: &[&'a ActionDescriptor],
    called: &impl Fn(&'a ActionDescriptor) -> Option<&'a str>,
) -> String {
    const NAMED: usize = 3;
    let names: Vec<&str> = actions
        .iter()
        .filter_map(|action| called(action))
        .map(|call| call.split_once('(').map_or(call, |(name, _)| name))
        .take(NAMED.saturating_add(1))
        .collect();
    let count = if actions.len() == 1 {
        "1 action".to_owned()
    } else {
        format!("{} actions", actions.len())
    };
    if names.is_empty() {
        return format!("[{count}]");
    }
    let more = if actions.len() > NAMED { ", …" } else { "" };
    let shown: Vec<&str> = names.into_iter().take(NAMED).collect();
    format!("[{count}: {}{more}]", shown.join(", "))
}

/// Whether `word` is written as a reference is (`n3`, `t91`).
fn looks_like_ref(word: &str) -> bool {
    let mut chars = word.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.clone().next().is_some()
        && chars.all(|c| c.is_ascii_digit())
}

/// A piece of a program's text: an identifier, or anything else (a string with its quotes,
/// punctuation, space) kept as it was written.
enum Piece {
    Word {
        text: String,
        /// It follows a dot, `@` or `:`: a member, or a part of a literal.
        member: bool,
    },
    Other(String),
}

/// Whether a `/` after `previous` (the last character that is no space) starts a regular
/// expression, as the lexer reads it: where a value starts, not after one.
fn starts_regex(previous: Option<char>) -> bool {
    previous.is_none_or(|c| "(,:=[{+-*!<>&|;\n".contains(c))
}

/// The rest of a regular expression whose `/` was just taken: up to its closing `/` on the
/// line (a `\/` is a slash), with its flags.
fn regex_rest(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut text = String::new();
    let mut escaped = false;
    while let Some(&next) = chars.peek() {
        if next == '\n' {
            break;
        }
        chars.next();
        text.push(next);
        match next {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '/' => {
                while let Some(flag) = chars.next_if(char::is_ascii_alphabetic) {
                    text.push(flag);
                }
                break;
            }
            _ => {}
        }
    }
    text
}

/// `source` in pieces, which joined give it back.
fn pieces(source: &str) -> Vec<Piece> {
    let mut found: Vec<Piece> = Vec::new();
    let mut chars = source.chars().peekable();
    let mut previous: Option<char> = None;
    let mut significant: Option<char> = None;
    while let Some(character) = chars.next() {
        if character == '/' && chars.peek() != Some(&'/') && starts_regex(significant) {
            let text = format!("/{}", regex_rest(&mut chars));
            previous = text.chars().last();
            significant = previous;
            match found.last_mut() {
                Some(Piece::Other(run)) => run.push_str(&text),
                _ => found.push(Piece::Other(text)),
            }
            continue;
        }
        if character.is_alphabetic() || character == '_' {
            let mut text = String::from(character);
            while let Some(next) = chars.next_if(|c| c.is_alphanumeric() || *c == '_') {
                text.push(next);
            }
            let member = matches!(previous, Some('.' | '@' | ':'));
            previous = text.chars().last();
            significant = previous;
            found.push(Piece::Word { text, member });
            continue;
        }
        let mut text = String::from(character);
        if character == '"' {
            let mut escaped = false;
            for next in chars.by_ref() {
                text.push(next);
                match next {
                    _ if escaped => escaped = false,
                    '\\' => escaped = true,
                    '"' => break,
                    _ => {}
                }
            }
        }
        previous = text.chars().last();
        if let Some(last) = text.chars().rev().find(|c| *c != ' ' && *c != '\t') {
            significant = Some(last);
        }
        match found.last_mut() {
            Some(Piece::Other(run)) => run.push_str(&text),
            _ => found.push(Piece::Other(text)),
        }
    }
    found
}

/// `source` with each reference written as the resource it names; a word written as a
/// reference that the session never showed (and no `let` bound) is refused.
#[cfg(test)]
fn resolve_refs(source: &str, programs: &Programs) -> Result<String, String> {
    resolve_placed(source, programs).map(|resolved| resolved.source)
}

/// A program as its host compiles it, with each reference written as the resource it names, and
/// where each one was written in the program as sent: what places the compiler's errors in the
/// program its writer sent rather than in the literals it never wrote.
#[derive(Debug, Default)]
struct Resolved {
    source: String,
    /// Each reference replaced: its line, and its column and length in characters as sent and
    /// as compiled, in the order written.
    written: Vec<Written>,
}

#[derive(Debug)]
struct Written {
    line: usize,
    sent_col: usize,
    sent_len: usize,
    compiled_col: usize,
    compiled_len: usize,
}

impl Resolved {
    /// The column, in the program as sent, of the 1-based `col` of `line` as compiled: inside a
    /// literal, where its reference was written.
    fn sent_col(&self, line: usize, col: usize) -> usize {
        // What the literals before it added, and what their references took away.
        let (mut added, mut taken) = (0usize, 0usize);
        for written in self.written.iter().filter(|w| w.line == line) {
            if col < written.compiled_col {
                break;
            }
            if col < written.compiled_col.saturating_add(written.compiled_len) {
                return written.sent_col;
            }
            added = added.saturating_add(written.compiled_len);
            taken = taken.saturating_add(written.sent_len);
        }
        col.saturating_add(taken).saturating_sub(added).max(1)
    }

    /// The compiler's diagnostics of a program that did not compile (`line:col: E0xx message`),
    /// each placed in the program as sent, its resources named by their references.
    fn place_detail(&self, answer: &mut Value, programs: &mut Programs) {
        if answer.get("state").and_then(Value::as_str) != Some("rejected") {
            return;
        }
        let Some(lines) = answer.get_mut("detail").and_then(Value::as_array_mut) else {
            return;
        };
        for line in lines {
            let Some(text) = line.as_str() else {
                continue;
            };
            *line = Value::String(self.placed(text, programs));
        }
    }

    fn placed(&self, text: &str, programs: &mut Programs) -> String {
        let shown = shown_line(text, programs);
        let mut parts = shown.splitn(3, ':');
        let (Some(line), Some(col), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
            return shown;
        };
        let (Ok(line), Ok(col)) = (line.parse::<usize>(), col.parse::<usize>()) else {
            return shown;
        };
        format!("{line}:{}:{rest}", self.sent_col(line, col))
    }
}

/// Move the 1-based place `line:col` past `text`.
fn advance(text: &str, line: &mut usize, col: &mut usize) {
    for ch in text.chars() {
        if ch == '\n' {
            *line = line.saturating_add(1);
            *col = 1;
        } else {
            *col = col.saturating_add(1);
        }
    }
}

/// [`resolve_refs`], keeping where each reference was written.
fn resolve_placed(source: &str, programs: &Programs) -> Result<Resolved, String> {
    if outside_strings(source).contains('@') {
        // A literal typed by hand names a resource no answer showed.
        return Err(
            "write a resource by the reference an answer showed for it (`n3`), not as a literal"
                .to_owned(),
        );
    }
    let mut resolved = Resolved::default();
    // Where the next piece starts: its line, and its 1-based column as sent and as compiled.
    let (mut line, mut sent_col, mut compiled_col) = (1usize, 1usize, 1usize);
    for piece in pieces(source) {
        let (sent, compiled) = match piece {
            Piece::Word {
                text,
                member: false,
            } => {
                let named = programs.by_ref.get(&text).cloned().or_else(|| {
                    (text == "this" && !programs.lets.contains(&text))
                        .then(|| programs.this.clone())
                        .flatten()
                });
                match named {
                    Some(literal) => (text, literal),
                    None if looks_like_ref(&text) && !programs.lets.contains(&text) => {
                        return Err(format!(
                            "`{text}` is no reference this session showed: a reference comes \
                             from an answer (the word at the start of its line)"
                        ));
                    }
                    None => (text.clone(), text),
                }
            }
            Piece::Word { text, .. } | Piece::Other(text) => (text.clone(), text),
        };
        if sent != compiled {
            resolved.written.push(Written {
                line,
                sent_col,
                sent_len: sent.chars().count(),
                compiled_col,
                compiled_len: compiled.chars().count(),
            });
        }
        resolved.source.push_str(&compiled);
        let mut compiled_line = line;
        advance(&compiled, &mut compiled_line, &mut compiled_col);
        advance(&sent, &mut line, &mut sent_col);
    }
    Ok(resolved)
}

/// `source` without the text of its strings and regular expressions.
fn outside_strings(source: &str) -> String {
    let mut out = String::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut significant: Option<char> = None;
    let mut chars = source.chars().peekable();
    while let Some(character) = chars.next() {
        if !in_string && character == '/' && chars.peek() != Some(&'/') && starts_regex(significant)
        {
            let _ = regex_rest(&mut chars);
            significant = Some('/');
            continue;
        }
        if !in_string && !character.is_whitespace() {
            significant = Some(character);
        }
        if in_string {
            match character {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
        } else if character == '"' {
            in_string = true;
        } else {
            out.push(character);
        }
    }
    out
}

/// One line a program printed, each resource literal in it (`@notes.Note:root/notes:n2`)
/// written as its reference.
fn shown_line(line: &str, programs: &mut Programs) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some((before, after)) = rest.split_once('@') {
        out.push_str(before);
        // A diagnostic quotes a literal in backticks.
        let end = after
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | ')' | ']' | ',' | '`' | ';'))
            .unwrap_or(after.len());
        let (literal, tail) = after.split_at(end);
        let ty = literal.split_once(':').and_then(|(path, _)| {
            let (_, ty) = path.rsplit_once('.')?;
            ty.chars().next()?.is_uppercase().then_some(ty)
        });
        if let Some(ty) = ty {
            out.push_str(&programs.name(ty, &format!("@{literal}")));
        } else {
            out.push('@');
            out.push_str(literal);
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// A program the person's line ran, as the task's host answered it: its text, its host answer
/// (`{state, output, detail, lets, steps, changed, apps}`), and the references it showed
/// (`t3`), each with the resource literal it names. The API records it as the run's step
/// (`tilo_reference::ProgramStep`), which lives in the runs and nowhere here.
#[derive(Clone, Debug, Default)]
pub struct PersonStep {
    /// What the person reads: tilolang, each resource named by a short reference.
    pub text: String,
    pub failed: bool,
    /// The host's answer, when the program ran.
    pub answer: Value,
}

/// Every resource literal (`@app.Type:id`) `text` names, in the order it names them.
pub fn literals_in(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some((_, after)) = rest.split_once('@') {
        let end = after
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | ')' | ']' | ','))
            .unwrap_or(after.len());
        let (literal, tail) = after.split_at(end);
        if literal.split_once(':').is_some_and(|(path, id)| {
            !id.is_empty()
                && path.rsplit_once('.').is_some_and(|(app, ty)| {
                    !app.is_empty() && ty.chars().next().is_some_and(char::is_uppercase)
                })
        }) {
            found.push(literal.to_owned());
        }
        rest = tail;
    }
    found
}

/// What a previewed program would apply, and how to apply it.
fn preview(answer: &Value) -> String {
    let steps = answer["steps"].as_array().cloned().unwrap_or_default();
    let mut actions: Vec<(String, usize, bool)> = Vec::new();
    let mut resources: Vec<&str> = Vec::new();
    for step in &steps {
        let name = step["action"].as_str().unwrap_or("an action").to_owned();
        let destroys = step["destructive"] == true;
        match actions.iter_mut().find(|(known, _, _)| *known == name) {
            Some(entry) => {
                entry.1 = entry.1.saturating_add(1);
                entry.2 |= destroys;
            }
            None => actions.push((name, 1, destroys)),
        }
        for resource in step["resources"].as_array().into_iter().flatten() {
            if let Some(resource) = resource.as_str()
                && !resources.contains(&resource)
            {
                resources.push(resource);
            }
        }
    }
    let mut text = format!(
        "preview: {} actions on {} resources",
        steps.len(),
        resources.len()
    );
    for (name, times, destroys) in actions {
        let _ = write!(text, "\n  {name} ×{times}");
        if destroys {
            text.push_str("  destroys");
        }
    }
    if answer["complete"] == false {
        text.push_str(
            "\nthe program goes on with what these answer, so more may follow; it stopped here: ",
        );
        text.push_str(
            answer["detail"]
                .as_str()
                .unwrap_or("its preview cannot go on"),
        );
    }
    text.push_str("\nnothing changed: it runs once its person says yes");
    text
}

/// One program of a task as the task's own runner reads it back: what the agent is told, and
/// what the program did, for the runner to show the person.
#[derive(Clone, Debug, Default)]
pub struct TaskProgram {
    /// What the agent reads: the API's tilolang answer.
    pub text: String,
    /// What a person reads: the same answer in their words (`person_text`), no references.
    pub person_text: String,
    pub failed: bool,
    /// The host's answer (`{state, output, detail, steps, changed, apps}`), when the program ran.
    pub answer: Option<Value>,
    /// Each reference the text shows, with the resource literal it names.
    pub refs: Vec<(String, String)>,
    /// The program as it ran, its references written as the resources they name.
    pub source: Option<String>,
}

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// One task of agent `agent_id` of the person `of` in `workspace`, acting for `person` who
    /// started it (the Tasks agent's run of an issue): it may do what both the agent and that
    /// person may, as the agent's own calls of the run do. Each session is its own fresh
    /// execution.
    #[must_use]
    pub fn for_task(
        host: H,
        workspace: String,
        (agent_id, of): (&str, &str),
        person: Option<&str>,
    ) -> Self {
        let mut caller = tilo_actions::remote::Caller::local_agent(agent_id, of);
        if let Some(person) = person {
            caller = caller.acting_for(person);
        }
        let caller = caller.in_workspace(workspace.clone()).in_new_execution();
        Self::from_caller(host, workspace, caller)
    }

    /// Attribute every invocation of this session to `group`, the run of a task whose own
    /// runner (the Tasks agent's loop) asks this session's programs: its changes are that run's.
    #[must_use]
    pub fn with_work_group(mut self, group: tilo_actions::WorkGroup) -> Self {
        *self
            .agent_session
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(crate::Session::Given(group));
        self
    }
}

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// One program of this task for a runner of the task's own (the Tasks agent's loop) rather
    /// than an MCP client: the same step as the `tilo` tool, read back with what it did.
    /// `this` is what the person pointed at, as a resource literal, which `this` then names.
    pub async fn task_program(&self, program: &str, go: Go, this: Option<&str>) -> TaskProgram {
        if let Some(this) = this {
            let mut programs = self.programs.lock_recover();
            programs.this = Some(this.to_owned());
        }
        let (reply, answer, source) = match Box::pin(self.tilo_step(program, go)).await {
            Ok(ran) => ran,
            Err(text) => {
                return TaskProgram {
                    text,
                    failed: true,
                    ..TaskProgram::default()
                };
            }
        };
        let programs = self.programs.lock_recover();
        let mut refs: Vec<(String, String)> = Vec::new();
        for piece in pieces(&reply.text) {
            if let Piece::Word {
                text,
                member: false,
            } = piece
                && let Some(literal) = programs.by_ref.get(&text)
                && !refs.iter().any(|(known, _)| *known == text)
            {
                refs.push((text, literal.clone()));
            }
        }
        let person_text = answer
            .as_ref()
            .map_or_else(String::new, |answer| person_text(answer, &reply));
        TaskProgram {
            text: reply.text,
            person_text,
            failed: reply.failed,
            answer,
            refs,
            source,
        }
    }

    /// The `tilo` tool.
    pub(super) async fn agent_tilo(
        &self,
        args: TiloArgs,
        structured: bool,
    ) -> Result<Reply, Failure> {
        if self.programs_on_device && !self.starts_programs() {
            return Box::pin(self.tilo_on_device(args, structured)).await;
        }
        let (mut reply, answer, _) = Box::pin(self.own_program(&args.program)).await?;
        if structured {
            reply.structured = Some(self.program_answer(answer.as_ref(), &reply));
        }
        Ok(reply)
    }

    /// The `tilo` tool where the device this server's calls go to runs the programs: `args`'
    /// program as one step of this session there ([`task_programs::RUN`]), which that device runs
    /// in a task host of its own, as this session's caller.
    async fn tilo_on_device(&self, args: TiloArgs, structured: bool) -> Result<Reply, Failure> {
        use tilo_action_services::task_programs;
        use tilo_actions::InvocationUpdate;
        let mut fields = Map::new();
        fields.insert("program".into(), args.program.into());
        // A person's programs go on in this server's own program session there, so its `let`s
        // carry from one program to the next and another client's never mix in.
        if self.session_id().is_none()
            && let Some(execution) = self.caller.execution_id()
        {
            fields.insert("session".into(), execution.simple().to_string().into());
        }
        let invocation = tilo_actions::InvocationId::new();
        let request = InvokeRequest {
            asks: false,
            invocation,
            target: task_programs::target(),
            argument: InvokeArgument::Json(fields),
            context: self.context(),
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let receipt = match self
            .host()
            .invoke(&self.caller(), request)
            .await
            .ok_or_else(|| "the device did not answer".to_owned())?
        {
            Ok(receipt) => receipt,
            Err(tilo_actions::remote::ActionCallError::ConfirmationRequired(_)) => {
                return Ok(waiting(None, structured));
            }
            Err(error) => return Err(Failure::from(error)),
        };
        let output = match receipt.status {
            ReceiptStatus::Completed { output } => output,
            ReceiptStatus::Rejected { error } => return Err(Failure::from(error)),
            _ => {
                let value = self
                    .settled(invocation, crate::Hold::For(self.task_answer_wait))
                    .await?;
                match serde_json::from_value(value.get("update").cloned().unwrap_or_default()) {
                    Ok(InvocationUpdate::Succeeded { output, .. }) => output,
                    Ok(InvocationUpdate::Failed { error, .. }) => {
                        return Ok(Reply::failed(error.message));
                    }
                    Ok(InvocationUpdate::Progress { phase, message, .. })
                        if phase == tilo_actions::remote::asked::ASKED_PHASE =>
                    {
                        return Ok(waiting(message.as_deref(), structured));
                    }
                    _ => return Err(waited_for_the_task("the device's answer").into()),
                }
            }
        };
        let answer: task_programs::ProgramAnswer = output
            .value
            .and_then(|value| serde_json::from_value(value).ok())
            .ok_or_else(|| "the device answered no program's answer".to_owned())?;
        let mut reply = if answer.failed {
            Reply::failed(answer.text)
        } else {
            Reply::ok(answer.text)
        };
        if structured {
            reply.structured = Some(self.program_answer(answer.answer.as_ref(), &reply));
        }
        Ok(reply)
    }

    /// A program's answer, typed: what it printed, what it changed, the
    /// steps of this session's `history task`, and the words a person reads.
    fn program_answer(&self, answer: Option<&Value>, reply: &Reply) -> Value {
        let steps = self.programs.lock_recover().steps_json();
        // Each line as the tool says it, its resources by the references a later program and a
        // REPL entry name them by: what the model's `output` is for.
        let output: Vec<String> = {
            let mut programs = self.programs.lock_recover();
            answer
                .and_then(|answer| answer.get("output"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .flat_map(str::lines)
                .map(|line| shown_line(line, &mut programs))
                .collect()
        };
        let Some(answer) = answer else {
            // `undo`, `undo task` and `history task` answer text: the steps
            // they report are the typed answer, and a failure's words are its
            // reason.
            let mut typed = json!({
                "status": if reply.failed {"failed"} else {"ok"},
                "output": reply.text.lines().collect::<Vec<&str>>(),
                "steps": steps,
                "text": reply.text,
            });
            if reply.failed
                && let Some(fields) = typed.as_object_mut()
            {
                fields.insert("detail".into(), reply.text.clone().into());
            }
            return typed;
        };
        let status = match answer.get("state").and_then(Value::as_str) {
            Some("done") => "ok",
            _ => "failed",
        };
        let mut typed = json!({
            "status": status,
            "output": output,
            // The same lines as a person reads them: no reference literals, a day or a moment in
            // their formats, a field that is none left out. A reply the person reads takes these;
            // the model keeps `output` for its next program.
            "person_output": answer
                .get("person_output")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            "changed": answer.get("changed").cloned().unwrap_or_else(|| json!(0)),
            "apps": answer.get("apps").cloned().unwrap_or_else(|| json!([])),
            "steps": steps,
            // What a person reads: the answer's lines as they read them, each resource by its
            // title, not the reference the model names it by.
            "text": person_text(answer, reply),
        });
        if let Some(fields) = typed.as_object_mut() {
            for key in ["detail", "error"] {
                if let Some(value) = answer.get(key).filter(|value| !value.is_null()) {
                    fields.insert(key.to_owned(), value.clone());
                }
            }
        }
        typed
    }

    /// `program` as this connection's caller sends it, through the `tilo` tool: a person's own
    /// runs at once, and an agent's is previewed first, running when its person need not be asked.
    /// An agent's program that needs their yes waits for it as a step of its session on the
    /// device ([`tilo_action_services::task_programs`]), which this connection, running programs
    /// itself, cannot ask: it is refused, saying so.
    async fn own_program(
        &self,
        program: &str,
    ) -> Result<(Reply, Option<Value>, Option<String>), String> {
        if self.caller.acting().is_person() {
            return Box::pin(self.tilo_step(program, Go::Run)).await;
        }
        let previewed = Box::pin(self.tilo_step(program, Go::Preview)).await?;
        match previewed.1.as_ref() {
            Some(answer) if answer.get("state").and_then(Value::as_str) == Some("preview") => {
                if answer.get("asks").and_then(Value::as_bool) == Some(true) {
                    let (mut reply, answer, source) = previewed;
                    reply.failed = true;
                    reply.text.push_str(
                        "\nits person is asked first, and this connection cannot ask them: run it \
                         as a step of this session on the device",
                    );
                    return Ok((reply, answer, source));
                }
                Box::pin(self.tilo_step(program, Go::Run)).await
            }
            _ => Ok(previewed),
        }
    }

    /// One program of the task, gone `go`: its reply, the host's answer when a program ran, and
    /// the program as it ran.
    async fn tilo_step(
        &self,
        program: &str,
        go: Go,
    ) -> Result<(Reply, Option<Value>, Option<String>), String> {
        let program = program.trim();
        match program {
            "" => return Err("send a program: tilolang statements, one per line".to_owned()),
            "undo" => {
                return self
                    .undo_steps(false)
                    .await
                    .map(|reply| (reply, None, None))
                    .map_err(String::from);
            }
            "undo task" => {
                return self
                    .undo_steps(true)
                    .await
                    .map(|reply| (reply, None, None))
                    .map_err(String::from);
            }
            "history task" => return Ok((Reply::ok(self.history().await), None, None)),
            "catalogue" => {
                return self
                    .catalogue()
                    .await
                    .map(|catalogue| (Reply::ok(catalogue.to_string()), None, None));
            }
            _ => {}
        }
        let resolved = resolve_placed(program, &self.programs.lock_recover())?;
        let source = resolved.source.clone();
        let mut answer = self.run_in_task(&source, go).await?;
        resolved.place_detail(&mut answer, &mut self.programs.lock_recover());
        let failed = !matches!(
            answer.get("state").and_then(Value::as_str),
            Some("done" | "preview" | "suspended")
        );
        let changed_apps = answer
            .get("apps")
            .and_then(Value::as_array)
            .is_some_and(|apps| !apps.is_empty());
        if failed && changed_apps {
            // One step is all or nothing: what it changed before it failed is taken back.
            let step = crate::step::STEP.try_with(|step| *step).ok();
            if let Err(why) = self.take_back_step(step).await {
                let mut reply = self.answered(program, &answer);
                let _ = write!(
                    reply.text,
                    "\nwhat it changed could not be taken back: {why}"
                );
                return Ok((reply, Some(answer), Some(source)));
            }
        }
        Ok((self.answered(program, &answer), Some(answer), Some(source)))
    }

    /// Every installed app the task's programs may name, with its declarations, as JSON
    /// (`{alias: {app, signatures: [{name, id, kind, signature, call}]}}`): read from the
    /// packages in the task's host, so an app that runs nowhere is listed like any other.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the task's link is held while its host answers, as a program's run holds it"
    )]
    pub(super) async fn catalogue(&self) -> Result<Value, String> {
        use std::sync::atomic::Ordering;
        // A program that did not answer for a listing is not started again by
        // every listing after it: the export of a workspace lists tens of
        // paths, and none of them waits its start again. A program's call
        // starts another and clears this.
        if self.task_given_up.load(Ordering::SeqCst) {
            return Err(waited_for_the_task("its program"));
        }
        // The task's program is started on its first call, and answers one
        // line for it: neither is bounded by a timer inside it, so both are
        // waited for here, under a listing's answer budget. A listing that
        // asked nothing of programs still has to list.
        let wait = self.task_answer_wait;
        let Ok(mut link) = tokio::time::timeout(wait, self.task.lock()).await else {
            tracing::warn!(
                "a task's program held its connection past the answer budget; a listing \
                 reads no programs until a program's call ends"
            );
            return Err(waited_for_the_task("the task's connection"));
        };
        if link.is_none() {
            match tokio::time::timeout(wait, self.start_task()).await {
                Ok(Ok(task)) => *link = Some(task),
                Ok(Err(why)) => {
                    self.task_given_up.store(true, Ordering::SeqCst);
                    return Err(why);
                }
                Err(_) => {
                    self.task_given_up.store(true, Ordering::SeqCst);
                    tracing::warn!(
                        "a task's program did not start within the answer budget; a listing \
                         reads no programs, and a program's call starts another"
                    );
                    return Err(waited_for_the_task("its start"));
                }
            }
        }
        let Some(task) = link.as_mut() else {
            return Err("the task's host did not start".to_owned());
        };
        let asked = tokio::time::timeout(wait, async {
            task.send(&json!({"catalogue": {}})).await?;
            let answer = task.next().await?;
            answer
                .get("catalogue")
                .cloned()
                .ok_or_else(|| format!("the task's host answered the catalogue with {answer}"))
        })
        .await;
        let asked = if let Ok(asked) = asked {
            asked
        } else {
            tracing::warn!(
                "a task's program did not answer the catalogue within the answer budget; \
                 a listing reads no programs, and a program's call starts another"
            );
            Err(waited_for_the_task("the catalogue"))
        };
        if asked.is_err() {
            // A program that failed to answer is started afresh next time, and
            // a listing in between reads no programs rather than wait on it.
            *link = None;
            self.forget_forms();
            self.task_given_up.store(true, Ordering::SeqCst);
        } else {
            self.task_given_up.store(false, Ordering::SeqCst);
        }
        drop(link);
        asked
    }

    /// Whether this API starts programs: only then is `tilo`, the tool for them, offered, and a
    /// listing written the way they write it.
    pub(crate) const fn starts_programs(&self) -> bool {
        self.task_hosts.is_some()
    }

    /// How the task's programs write what a listing shows, when this API runs programs: each
    /// action as it is called and each type by its name ([`ProgramForms`]). `None` for an API
    /// that runs none, whose listing names actions as the API's own tools take them.
    ///
    /// Read from the task's host once a session ([`Self::forget_forms`] when that host is
    /// replaced): the host's packages are the ones it started with, so every page of a listing
    /// names what it shows by the same forms, and no page waits for the catalogue again.
    pub(super) async fn program_forms(&self) -> Option<ProgramForms> {
        if !self.starts_programs() {
            return None;
        }
        let cached = self.forms.lock_recover().clone();
        if let Some(forms) = cached {
            return Some(forms);
        }
        // optional-decode: a listing names actions by their ids when the catalogue is not there
        let forms = ProgramForms::of(&self.catalogue().await.ok()?);
        *self.forms.lock_recover() = Some(forms.clone());
        Some(forms)
    }

    /// Forget the forms the task's host declared: its connection ended, and the host started
    /// after it declares whatever its packages hold then.
    fn forget_forms(&self) {
        *self.forms.lock_recover() = None;
    }

    /// The reference a listed resource (the published `id` of a resource of `kind`) goes by in
    /// this session's programs, given it the first time it is shown, as an answer's are: a
    /// resource of a tilolang app (`tl.<app>.<Type>`), which a program passes as the value it
    /// is. `None` for any other.
    pub(super) fn listed_reference(&self, kind: &str, id: &str) -> Option<String> {
        let (app, ty) = kind.strip_prefix("tl.")?.rsplit_once('.')?;
        if !ty.starts_with(|c: char| c.is_ascii_uppercase()) {
            return None;
        }
        let inner = id.strip_prefix(&format!("tl:{app}:"))?;
        if !literal_id(inner) {
            return None;
        }
        let literal = format!("@{app}.{ty}:{inner}");
        Some(self.programs.lock_recover().name(ty, &literal))
    }

    /// A program run in this task's session as the caller this connection acts as: the
    /// person at this device, typing a line in the run's conversation (shell.md §8.1), which
    /// shares the same host and `let` scope as the agent's own `tilo` calls. The program's
    /// step is [`PersonStep`]: the `ProgramStep` the run records, and the references a later
    /// program may take (`t3`).
    pub async fn person_line(&self, source: &str) -> Result<PersonStep, String> {
        let (text, answer) = self.run_as_person(source, &self.program_author()).await?;
        let failed = !matches!(
            answer.get("state").and_then(Value::as_str),
            Some("done" | "preview" | "suspended")
        );
        Ok(PersonStep {
            text,
            failed,
            answer,
        })
    }

    /// Run `source` in the task's host as `author`, read back: its text, its host answer and
    /// the references it showed. A host that went away takes the task's `let`s with it, and the
    /// next program starts another.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the programs' guard is taken only to resolve a reference, and dropping it early \
                  across an await would leave the future unsendable"
    )]
    async fn run_as_person(&self, source: &str, author: &Value) -> Result<(String, Value), String> {
        let resolved = {
            let programs = self.programs.lock_recover();
            resolve_placed(source, &programs)?
        };
        let source = resolved.source.clone();
        let mut link = self.task.lock().await;
        if link.is_none() {
            *link = Some(self.start_task().await?);
        }
        // A program's call started the task's program: listings read from it again.
        self.task_given_up
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let Some(task) = link.as_mut() else {
            return Err("the task's host did not start".to_owned());
        };
        let ran = Box::pin(self.serve_run(task, &source, Go::Run, author)).await;
        let mut answer = match ran {
            Ok(answer) => answer,
            Err(why) => {
                *link = None;
                self.forget_forms();
                return Err(why);
            }
        };
        resolved.place_detail(&mut answer, &mut self.programs.lock_recover());
        let text = self.answered(source.as_str(), &answer).text;
        Ok((text, answer))
    }

    /// Run `source` in the task's host, doing what it reads and invokes as the task, until it
    /// answers. The host is started on the task's first program.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the task's link is held while its host runs the program, as its catalogue is read"
    )]
    async fn run_in_task(&self, source: &str, go: Go) -> Result<Value, String> {
        let mut link = self.task.lock().await;
        if link.is_none() {
            *link = Some(self.start_task().await?);
        }
        // A program's call started the task's program: listings read from it again.
        self.task_given_up
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let Some(task) = link.as_mut() else {
            return Err("the task's host did not start".to_owned());
        };
        let author = self.program_author();
        let ran = Box::pin(self.serve_run(task, source, go, &author)).await;
        if ran.is_err() {
            // A host that went away takes the task's `let`s with it; the next program starts
            // another.
            *link = None;
            self.forget_forms();
        }
        ran
    }

    async fn serve_run(
        &self,
        task: &mut TaskLink,
        source: &str,
        go: Go,
        author: &Value,
    ) -> Result<Value, String> {
        // Who wrote this program: the caller of this call. The host's session runs each program
        // as its own author, so the agent's programs and the person's lines share one session's
        // `let`s and each is attributed to whoever wrote it.
        let mut sent = Map::new();
        sent.insert(
            go.message().to_owned(),
            json!({
                "source": source,
                "author": author,
            }),
        );
        task.send(&Value::Object(sent)).await?;
        // Which instance answers for each app, found once per program.
        let mut providers: Providers = BTreeMap::new();
        loop {
            let message = task.next().await?;
            if let Some(answer) = message.get("answer") {
                return Ok(answer.clone());
            }
            let reply = if let Some(read) = message.get("store") {
                self.task_store_read(read).await
            } else if let Some(listing) = message.get("children") {
                Box::pin(self.task_children(listing, &mut providers)).await
            } else if let Some(reads) = message.get("reads") {
                Box::pin(self.task_reads(reads, &mut providers)).await
            } else if let Some(reading) = message.get("reading") {
                self.task_reading(reading).await
            } else if let Some(call) = message.get("invoke") {
                Box::pin(self.task_invoke(call, &mut providers)).await
            } else {
                json!({"err": "the API answers store reads, listings, reads, readings and invocations"})
            };
            task.send(&reply).await?;
        }
    }

    /// Who a program sent now is written by, as the task's host names it: `{actor, agent,
    /// on_behalf_of, handed}`, the caller this session acts as, and whether its person's device
    /// handed it the session and answers for what it does. The host decides nothing by it; every
    /// read and change still comes back here and passes the check as that caller.
    fn program_author(&self) -> Value {
        let caller = self.execution_caller();
        let acting = caller.acting();
        json!({
            "actor": acting.by,
            "agent": !acting.is_person(),
            "on_behalf_of": acting.acted_for_person(),
            "handed": acting.handed,
        })
    }

    /// Start this task's host.
    async fn start_task(&self) -> Result<TaskLink, String> {
        let hosts = self.task_hosts.clone().ok_or(
            "this connection runs no programs: the API serving it starts no task's host; \
             do the work with tilo_do (several actions in order with calls) and \
             tilo_read; `undo task` and `history task` are still taken here",
        )?;
        let task = self
            .session_id()
            .ok_or("this API's agent works in no run, which a task's programs are")?;
        let workspace = self.workspace.clone();
        let connection = tokio::task::spawn_blocking(move || hosts.start(&workspace, &task))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("the task's host did not start: {error}"))?;
        TaskLink::new(connection).map_err(|error| error.to_string())
    }

    /// The provider of the app `app` (an id) a task's invocation goes to, starting the app in
    /// the workspace when none of it runs yet: the task invokes the app's own API, and the
    /// workspace starts it without a window.
    async fn provider_of(&self, app: &str, known: &mut Providers) -> Result<Addressed, String> {
        if let Some(found) = known.get(app) {
            return Ok(found.clone());
        }
        let found = Addressed {
            provider: self
                .tree
                .started_provider(app, Some(std::time::Duration::from_millis(PROGRAM_WAIT_MS)))
                .await?,
            app_id: Some(app.to_owned()),
        };
        known.insert(app.to_owned(), found.clone());
        Ok(found)
    }

    /// The task's read of the resources the app `app` keeps in its record `record` (`{app,
    /// record, command}`, the record empty for the workspace's): the device's store's answer to
    /// the read command `command`, as the broker decides the task may read it now
    /// ([`tilo_actions::remote::ResourceReads::store_read`]), with no app asked: `{"ok": reply}`
    /// or `{"err": why}`.
    async fn task_store_read(&self, read: &Value) -> Value {
        let (Some(app), Some(record), Some(command)) = (
            read["app"].as_str(),
            read["record"].as_str(),
            read.get("command"),
        ) else {
            return json!({"err": "a store read names its app, record and command"});
        };
        let Some(reads) = self.host().resource_reads() else {
            return json!({"err": tilo_actions::remote::RESOURCE_READS_UNAVAILABLE});
        };
        let caller = self.execution_caller();
        let address = tilo_actions::remote::ResourceAddress {
            workspace: caller.workspace().unwrap_or_default().to_owned(),
            app: app.to_owned(),
            record: record.to_owned(),
        };
        let command = command.clone();
        // The store answers under the workspace's lock, which the read awaits.
        match reads.store_read(&caller, &address, command).await {
            Ok(reply) => json!({"ok": reply}),
            Err(error) => {
                let name = app_names(&[app.to_owned()]).join("");
                json!({"err": format!("{name}: {error}")})
            }
        }
    }

    /// What the app `app` lists under `resource` (`{app, resource}`): its directory there, as
    /// an agent reads it, `{"ok": {"rows": [{id, kind, key, name}]}}` or `{"err": why}`. A
    /// program enumerates a native app's resources through this, and reads each by the `key` its
    /// own listing gave ([`tilolang_tilo::native`]).
    async fn task_children(&self, listing: &Value, providers: &mut Providers) -> Value {
        let (Some(app), Some(resource)) = (listing["app"].as_str(), listing["resource"].as_str())
        else {
            return json!({"err": "a listing names its app and the resource it lists"});
        };
        let provider = match self.provider_of(app, providers).await {
            Ok(provider) => provider,
            Err(why) => return json!({"err": why}),
        };
        match self.children_rows(&provider, resource).await {
            Ok(rows) => json!({"ok": {"rows": rows}}),
            Err(why) => json!({"err": why}),
        }
    }

    /// Whether the resource `resource` a provider publishes (`{app, resource}`) answers a read of
    /// itself: `{"ok": {"reads": bool}}` or `{"err": why}`. A resource that does not is read by
    /// the resource the app's directory lists it under, which takes it as its `item`
    /// ([`tilolang_tilo::native`]).
    async fn task_reads(&self, reads: &Value, providers: &mut Providers) -> Value {
        let (Some(app), Some(resource)) = (reads["app"].as_str(), reads["resource"].as_str())
        else {
            return json!({"err": "a read question names its app and resource"});
        };
        let provider = match self.provider_of(app, providers).await {
            Ok(provider) => provider,
            Err(why) => return json!({"err": why}),
        };
        match self.level_reads(&provider, resource).await {
            Ok(reads) => json!({"ok": {"reads": reads}}),
            Err(why) => json!({"err": why}),
        }
    }

    /// The newest reading of the resource `resource` the app `app` publishes (`{app, resource,
    /// type}`), through the same record API a program reads an app's record by
    /// ([`tilo_actions::remote::ResourceReads::read`]): `{"ok": {"reading": …}}`, `{"none": true}`
    /// for a resource no service publishes, or `{"err": why}`. Taken at once, so the program
    /// reading it goes on within the same call: `styles.installed.offered`, `device.time.clock`, the
    /// session's windows and Settings' sections are read this way, and by the shell's parts too,
    /// so one reading serves every reader.
    async fn task_reading(&self, reading: &Value) -> Value {
        let (Some(app), Some(resource)) = (reading["app"].as_str(), reading["resource"].as_str())
        else {
            return json!({"err": "a reading names its app and resource"});
        };
        let Some(reads) = self.host().resource_reads() else {
            return json!({"none": true});
        };
        let caller = self.execution_caller();
        let read = tilo_actions::remote::ResourceReadRequest {
            workspace: caller.workspace().unwrap_or_default().to_owned(),
            app: app.to_owned(),
            record: resource.to_owned(),
            joined: false,
            after: None,
            wait_ms: None,
            // A document is answered as one, so a reading tells itself apart.
            answer: tilo_actions::remote::ReadAnswer::Document,
        };
        match tilo_actions::remote::answered(reads, &caller, read).await {
            // An app's record is read through the record API, never as one of the session's
            // readings, as a resource no service publishes is none of its.
            Ok(answer) => match answer.pointer("/records/0/value") {
                Some(value) => json!({"ok": {"reading": value}}),
                None => json!({"none": true}),
            },
            Err(error) => {
                let name = app_names(&[app.to_owned()]).join("");
                json!({"err": format!("{name}: {error}")})
            }
        }
    }

    /// The task's invocation `call` (`{app, resource, action, arguments}`): invoked at the
    /// action's own app as the task, in this call's step, decided there.
    async fn task_invoke(&self, call: &Value, providers: &mut Providers) -> Value {
        let (Some(app), Some(resource), Some(action)) = (
            call["app"].as_str(),
            call["resource"].as_str(),
            call["action"].as_str(),
        ) else {
            return json!({"failed": "an invocation names its app, resource and action"});
        };
        // A change of a tilolang app's record goes to the record's origin, which runs the
        // action against the record as it is then; no app's instance is asked.
        if let Some(record) = call.get("record").and_then(Value::as_str) {
            return self
                .task_change(
                    app,
                    record,
                    resource,
                    action,
                    call["arguments"].as_object().cloned().unwrap_or_default(),
                )
                .await;
        }
        // An action of a resource the session itself publishes (its shell, its layout) goes
        // to the session's own provider, as every other caller of the desktop reaches it.
        let provider = if call.get("published").and_then(Value::as_bool) == Some(true) {
            Addressed {
                provider: crate::session_api::SESSION_PROVIDER.to_owned(),
                app_id: Some(crate::session_api::SESSION_PROVIDER.to_owned()),
            }
        } else {
            match self.provider_of(app, providers).await {
                Ok(provider) => provider,
                Err(why) => return json!({"failed": why}),
            }
        };
        let arguments = call["arguments"].as_object().cloned().unwrap_or_default();
        let value = Box::pin(self.invoke_one(OneCall {
            provider,
            workspace: false,
            revision: None,
            resource: resource.to_owned(),
            action: action.to_owned(),
            arguments,
            invocation_id: None,
            wait_ms: Some(PROGRAM_WAIT_MS),
        }))
        .await;
        program_answer(action, value)
    }

    /// The task's change of `app`'s record `record`: an action of the record's own provider, run
    /// through the one dispatch as every other call is, so the workspace's invocations record keeps
    /// it as this task's step.
    async fn task_change(
        &self,
        app: &str,
        record: &str,
        resource: &str,
        action: &str,
        arguments: Map<String, Value>,
    ) -> Value {
        let value = Box::pin(self.invoke_one(OneCall {
            provider: Addressed {
                provider: format!("{}:{app}:{record}", tilo_actions::remote::RESOURCE_PROVIDER),
                app_id: Some(app.to_owned()),
            },
            workspace: false,
            revision: None,
            resource: resource.to_owned(),
            action: action.to_owned(),
            arguments,
            invocation_id: None,
            wait_ms: Some(PROGRAM_WAIT_MS),
        }))
        .await;
        program_answer(action, value)
    }

    /// The text of a program's answer, its resources named by references.
    fn answered(&self, program: &str, answer: &Value) -> Reply {
        let (text, failed) = answer_text(&mut self.programs.lock_recover(), program, answer);
        if failed {
            Reply::failed(text)
        } else {
            Reply::ok(text)
        }
    }

    /// `history task`: the steps of this task, and this agent's runs in the
    /// workspace, newest first, so an agent reads what it did in earlier
    /// sessions too.
    async fn history(&self) -> String {
        let steps = history_text(&self.programs.lock_recover());
        let who = if self.caller.acting().is_agent() {
            "this agent"
        } else {
            "you"
        };
        match self.agent_runs().await {
            Ok(runs) => history_answer(&steps, who, &runs),
            Err(why) => {
                let line = format!("the workspace's runs could not be read: {why}");
                if steps.is_empty() {
                    line
                } else {
                    format!("{steps}\n{line}")
                }
            }
        }
    }

    /// This agent's runs in the workspace, newest first, as its host lists
    /// them: the runs of this one member, so another agent's are not read.
    async fn agent_runs(&self) -> Result<Invocations, String> {
        let acting = self.caller.acting();
        let by = acting
            .agent_id()
            .or_else(|| acting.member_id().map(collab_types::MemberId::id))
            .map(str::to_owned);
        self.listed_invocations(&InvocationsQuery {
            limit: workspace_invocations::PAGE,
            by,
            ..InvocationsQuery::default()
        })
        .await
    }

    /// The page of the workspace's invocations `query` asks for, read through the record API as
    /// every record is: the whole record of them, its items narrowed and paged here.
    pub(super) async fn listed_invocations(
        &self,
        query: &InvocationsQuery,
    ) -> Result<Invocations, String> {
        let Some(reads) = self.host.resource_reads() else {
            return Err(tilo_actions::remote::RESOURCE_READS_UNAVAILABLE.to_owned());
        };
        let caller = self.execution_caller();
        let read = tilo_actions::remote::ResourceReadRequest {
            workspace: caller.workspace().unwrap_or_default().to_owned(),
            app: workspace_invocations::PROVIDER.to_owned(),
            record: String::new(),
            joined: false,
            after: None,
            wait_ms: None,
            answer: tilo_actions::remote::ReadAnswer::Reading,
        };
        let answer = tilo_actions::remote::answered(reads, &caller, read)
            .await
            .map_err(|error| error.to_string())?;
        let Some(value) = answer.pointer("/records/0/value") else {
            return Err("the workspace answered no list of runs".to_owned());
        };
        let items: Vec<Invocation> = serde_json::from_value(
            value
                .get("items")
                .cloned()
                .unwrap_or(Value::Array(Vec::new())),
        )
        .map_err(|error| format!("the runs do not read: {error}"))?;
        Ok(page_of(items, query))
    }
}

/// `items`, the workspace's runs newest first, narrowed by `query` and cut to its page: what a
/// reader of the record reads without paging the record itself.
fn page_of(items: Vec<Invocation>, query: &InvocationsQuery) -> Invocations {
    let items: Vec<Invocation> = items
        .into_iter()
        .filter(|item| {
            query
                .by
                .as_deref()
                .is_none_or(|by| item.by.id.as_deref() == Some(by))
        })
        .filter(|item| {
            query.invocation.as_deref().is_none_or(|named| {
                item.id.as_deref() == Some(named)
                    || item.step.as_deref() == Some(named)
                    || item.calls.iter().any(|call| call == named)
            })
        })
        .filter(|item| query.status.is_none_or(|status| item.status == status))
        .collect();
    let total = items.len();
    let waiting = items
        .iter()
        .filter(|item| item.status == InvocationStatus::Waiting)
        .count();
    let offset = query
        .cursor
        .as_deref()
        .and_then(|cursor| cursor.parse::<usize>().ok())
        .unwrap_or(0);
    let page = query.page();
    let next_cursor =
        (offset.saturating_add(page) < total).then(|| offset.saturating_add(page).to_string());
    let items = items.into_iter().skip(offset).take(page).collect();
    Invocations {
        items,
        total,
        waiting,
        pending: false,
        next_cursor,
    }
}

/// The instance that answers for each app, by app id.
type Providers = BTreeMap<String, Addressed>;

/// Whether the API's error `why` is the policy's refusal, not a failure of the action.
const fn refused(why: &super::Failure) -> bool {
    use tilo_actions::remote::ActionCallError;
    matches!(
        why.host,
        Some(
            ActionCallError::NotPermitted
                | ActionCallError::Refused { .. }
                | ActionCallError::ConfirmationRequired { .. }
                | ActionCallError::UnknownAction
        )
    )
}

/// The text of a program's answer as a person reads it: what the program printed in their words
/// (`person_output`: no reference literals, days and moments in their formats), and the step's own
/// bookkeeping line. Where the task answered no person line — a program that only changed
/// something — the model's text with its references dropped is what is left.
fn person_text(answer: &Value, reply: &Reply) -> String {
    let person: Vec<&str> = answer["person_output"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .flat_map(str::lines)
        .collect();
    let bookkeeping: Vec<&str> = reply
        .text
        .lines()
        .filter(|line| line.starts_with("step "))
        .collect();
    let mut lines: Vec<&str> = person;
    lines.extend(bookkeeping);
    if lines.is_empty() {
        reply.text.clone()
    } else {
        lines.join("\n")
    }
}

/// The text of a program's answer in `programs`' session, and whether it is an error.
fn answer_text(programs: &mut Programs, program: &str, answer: &Value) -> (String, bool) {
    {
        programs.lets = answer["lets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|binding| binding["name"].as_str().map(str::to_owned))
            .collect();
        let mut lines: Vec<String> = answer["output"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .flat_map(str::lines)
            .map(|line| shown_line(line, programs))
            .collect();
        let changed = answer["changed"].as_u64().unwrap_or(0);
        let state = answer["state"].as_str().unwrap_or_default();
        let failed = match state {
            "done" => {
                if changed > 0 {
                    let number = programs.steps.len().saturating_add(1);
                    let apps: Vec<String> = answer["apps"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect();
                    lines.push(format!(
                        "step {number}: {changed} {} in {}; `undo` takes it back",
                        if changed == 1 { "change" } else { "changes" },
                        app_names(&apps).join(" and ")
                    ));
                    programs.steps.push(Step {
                        number,
                        apps,
                        program: program.lines().next().unwrap_or_default().to_owned(),
                        changed,
                        undone: false,
                        kept: false,
                        gesture: crate::step::STEP.try_with(|step| *step).ok(),
                    });
                }
                false
            }
            "preview" => {
                lines.push(preview(answer));
                false
            }
            "suspended" => {
                lines.push(format!(
                    "waiting: {} asks the person first",
                    answer
                        .pointer("/detail/action")
                        .and_then(Value::as_str)
                        .unwrap_or("an action")
                ));
                false
            }
            "rejected" => {
                lines.push("error: the program does not compile; nothing ran".to_owned());
                lines.extend(
                    answer["detail"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned),
                );
                true
            }
            _ => {
                let kept = if answer["changed"].as_u64().unwrap_or(0) > 0 {
                    "what it changed is taken back"
                } else {
                    "it changed nothing"
                };
                let detail = answer["detail"].as_str().unwrap_or("the program failed");
                let mut shown = detail.lines().map(|line| shown_line(line, programs));
                lines.push(format!("error: {}", shown.next().unwrap_or_default()));
                lines.extend(shown);
                lines.push(kept.to_owned());
                true
            }
        };
        let text = if lines.is_empty() {
            "ok".to_owned()
        } else {
            lines.join("\n")
        };
        (text, failed)
    }
}

/// The steps of `programs`' session, oldest first; empty when it made none.
fn history_text(programs: &Programs) -> String {
    if programs.steps.is_empty() {
        return String::new();
    }
    let mut text = String::from("steps of this task, oldest first");
    for step in &programs.steps {
        let _ = write!(
            text,
            "\n{}  {}  {} {}  {}{}",
            step.number,
            app_names(&step.apps).join(" and "),
            step.changed,
            if step.changed == 1 {
                "change"
            } else {
                "changes"
            },
            step.program,
            if step.undone {
                "  (undone)"
            } else if step.kept {
                "  (could not be undone)"
            } else {
                ""
            }
        );
    }
    text
}

/// `history task`'s text: the steps this task took and can still take back,
/// then the runs of `who` in the workspace.
fn history_answer(steps: &str, who: &str, runs: &Invocations) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !steps.is_empty() {
        parts.push(format!(
            "{steps}\n`undo` takes back this task's newest step still in effect"
        ));
    }
    if !runs.items.is_empty() {
        parts.push(runs_text(who, runs));
    }
    if parts.is_empty() {
        return format!("no run of {who} changed anything in this workspace yet");
    }
    parts.join("\n\n")
}

/// What `who` reads of its runs in a workspace: one line each, newest first,
/// saying what changed where and how it stands.
fn runs_text(who: &str, runs: &Invocations) -> String {
    let mut text = format!(
        "runs by {who} in this workspace, newest first ({} {})",
        runs.total,
        if runs.total == 1 { "run" } else { "runs" }
    );
    for run in &runs.items {
        let _ = write!(text, "\n{}", run_line(run));
    }
    if runs.next_cursor.is_some() {
        let _ = write!(
            text,
            "\nshowing the newest {} of {}",
            runs.items.len(),
            runs.total
        );
    }
    text
}

/// One run as `history task` lists it: how many changes, in which apps, what
/// they did, and how it stands.
fn run_line(run: &Invocation) -> String {
    let mut apps: Vec<&str> = Vec::new();
    for record in &run.records {
        if !record.app.is_empty() && !apps.contains(&record.app.as_str()) {
            apps.push(&record.app);
        }
    }
    let apps = if apps.is_empty() {
        "the workspace".to_owned()
    } else {
        apps.join(", ")
    };
    let mut line = format!(
        "{} {} in {apps}",
        run.changes,
        if run.changes == 1 {
            "change"
        } else {
            "changes"
        }
    );
    if run.undone > 0 && run.status != InvocationStatus::Undone {
        let _ = write!(line, " ({} undone)", run.undone);
    }
    if let Some(summary) = &run.summary {
        let _ = write!(line, ": {summary}");
    } else if let Some(action) = &run.action {
        let _ = write!(line, ": {action}");
    }
    if let Some(title) = &run.title {
        let _ = write!(line, " (run \"{title}\")");
    }
    let status = match run.status {
        InvocationStatus::Ended => None,
        InvocationStatus::Waiting => Some("waiting for the person"),
        InvocationStatus::Running => Some("running"),
        InvocationStatus::Failed => Some("failed"),
        InvocationStatus::Cancelled => Some("cancelled"),
        InvocationStatus::Undone => Some("taken back"),
    };
    match status {
        Some(status) => format!("{status}: {line}"),
        None => line,
    }
}

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// Take back the calls of the step `step` names, newest first, each through its own record's
    /// take-back (`<invocation>#undo`): the workspace's invocations record keeps them by the
    /// step, and what each changed, in every record, is found there.
    async fn take_back_step(&self, step: Option<tilo_actions::ActionId>) -> Result<(), String> {
        let Some(step) = step.map(|step| step.0.to_string()) else {
            return Ok(());
        };
        let calls = self.own_invocations().await?;
        for call in calls.iter().filter(|call| {
            call.step.as_deref() == Some(step.as_str()) && call.status != InvocationStatus::Undone
        }) {
            let Some(id) = &call.id else {
                continue;
            };
            // A call that changed no record (a read) has nothing to take back.
            if let Err(why) = Box::pin(self.undo_invocation(id, false)).await
                && !why.message.contains(workspace_invocations::NO_HISTORY)
            {
                return Err(why.message);
            }
        }
        Ok(())
    }

    /// Take back the last step that is in effect, or with `all` every step of this task, newest
    /// first.
    async fn undo_steps(&self, all: bool) -> Result<Reply, Failure> {
        let mut text = String::new();
        loop {
            let next = {
                let programs = self.programs.lock_recover();
                programs
                    .steps
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, step)| !step.undone && !step.kept)
                    .map(|(index, step)| (index, step.clone()))
            };
            let Some((index, step)) = next else {
                if text.is_empty() {
                    return Ok(Reply::ok("nothing of this task's to undo".to_owned()));
                }
                break;
            };
            if let Err(why) = self.take_back_step(step.gesture).await {
                // The step stays as it is; the steps before it are still the task's to take
                // back, so the next `undo` goes on to them rather than failing here again.
                let before = {
                    let mut programs = self.programs.lock_recover();
                    if let Some(kept) = programs.steps.get_mut(index) {
                        kept.kept = true;
                    }
                    programs
                        .steps
                        .iter()
                        .rev()
                        .find(|step| !step.undone && !step.kept)
                        .map(|step| step.number)
                };
                let _ = write!(
                    text,
                    "{}undoing step {}: {why}; it stays{}",
                    if text.is_empty() { "" } else { "\n" },
                    step.number,
                    before.map_or_else(String::new, |number| format!(
                        ", and `undo` takes back step {number} next"
                    )),
                );
                return Ok(Reply::failed(text));
            }
            if let Some(undone) = self.programs.lock_recover().steps.get_mut(index) {
                undone.undone = true;
            }
            let _ = write!(
                text,
                "{}undone: step {} in {} ({})",
                if text.is_empty() { "" } else { "\n" },
                step.number,
                app_names(&step.apps).join(" and "),
                step.program
            );
            if !all {
                break;
            }
        }
        Ok(Reply::ok(text))
    }
}

/// A program's answer to its call of `action`: its output when it succeeded, with the invocation
/// that takes it back, or why it did not.
fn program_answer(action: &str, value: Result<Value, Failure>) -> Value {
    match value {
        Ok(value) => match value.pointer("/update/state").and_then(Value::as_str) {
            Some("succeeded") => {
                let mut output = value
                    .pointer("/update/output")
                    .cloned()
                    .unwrap_or(Value::Null);
                // What takes this change back: the invocation it was, as the workspace
                // lists it, and so the documents it changed.
                if let Some(invocation) = value
                    .pointer("/update/invocation")
                    .or_else(|| value.get("invocation_id"))
                    .and_then(Value::as_str)
                    && let Some(fields) = output.as_object_mut()
                {
                    fields.insert("invocation".to_owned(), json!(invocation));
                }
                json!({"ok": output})
            }
            // Failed, cancelled, or not finished in time: it may still run, so the
            // program cannot go on as if it had not.
            _ => json!({"failed": super::outcome(&value).text}),
        },
        Err(why) if refused(&why) => json!({"refused": format!("{action}: {why}")}),
        Err(why) => json!({"failed": String::from(why)}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::InvocationId;
    use tilo_actions::remote::{
        ActionCallError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt, InvocationState,
        InvokeRequest, Pending,
    };

    tilo_test_env::scenario! {
        fn a_task_answers_its_programs_and_keeps_its_steps;

        phase a_program_answers_typed_with_its_output_and_the_task_steps;
        // A program's answer typed: what it printed, what it changed, and the
        // steps of the task's history beside it.
        {
            let host = Native::new();
            let server = McpServer::for_person(host, "workspace".to_owned(), "owner");
            server.programs.lock_recover().steps.push(Step {
                number: 1,
                apps: vec!["so.tilo.notes".to_owned()],
                program: "notes.pin(n1)".to_owned(),
                changed: 1,
                undone: false,
                kept: false,
                gesture: None,
            });
            let answer = json!({
                "state": "done",
                "output": ["n1 \"Q3 plan\""],
                "changed": 2,
                "apps": ["so.tilo.notes"],
            });
            let typed = server.program_answer(Some(&answer), &Reply::ok(String::new()));
            assert_eq!(typed["status"], "ok");
            assert_eq!(typed["output"], json!(["n1 \"Q3 plan\""]));
            assert_eq!(typed["changed"], 2);
            assert_eq!(typed["apps"], json!(["so.tilo.notes"]));
            assert_eq!(typed["steps"][0]["program"], "notes.pin(n1)");
            // A failed program carries its detail.
            let refused = server.program_answer(
                Some(&json!({"state":"rejected","detail":["line 1: no such field"]})),
                &Reply::failed("error".to_owned()),
            );
            assert_eq!(refused["status"], "failed");
            assert_eq!(refused["detail"], json!(["line 1: no such field"]));
            // The tools that answer text carry their lines as the typed output.
            let listed = server.program_answer(None, &Reply::ok("ok\nstep 1: 1 change".to_owned()));
            assert_eq!(listed["status"], "ok");
            assert_eq!(listed["output"], json!(["ok", "step 1: 1 change"]));
        }

        phase references_name_what_answers_showed_and_nothing_else;
        {
            let mut programs = Programs::default();
            assert_eq!(
                shown_line("@notes.Note:root/notes:a \"Q3 plan\"", &mut programs),
                "n1 \"Q3 plan\""
            );
            assert_eq!(
                resolve_refs("notes.pin(n1)", &programs).unwrap(),
                "notes.pin(@notes.Note:root/notes:a)"
            );
            let invented = resolve_refs("notes.pin(n7)", &programs).unwrap_err();
            assert!(
                invented.contains("no reference this session showed"),
                "{invented}"
            );
            assert!(resolve_refs("notes.pin(@notes.Note:x)", &programs).is_err());
            // A word in a string, or a member, is not a reference.
            assert_eq!(
                resolve_refs("print(\"n7\", x.n7)", &programs).unwrap(),
                "print(\"n7\", x.n7)"
            );
        }

        phase a_regular_expression_holds_no_references_and_no_literal;
        {
            let programs = Programs::default();
            let grep = r"grep(/\bt1\b|x@y/i, in: notes).count_by(.doc)";
            assert_eq!(resolve_refs(grep, &programs).unwrap(), grep);
            // After a value, a slash divides; the reference after it is still checked.
            let divided = resolve_refs("print(10 / t7)", &programs).unwrap_err();
            assert!(divided.contains("`t7`"), "{divided}");
        }

        phase a_preview_counts_the_actions_and_resources;
        {
            let answer = json!({"steps": [
                {"action": "pin", "resources": ["a", "root"], "destructive": false},
                {"action": "pin", "resources": ["b", "root"], "destructive": false},
                {"action": "delete_card", "resources": ["c"], "destructive": true},
            ]});
            assert_eq!(
                preview(&answer),
                "preview: 3 actions on 4 resources\n  pin ×2\n  delete_card ×1  destroys\nnothing changed: it runs once its person says yes"
            );
        }

        phase a_step_in_two_apps_is_one_step_of_the_task;
        {
            let mut programs = Programs::default();
            let answer = json!({
                "state": "done",
                "output": ["@notes.Note:root-a/n1 \"Q3 plan\""],
                "changed": 2,
                "apps": ["so.tilo.notes", "so.tilo.lang.todo"],
                "lets": [{"name": "plan", "type": "so.tilo.notes.Note"}],
            });
            let (text, failed) =
                answer_text(&mut programs, "notes.pin(n1)\ntodo.add(t1, \"x\")", &answer);
            assert!(!failed, "{text}");
            assert_eq!(
                text,
                "n1 \"Q3 plan\"\nstep 1: 2 changes in notes and todo; `undo` takes it back"
            );
            assert_eq!(programs.lets, ["plan"]);
            assert_eq!(
                history_text(&programs),
                "steps of this task, oldest first\n1  notes and todo  2 changes  notes.pin(n1)"
            );
        }

        phase a_failed_step_says_what_became_of_its_changes;
        {
            let mut programs = Programs::default();
            let answer = json!({"state": "failed", "detail": "trap: no", "changed": 1, "apps": ["so.tilo.lang.todo"]});
            let (text, failed) = answer_text(&mut programs, "x", &answer);
            assert!(failed);
            assert_eq!(text, "error: trap: no\nwhat it changed is taken back");
            assert!(programs.steps.is_empty(), "a failed program is no step");
        }

        phase the_policys_refusals_are_told_from_failures;
        {
            use tilo_actions::remote::ActionCallError;
            let host = |error: ActionCallError| {
                crate::Failure::from(crate::BridgeError::Host(error))
            };
            assert!(refused(&host(ActionCallError::NotPermitted)));
            assert!(refused(&host(ActionCallError::Refused {
                reason: "asks the person first".into()
            })));
            assert!(refused(&host(ActionCallError::UnknownAction)));
            assert!(!refused(&crate::Failure::from("the action failed: no such note")));
        }
    }

    /// A workspace that offers nothing: what a program's answer is typed against.
    #[derive(Clone)]
    struct Native;

    impl Native {
        const fn new() -> Self {
            Self
        }
    }

    impl ActionProvider for Native {
        fn query(
            &self,
            _caller: &Caller,
            _query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            Pending::ready(Ok(CataloguePage::default()))
        }

        fn invoke(
            &self,
            _caller: &Caller,
            _request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            Pending::ready(Err(ActionCallError::UnknownAction))
        }

        fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
            None
        }

        fn cancel(
            &self,
            _caller: &Caller,
            _id: &InvocationId,
        ) -> Pending<Result<(), ActionCallError>> {
            Pending::ready(Ok(()))
        }
    }

    /// A program that does not compile is answered in the program its writer sent: the compiler
    /// placed its errors in the literals the references were written as, which the writer never
    /// saw, and named the resources by them.
    #[test]
    fn a_compile_error_is_placed_in_the_program_as_sent() {
        let mut programs = Programs::default();
        shown_line("@notes.Note:root/notes:a \"Q3 plan\"", &mut programs);
        let resolved = resolve_placed("n1.edited\nprint(n1, n1.titel)", &programs).unwrap();
        assert_eq!(
            resolved.source,
            "@notes.Note:root/notes:a.edited\nprint(@notes.Note:root/notes:a, @notes.Note:root/notes:a.titel)"
        );
        let mut answer = json!({
            "state": "rejected",
            "detail": [
                "1:26: E021 `Note` has no field `edited`; fields are: title, body",
                "2:7: E020 `@notes.Note:root/notes:a` is not printable",
                "2:58: E021 `Note` has no field `titel`; did you mean `title`?",
            ],
        });
        resolved.place_detail(&mut answer, &mut programs);
        assert_eq!(
            answer["detail"],
            json!([
                "1:4: E021 `Note` has no field `edited`; fields are: title, body",
                "2:7: E020 `n1` is not printable",
                "2:14: E021 `Note` has no field `titel`; did you mean `title`?",
            ])
        );
    }

    /// A program refused by the compiler is answered with the compiler's own lines: each error's
    /// place, code and the name it is about, so the person and the model read what to change
    /// rather than only that nothing ran.
    #[test]
    fn a_programs_refusal_names_each_error_where_the_compiler_placed_it() {
        let mut programs = Programs::default();
        let answer = json!({
            "state": "rejected",
            "detail": [
                "1:1: E014 unknown name `session`; variables in scope: none",
                "1:42: E037 cannot tell which enum `.none` belongs to here",
            ],
        });
        let (text, failed) = answer_text(&mut programs, "session.overview(open: .none)", &answer);
        assert!(failed, "a refusal is an error: {text}");
        assert_eq!(
            text,
            "error: the program does not compile; nothing ran\n\
             1:1: E014 unknown name `session`; variables in scope: none\n\
             1:42: E037 cannot tell which enum `.none` belongs to here"
        );
    }

    /// `history task` line by line: a run says what changed and where, and
    /// how it stands, never the values it carried (the host's words do not).
    #[test]
    fn history_lists_a_runs_changes_and_how_it_stands() {
        use tilo_action_services::workspace_invocations::{
            Invocation, InvocationBy, InvocationChange, InvocationStatus, Invocations,
        };
        let noted = |status: InvocationStatus, undone: usize| Invocation {
            by: InvocationBy {
                name: "Claude".into(),
                kind: "agent".into(),
                id: Some("claude".into()),
            },
            status,
            changes: 2,
            undone,
            summary: Some("Added a task".into()),
            records: vec![
                InvocationChange {
                    app: "tasks".into(),
                    ..InvocationChange::default()
                },
                InvocationChange {
                    app: "tasks".into(),
                    ..InvocationChange::default()
                },
                InvocationChange {
                    app: "notes".into(),
                    ..InvocationChange::default()
                },
            ],
            title: Some("Tidy".into()),
            ..Invocation::default()
        };
        let runs = Invocations {
            items: vec![
                noted(InvocationStatus::Ended, 0),
                noted(InvocationStatus::Waiting, 0),
                noted(InvocationStatus::Undone, 2),
            ],
            total: 3,
            waiting: 1,
            pending: false,
            next_cursor: None,
        };
        assert_eq!(
            history_answer("", "this agent", &runs),
            "runs by this agent in this workspace, newest first (3 runs)\n\
             2 changes in tasks, notes: Added a task (run \"Tidy\")\n\
             waiting for the person: 2 changes in tasks, notes: Added a task (run \"Tidy\")\n\
             taken back: 2 changes in tasks, notes: Added a task (run \"Tidy\")"
        );
        assert_eq!(
            history_answer("", "this agent", &Invocations::default()),
            "no run of this agent changed anything in this workspace yet"
        );
    }
}
