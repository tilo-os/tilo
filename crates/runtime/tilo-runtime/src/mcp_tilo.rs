//! The `tilo` tool: an agent's task writes tilolang programs
//! (`docs/designs/app-language/agents.md`, "How a program runs").
//!
//! A program runs as the task, in the task's own tilolang host: one per task, its code author the
//! task, started by the session ([`TaskHosts`]) on the task's first `tilo` call and living as long
//! as this connection. The host holds the task's `let`s and no records. It compiles each program
//! against every installed app and runs it; what the program reads and changes comes back here on
//! the task's program connection, and this door does it as the task, with the gate, attribution
//! and grants of every other call:
//!
//! - a read is the app's `read-values`, a read of the resource as the task;
//! - a change is an invocation of the action's own app: a method its type's declared action, an
//!   assignment the resource's field edit. Each is decided at that action's node, asked about or
//!   refused as the workspace's policy says, never done silently. A program never writes a record.
//!
//! One call is one step: every invocation it makes carries the call's undo step and the task's
//! run, so `undo` takes back the last step, in every app it changed, and `undo task` every step of
//! this task; `history task` lists them. A program that fails takes back what it changed. One that
//! would change many resources or destroy something answers with a preview, from a provisional run
//! that sends nothing, until it is sent again with `confirm`.
//!
//! What goes back is tilolang text. Every resource it shows is named by a short reference (`n3`,
//! `t91`) at the start of its line, which later programs of this session take; a reference the
//! session never showed is refused, so none can be invented.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

use tilo_lock::MutexRecover as _;

use super::{ActionHost, Addressed, McpServer, OneCall, Reply, app_names};

/// Where a door starts the tilolang host of a task: the session, which launches it as every
/// tilolang host is launched, its author the task.
pub trait TaskHosts: Send + Sync {
    /// Start the host of the task `task` (the agent run) in `room`: the door's end of the task's
    /// program connection.
    ///
    /// # Errors
    /// The host could not be started.
    fn start(&self, room: &str, task: &str) -> std::io::Result<std::os::fd::OwnedFd>;
}

/// The program connection to a task's host, one JSON object per line.
pub(crate) struct TaskLink {
    reader: tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl TaskLink {
    fn new(fd: std::os::fd::OwnedFd) -> std::io::Result<Self> {
        let stream = std::os::unix::net::UnixStream::from(fd);
        stream.set_nonblocking(true)?;
        let (reader, writer) = tokio::net::UnixStream::from_std(stream)?.into_split();
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
    #[serde(default)]
    confirm: bool,
}

/// One step a program of this task made.
#[derive(Clone, Debug)]
struct Step {
    /// Its number in `history task`, from 1.
    number: usize,
    /// The apps it changed, by id, in the order it first changed each.
    apps: Vec<String>,
    /// Where each of them takes a step back: the app's id and its resource the history verbs
    /// are published on, as the task's host named them.
    history: Vec<History>,
    /// The program's first line, as `history task` names the step.
    program: String,
    changed: u64,
    undone: bool,
}

/// Where one app a step changed takes it back: by the app's id and its resource, never by a
/// name, which several apps may answer to.
#[derive(Clone, Debug, PartialEq, Eq)]
struct History {
    app: String,
    resource: String,
}

/// What a program's answer says each app it changed takes a step back on.
fn history_of(answer: &Value) -> Vec<History> {
    answer
        .get("history")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            Some(History {
                app: entry.get("app")?.as_str()?.to_owned(),
                resource: entry.get("resource")?.as_str()?.to_owned(),
            })
        })
        .collect()
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
}

