//! The `tilo` tool: an agent's task writes tilolang programs (`docs/designs/app-language/agents.md`).
//!
//! A program runs in the tilolang host of the app it names, as this task's code: the host's
//! `run-program` (`crates/platform/tilolang-tilo/src/effects.rs`), invoked through the same
//! path, gate and attribution as every other action of the door. Each call is one step, committed
//! whole or not at all, and one that would change many resources or destroy something answers
//! with a preview until it is sent again with `confirm`. The program's `let`s stay in the host's
//! session of this task for its next program.
//!
//! What goes back is tilolang text. Every resource it shows is named by a short reference (`n3`,
//! `t91`) at the start of its line, which later programs of this session take; a reference the
//! session never showed is refused, so none can be invented. `undo` takes back the last step,
//! `undo task` every step of this task, and `history task` lists them.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use tilo_lock::MutexRecover as _;

use super::{ActionHost, Done, LsArgs, McpServer, Reply, app_names, outcome};

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
    app: String,
    /// The program's first line, as `history task` names the step.
    program: String,
    changed: u64,
    undone: bool,
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
    /// The app the last program ran in: where one that names none runs.
    current: Option<String>,
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
        /// A dot follows it: an app or a value whose member is read.
        dotted: bool,
        /// It follows `@`: the app of a resource literal.
        literal: bool,
    },
    Other(String),
}

/// `source` in pieces, which joined give it back.
fn pieces(source: &str) -> Vec<Piece> {
    let mut found: Vec<Piece> = Vec::new();
    let mut chars = source.chars().peekable();
    let mut previous: Option<char> = None;
    while let Some(character) = chars.next() {
        if character.is_alphabetic() || character == '_' {
            let mut text = String::from(character);
            while let Some(next) = chars.next_if(|c| c.is_alphanumeric() || *c == '_') {
                text.push(next);
            }
            let member = matches!(previous, Some('.' | '@' | ':'));
            let literal = previous == Some('@');
            previous = text.chars().last();
            found.push(Piece::Word {
                text,
                member,
                dotted: chars.peek() == Some(&'.'),
                literal,
            });
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
                ..
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

/// The apps a program names (`notes` in `notes.notes`, or in a resource literal), among
/// `apps`, in order.
fn apps_named(source: &str, apps: &[String]) -> Vec<String> {
    let mut named: Vec<String> = Vec::new();
    for piece in pieces(source) {
        if let Piece::Word {
            text,
            member,
            dotted: true,
            literal,
        } = piece
            && (literal || !member)
            && apps.contains(&text)
            && !named.contains(&text)
        {
            named.push(text);
        }
    }
    named
}

/// `source` without the text of its strings.
fn outside_strings(source: &str) -> String {
    let mut out = String::new();
    let mut in_string = false;
    let mut escaped = false;
    for character in source.chars() {
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
            _ => {}
        }
        let running = self.tilolang_names().await?;
        let (source, current) = {
            let programs = self.programs.lock_recover();
            (resolve_refs(program, &programs)?, programs.current.clone())
        };
        let app = {
            let named = apps_named(&source, &running);
            match named.as_slice() {
                [] => current.ok_or_else(|| {
                    format!(
                        "name the app the program uses (`notes.notes`); this task may use {}",
                        running.join(", ")
                    )
                })?,
                [one] => one.clone(),
                several => {
                    return Err(format!(
                        "a program runs in one app for now; this one uses {}: send one program per app",
                        several.join(" and ")
                    ));
                }
            }
        };
        let mut arguments = Map::new();
        arguments.insert("source".to_owned(), json!(source));
        arguments.insert(
            "session".to_owned(),
            json!(self.agent_run().map(|run| run.id).unwrap_or_default()),
        );
        arguments.insert("confirm".to_owned(), json!(args.confirm));
        let path = format!("{app}/tilolang#run-program");
        let value = match Box::pin(self.do_one_value(&path, arguments, None, None)).await? {
            Done::Launched(_) => {
                return Err(format!(
                    "{app} was not running; it is open now, send the program again"
                ));
            }
            Done::Invoked { value, .. } => value,
        };
        if value.pointer("/update/state").and_then(Value::as_str) != Some("succeeded") {
            let failed = outcome(&value);
            if failed.text.contains("no action run-program") {
                return Err(format!("{app} runs no tilolang programs"));
            }
            return Ok(failed);
        }
        let answer = value
            .pointer("/update/output/value")
            .cloned()
            .unwrap_or(Value::Null);
        Ok(self.answered(&app, program, &answer))
    }

    /// The text of a program's answer, its resources named by references.
    fn answered(&self, app: &str, program: &str, answer: &Value) -> Reply {
        let (text, failed) = answer_text(&mut self.programs.lock_recover(), app, program, answer);
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

/// The text of a program's answer in `programs`' session, and whether it is an error.
fn answer_text(
    programs: &mut Programs,
    app: &str,
    program: &str,
    answer: &Value,
) -> (String, bool) {
    {
        programs.current = Some(app.to_owned());
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
                    programs.steps.push(Step {
                        number,
                        app: app.to_owned(),
                        program: program.lines().next().unwrap_or_default().to_owned(),
                        changed,
                        undone: false,
                    });
                    lines.push(format!(
                        "step {}: {changed} {} in {app}; `undo` takes it back",
                        programs.steps.len(),
                        if changed == 1 { "change" } else { "changes" }
                    ));
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
                lines.push(format!(
                    "error: {}; nothing it did is kept",
                    answer["detail"].as_str().unwrap_or("the program failed")
                ));
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
            step.app,
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
    /// The running apps the task may run programs in, by the names programs call them.
    async fn tilolang_names(&self) -> Result<Vec<String>, String> {
        let (_, running) = self.instances().await?;
        let mut ids: Vec<String> = running
            .iter()
            .filter_map(|(_, app)| app["app_id"].as_str().map(str::to_owned))
            .collect();
        ids.sort();
        ids.dedup();
        Ok(app_names(&ids))
    }

    /// The room resource of `app`, whose `undo` takes back this task's steps there.
    async fn room_of(&self, app: &str) -> Result<String, String> {
        let listed = Box::pin(self.agent_ls(LsArgs {
            path: Some(app.to_owned()),
            ..LsArgs::default()
        }))
        .await?;
        listed
            .text
            .lines()
            .find(|line| line.trim_end().ends_with("tilolang.room"))
            .and_then(|line| line.split_whitespace().next())
            .map(|room| room.trim_end_matches('/').to_owned())
            .ok_or_else(|| format!("{app} offers no undo"))
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
            let room = self.room_of(&step.app).await?;
            let mut arguments = Map::new();
            arguments.insert("steps".to_owned(), json!(1));
            let reply =
                Box::pin(self.do_one(&format!("{}/{room}#undo", step.app), arguments, None, None))
                    .await?;
            if reply.failed {
                let _ = write!(
                    text,
                    "{}undoing step {}: {}",
                    if text.is_empty() { "" } else { "\n" },
                    step.number,
                    reply.text
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
                step.app,
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
}