impl Programs {
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
fn resolve_refs(source: &str, programs: &Programs) -> Result<String, String> {
    if outside_strings(source).contains('@') {
        // A literal typed by hand names a resource no answer showed.
        return Err(
            "write a resource by the reference an answer showed for it (`n3`), not as a literal"
                .to_owned(),
        );
    }
    let mut out = String::new();
    for piece in pieces(source) {
        match piece {
            Piece::Word {
                text,
                member: false,
            } => {
                if let Some(literal) = programs.by_ref.get(&text) {
                    out.push_str(literal);
                    continue;
                }
                if looks_like_ref(&text) && !programs.lets.contains(&text) {
                    return Err(format!(
                        "`{text}` is no reference this session showed: a reference comes from an \
                         answer (the word at the start of its line)"
                    ));
                }
                out.push_str(&text);
            }
            Piece::Word { text, .. } | Piece::Other(text) => out.push_str(&text),
        }
    }
    Ok(out)
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
        let end = after
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | ')' | ']' | ','))
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
    text.push_str("\nnothing changed; send the same program with confirm: true to apply it");
    text
}

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// The `tilo` tool.
    pub(super) async fn agent_tilo(&self, args: TiloArgs) -> Result<Reply, String> {
        let program = args.program.trim();
        match program {
            "" => return Err("send a program: tilolang statements, one per line".to_owned()),
            "undo" => return self.undo_steps(false).await,
            "undo task" => return self.undo_steps(true).await,
            "history task" => return Ok(Reply::ok(self.history())),
            "catalogue" => return self.catalogue().await,
            _ => {}
        }
        let source = resolve_refs(program, &self.programs.lock_recover())?;
        let answer = self.run_in_task(&source, args.confirm).await?;
        let failed = !matches!(
            answer.get("state").and_then(Value::as_str),
            Some("done" | "preview" | "suspended")
        );
        let apps: Vec<String> = answer
            .get("apps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        if failed && !apps.is_empty() {
            // One step is all or nothing: what it changed before it failed is taken back.
            if let Err(why) = self.undo_in(&history_of(&answer)).await {
                let mut reply = self.answered(program, &answer);
                let _ = write!(
                    reply.text,
                    "\nwhat it changed could not be taken back: {why}"
                );
                return Ok(reply);
            }
        }
        Ok(self.answered(program, &answer))
    }

    /// Every installed app the task's programs may name, with its declarations, as JSON
    /// (`{alias: {app, signatures: [{name, signature}]}}`): read from the packages in the task's
    /// host, so an app that runs nowhere is listed like any other.
    async fn catalogue(&self) -> Result<Reply, String> {
        let mut link = self.task.lock().await;
        if link.is_none() {
            *link = Some(self.start_task().await?);
        }
        let Some(task) = link.as_mut() else {
            return Err("the task's host did not start".to_owned());
        };
        let asked = async {
            task.send(&json!({"catalogue": {}})).await?;
            let answer = task.next().await?;
            answer
                .get("catalogue")
                .cloned()
                .ok_or_else(|| format!("the task's host answered the catalogue with {answer}"))
        };
        match asked.await {
            Ok(catalogue) => Ok(Reply::ok(catalogue.to_string())),
            Err(why) => {
                *link = None;
                Err(why)
            }
        }
    }

    /// Run `source` in the task's host, doing what it reads and invokes as the task, until it
    /// answers. The host is started on the task's first program.
    async fn run_in_task(&self, source: &str, confirm: bool) -> Result<Value, String> {
        let mut link = self.task.lock().await;
        if link.is_none() {
            *link = Some(self.start_task().await?);
        }
        let Some(task) = link.as_mut() else {
            return Err("the task's host did not start".to_owned());
        };
        let ran = Box::pin(self.serve_run(task, source, confirm)).await;
        if ran.is_err() {
            // A host that went away takes the task's `let`s with it; the next program starts
            // another.
            *link = None;
        }
        ran
    }

    async fn serve_run(
        &self,
        task: &mut TaskLink,
        source: &str,
        confirm: bool,
    ) -> Result<Value, String> {
        task.send(&json!({"run": {"source": source, "confirm": confirm}}))
            .await?;
        // Which instance answers for each app, found once per program.
        let mut providers: Providers = BTreeMap::new();
        loop {
            let message = task.next().await?;
            if let Some(answer) = message.get("answer") {
                return Ok(answer.clone());
            }
            let reply = if let Some(record) = message.get("record") {
                self.task_record(record).await
            } else if let Some(call) = message.get("invoke") {
                Box::pin(self.task_invoke(call, &mut providers)).await
            } else {
                json!({"err": "the door answers record reads and invocations"})
            };
            task.send(&reply).await?;
        }
    }

    /// Start this task's host.
    async fn start_task(&self) -> Result<TaskLink, String> {
        let hosts = self.task_hosts.clone().ok_or(
            "this door runs no programs: it is no desktop session's, which starts a task's host",
        )?;
        let task = self
            .agent_run()
            .map(|run| run.id)
            .ok_or("this door's agent works in no run, which a task's programs are")?;
        let room = self.room.clone();
        let fd = tokio::task::spawn_blocking(move || hosts.start(&room, &task))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("the task's host did not start: {error}"))?;
        TaskLink::new(fd).map_err(|error| error.to_string())
    }

    /// The running instance of the app `app` (an id), to ask.
    async fn provider_of(&self, app: &str, known: &mut Providers) -> Result<Addressed, String> {
        if let Some(found) = known.get(app) {
            return Ok(found.clone());
        }
        let (_, running) = self.instances().await?;
        let found = running
            .iter()
            .rev()
            .find(|(_, instance)| instance["app_id"].as_str() == Some(app))
            .map(|(peer, _)| Addressed {
                provider: peer.to_string(),
                app_id: Some(app.to_owned()),
            })
            .ok_or_else(|| {
                let name = app_names(&[app.to_owned()]).join("");
                format!("{name} is not open; open it with tilo_do {{path: \"{name}#open\"}}")
            })?;
        known.insert(app.to_owned(), found.clone());
        Ok(found)
    }

    /// The task's read of the record `record` (`{app, record}`): its committed content, as the
    /// broker decides the task may read it now ([`tilo_actions::remote::RecordReads`]), with no
    /// app asked: `{"ok": {"document": base64}}`, `{"absent": true}` or `{"err": why}`.
    async fn task_record(&self, record: &Value) -> Value {
        let (Some(app), Some(id)) = (record["app"].as_str(), record["record"].as_str()) else {
            return json!({"err": "a record read names its app and record"});
        };
        let Some(reads) = self.host().record_reads() else {
            return json!({"err": tilo_actions::remote::RECORD_READS_UNAVAILABLE});
        };
        let address = tilo_actions::remote::RecordAddress {
            app: app.to_owned(),
            record: id.to_owned(),
        };
        let caller = self.execution_caller();
        // The owner captures the record on this thread's behalf; it may take a while, and the
        // door's sessions share one runtime.
        let read = tokio::task::spawn_blocking(move || reads.read(&caller, &address)).await;
        match read {
            Ok(Ok(tilo_actions::remote::RecordCopy::Document(bytes))) => {
                use base64::Engine as _;
                json!({"ok": {"document": base64::engine::general_purpose::STANDARD.encode(bytes)}})
            }
            Ok(Ok(tilo_actions::remote::RecordCopy::Absent)) => json!({"absent": true}),
            Ok(Err(error)) => {
                let name = app_names(&[app.to_owned()]).join("");
                json!({"err": format!("{name}: {error}")})
            }
            Err(error) => json!({"err": format!("the record read ended: {error}")}),
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
        let provider = match self.provider_of(app, providers).await {
            Ok(provider) => provider,
            Err(why) => return json!({"failed": why}),
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
            wait_ms: None,
        }))
        .await;
        match value {
            Ok(value) => match value.pointer("/update/state").and_then(Value::as_str) {
                Some("succeeded") => json!({
                    "ok": value.pointer("/update/output").cloned().unwrap_or(Value::Null)
                }),
                // Failed, cancelled, or not finished in time: it may still run, so the
                // program cannot go on as if it had not.
                _ => json!({"failed": super::outcome(&value).text}),
            },
            Err(why) if refusal(&why) => json!({"refused": format!("{action}: {why}")}),
            Err(why) => json!({"failed": why}),
        }
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

    fn history(&self) -> String {
        history_text(&self.programs.lock_recover())
    }
}

/// The instance that answers for each app, by app id.
type Providers = BTreeMap<String, Addressed>;

/// Whether the door's error `why` is the policy's refusal, not a failure of the action.
fn refusal(why: &str) -> bool {
    [
        "may not invoke",
        "refused:",
        "requires confirmation",
        "ConfirmationRequired",
        "no such action is offered",
    ]
    .iter()
    .any(|said| why.contains(said))
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
                        history: history_of(answer),
                        program: program.lines().next().unwrap_or_default().to_owned(),
                        changed,
                        undone: false,
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

/// `history task`: the steps of `programs`' session, oldest first.
fn history_text(programs: &Programs) -> String {
    if programs.steps.is_empty() {
        return "no step of this task changed anything".to_owned();
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
            if step.undone { "  (undone)" } else { "" }
        );
    }
    text
}

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// Take back this task's last step in each of `apps` (ids), the last changed first.
    async fn undo_in(&self, history: &[History]) -> Result<(), String> {
        let mut providers: Providers = BTreeMap::new();
        for at in history.iter().rev() {
            let name = app_names(std::slice::from_ref(&at.app)).join("");
            let provider = self.provider_of(&at.app, &mut providers).await?;
            let mut arguments = Map::new();
            arguments.insert("steps".to_owned(), json!(1));
            let value = Box::pin(self.invoke_one(OneCall {
                provider,
                workspace: false,
                revision: None,
                resource: at.resource.clone(),
                action: "undo".to_owned(),
                arguments,
                invocation_id: None,
                wait_ms: None,
            }))
            .await
            .map_err(|why| format!("in {name}: {why}"))?;
            if value.pointer("/update/state").and_then(Value::as_str) != Some("succeeded") {
                return Err(format!("in {name}: {}", super::outcome(&value).text));
            }
        }
        Ok(())
    }

    /// Take back the last step that is in effect, or with `all` every step of this task, newest
    /// first.
    async fn undo_steps(&self, all: bool) -> Result<Reply, String> {
        let mut text = String::new();
        loop {
            let next = {
                let programs = self.programs.lock_recover();
                programs
                    .steps
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, step)| !step.undone)
                    .map(|(index, step)| (index, step.clone()))
            };
            let Some((index, step)) = next else {
                if text.is_empty() {
                    return Ok(Reply::ok("nothing of this task's to undo".to_owned()));
                }
                break;
            };
            if let Err(why) = self.undo_in(&step.history).await {
                let _ = write!(
                    text,
                    "{}undoing step {}: {why}",
                    if text.is_empty() { "" } else { "\n" },
                    step.number,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_name_what_answers_showed_and_nothing_else() {
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

    #[test]
    fn a_regular_expression_holds_no_references_and_no_literal() {
        let programs = Programs::default();
        let grep = r"grep(/\bt1\b|x@y/i, in: notes).count_by(.doc)";
        assert_eq!(resolve_refs(grep, &programs).unwrap(), grep);
        // After a value, a slash divides; the reference after it is still checked.
        let divided = resolve_refs("print(10 / t7)", &programs).unwrap_err();
        assert!(divided.contains("`t7`"), "{divided}");
    }

    #[test]
    fn a_preview_counts_the_actions_and_resources() {
        let answer = json!({"steps": [
            {"action": "pin", "resources": ["a", "root"], "destructive": false},
            {"action": "pin", "resources": ["b", "root"], "destructive": false},
            {"action": "delete_card", "resources": ["c"], "destructive": true},
        ]});
        assert_eq!(
            preview(&answer),
            "preview: 3 actions on 4 resources\n  pin ×2\n  delete_card ×1  destroys\nnothing changed; send the same program with confirm: true to apply it"
        );
    }

    #[test]
    fn a_step_in_two_apps_is_one_step_of_the_task() {
        let mut programs = Programs::default();
        let answer = json!({
            "state": "done",
            "output": ["@notes.Note:root-a/n1 \"Q3 plan\""],
            "changed": 2,
            "apps": ["so.tilo.lang.notes", "so.tilo.lang.todo"],
            "history": [
                {"app": "so.tilo.lang.notes", "resource": "tl:so.tilo.lang.notes"},
                {"app": "so.tilo.lang.todo", "resource": "tl:so.tilo.lang.todo"},
            ],
            "lets": [{"name": "plan", "type": "so.tilo.lang.notes.Note"}],
        });
        let (text, failed) =
            answer_text(&mut programs, "notes.pin(n1)\ntodo.add(t1, \"x\")", &answer);
        assert!(!failed, "{text}");
        assert_eq!(
            text,
            "n1 \"Q3 plan\"\nstep 1: 2 changes in notes and todo; `undo` takes it back"
        );
        assert_eq!(programs.lets, ["plan"]);
        // Undo goes to each app by its id and resource, never by a name another app may share.
        assert_eq!(
            programs.steps.first().map(|step| step.history.clone()),
            Some(vec![
                History {
                    app: "so.tilo.lang.notes".into(),
                    resource: "tl:so.tilo.lang.notes".into(),
                },
                History {
                    app: "so.tilo.lang.todo".into(),
                    resource: "tl:so.tilo.lang.todo".into(),
                },
            ])
        );
        assert_eq!(
            history_text(&programs),
            "steps of this task, oldest first\n1  notes and todo  2 changes  notes.pin(n1)"
        );
    }

    #[test]
    fn a_failed_step_says_what_became_of_its_changes() {
        let mut programs = Programs::default();
        let answer = json!({"state": "failed", "detail": "trap: no", "changed": 1, "apps": ["so.tilo.lang.todo"]});
        let (text, failed) = answer_text(&mut programs, "x", &answer);
        assert!(failed);
        assert_eq!(text, "error: trap: no\nwhat it changed is taken back");
        assert!(programs.steps.is_empty(), "a failed program is no step");
    }

    #[test]
    fn the_policys_refusals_are_told_from_failures() {
        assert!(refusal("the caller may not invoke this action"));
        assert!(refusal("refused: asks the person first"));
        assert!(refusal("no such action is offered"));
        assert!(!refusal("the action failed: no such note"));
    }
}
