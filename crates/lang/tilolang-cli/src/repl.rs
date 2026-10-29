//! `tilolang repl`: tilolang typed against a running Tilo session, through its agent door.
//!
//! The REPL is an agent of the session like any other (`docs/runtime-mcp.md`): the person
//! admits it, it reaches what they grant it, and what it changes is its run's in history and
//! undo. Each entry is a program for the door's `tilo` tool (`docs/designs/app-language/agents.md`),
//! with the same session semantics an agent has: `let`s kept between entries, references (`n3`)
//! naming what answers showed, one step per entry with a preview before a bulk or destructive
//! change, and `undo`. The REPL adds line editing, history, completion and colour.

mod door;
mod editor;
mod render;
pub mod style;

use std::path::{Path, PathBuf};

use serde_json::{Value as Json, json};

use door::{Control, Door};
use editor::{Complete, Editor, Read};
use style::{Role, paint};

/// Where the REPL's session is, and how it looks.
#[derive(Clone, Debug, Default)]
pub struct ReplOptions {
    /// A running nested session: its --ready file, runtime directory or control socket.
    pub session: Option<PathBuf>,
    /// The agent door's socket itself (another session's, the person's real one).
    pub door: Option<PathBuf>,
    /// The person's own session: its door at the well-known path.
    pub real: bool,
    /// Colour: `None` for when the output is a terminal.
    pub color: Option<bool>,
}

/// What the REPL knows of the session's code, for completion: every app's declarations by
/// their names, and the fields of each type.
#[derive(Default)]
struct Catalogue {
    /// The apps programs may name.
    apps: Vec<String>,
    /// `notes.pin`, `notes.Note`, ...
    members: Vec<String>,
    /// Every field name of every type.
    fields: Vec<String>,
}

impl Catalogue {
    fn learn(&mut self, alias: &str, answer: &Json) {
        for entry in answer.as_array().into_iter().flatten() {
            let (Some(name), Some(signature)) =
                (entry["name"].as_str(), entry["signature"].as_str())
            else {
                continue;
            };
            let member = format!("{alias}.{name}");
            if !self.members.contains(&member) {
                self.members.push(member);
            }
            if signature.trim_start().starts_with("type ") {
                for field in fields_of(signature) {
                    if !self.fields.contains(&field) {
                        self.fields.push(field);
                    }
                }
            }
        }
    }
}

/// The field names of a type's signature (`type Note { title: Text, pinned: Bool = false }`).
fn fields_of(signature: &str) -> Vec<String> {
    let Some((_, body)) = signature.split_once('{') else {
        return Vec::new();
    };
    body.split([',', '\n', '}'])
        .filter_map(|part| {
            let (name, _) = part.split_once(':')?;
            let name = name.trim();
            (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_'))
                .then(|| name.to_owned())
        })
        .collect()
}

/// The methods a value's completion offers beside its fields.
const METHODS: &[&str] = &[
    "filter", "map", "sort_by", "count", "first", "last", "len", "any", "all", "find", "matching",
];

/// The REPL's state between entries.
struct Session {
    door: Door,
    catalogue: Catalogue,
    /// The names entries bound, and the references answers showed: what completes a word.
    names: Vec<String>,
    /// The program the last answer was a preview of: what `confirm` sends again.
    previewed: Option<String>,
}

/// What an entry asks for.
#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Nothing,
    Command(String, String),
    /// Apply the program the last preview was of.
    Confirm,
    Program(String),
}

fn parse_entry(text: &str) -> Entry {
    let text = text.trim();
    if text.is_empty() {
        return Entry::Nothing;
    }
    if let Some(command) = text.strip_prefix(':') {
        let (name, rest) = command
            .split_once(char::is_whitespace)
            .unwrap_or((command, ""));
        return Entry::Command(name.to_owned(), rest.trim().to_owned());
    }
    if text == "confirm" {
        return Entry::Confirm;
    }
    Entry::Program(text.to_owned())
}

/// The name a `let` line binds.
fn bound_name(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("let ")?;
    let (head, _) = rest.split_once('=')?;
    let name = head.split(':').next()?.trim();
    (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')).then_some(name)
}

impl Session {
    fn say(line: &str) {
        println!("{line}");
    }

    /// Learn the session's apps and their declarations, for completion.
    fn survey(&mut self) -> Result<(), String> {
        // Every installed app, from the packages the task's host holds: whether a window of it
        // is open does not matter.
        let listed = self.door.call("tilo", &json!({"program": "catalogue"}))?;
        if listed.failed {
            return Err(format!("the door did not list its apps: {}", listed.text));
        }
        let catalogue: Json = serde_json::from_str(listed.text.trim())
            .map_err(|error| format!("the door's catalogue is not JSON: {error}"))?;
        for (alias, app) in catalogue.as_object().into_iter().flatten() {
            self.catalogue.learn(alias, &app["signatures"]);
            self.catalogue.apps.push(alias.clone());
        }
        Ok(())
    }

    /// Send `program` to the door's `tilo` tool and print its answer.
    fn run(&mut self, program: &str, confirm: bool) {
        let answer = match self
            .door
            .call("tilo", &json!({"program": program, "confirm": confirm}))
        {
            Ok(answer) => answer,
            Err(error) => {
                Self::say(&format!("{} {error}", paint(Role::Error, "error:")));
                return;
            }
        };
        self.previewed = answer
            .text
            .lines()
            .any(|line| line.starts_with("preview:"))
            .then(|| program.to_owned());
        for line in answer.text.lines() {
            if let Some(shown) = render::line_ref(line)
                && !self.names.iter().any(|name| name == shown)
            {
                self.names.push(shown.to_owned());
            }
            Self::say(&render::door_line(line, answer.failed));
        }
        if !answer.failed {
            for name in program.lines().filter_map(bound_name) {
                if !self.names.iter().any(|known| known == name) {
                    self.names.push(name.to_owned());
                }
            }
        }
    }

    fn command(&self, name: &str) -> bool {
        match name {
            "q" | "quit" | "exit" => return false,
            "h" | "help" => Self::say(HELP),
            "apps" => Self::say(&self.catalogue.apps.join("  ")),
            other => Self::say(&paint(
                Role::Error,
                &format!("no command :{other}; :help lists them"),
            )),
        }
        true
    }
}

const HELP: &str = "\
tilolang, run by the session as this REPL's task:
  notes.notes                  a value: printed, each resource by a reference (n3)
  notes.pin(n3)                an action of the app, applied as the REPL's agent
  let pinned = notes.notes.filter(.pinned)
                               kept for the entries after it
  for n in pinned { notes.pin(n) }
                               one step: applied whole or not at all; one that changes many
                               resources or destroys something shows a preview first
  confirm                      apply what the last preview listed
  undo                         take back the last step
  undo task / history task     take back / list every step of this session
  :apps  :help  :quit";

impl Complete for Session {
    fn complete(&self, line: &str, cursor: usize) -> (usize, Vec<String>) {
        let before: Vec<char> = line.chars().take(cursor).collect();
        let typed = before
            .iter()
            .rev()
            .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.'))
            .count();
        let start = before.len().saturating_sub(typed);
        let word: String = before.get(start..).unwrap_or_default().iter().collect();
        let mut candidates: Vec<String> = Vec::new();
        let mut offer = |candidate: String| {
            if candidate.starts_with(&word) && !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        };
        if let Some((head, _)) = word.rsplit_once('.') {
            // A member of an app, or a field or method of a value.
            for member in &self.catalogue.members {
                offer(member.clone());
            }
            for field in self
                .catalogue
                .fields
                .iter()
                .map(String::as_str)
                .chain(METHODS.iter().copied())
            {
                offer(format!("{head}.{field}"));
            }
        } else {
            for name in self
                .catalogue
                .apps
                .iter()
                .chain(&self.names)
                .cloned()
                .chain(["let", "undo", "confirm", "true", "false", "none"].map(str::to_owned))
            {
                offer(name);
            }
        }
        candidates.sort();
        (start, candidates)
    }
}

/// The door and control socket of the session `options` names.
fn locate(options: &ReplOptions) -> Result<(PathBuf, Option<PathBuf>), String> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    if let Some(door) = &options.door {
        return Ok((door.clone(), None));
    }
    if options.real {
        let door = runtime
            .map(|runtime| runtime.join("tilo-session").join("agent.sock"))
            .ok_or("XDG_RUNTIME_DIR is unset: no session door to reach")?;
        return Ok((door, None));
    }
    let nested = match &options.session {
        Some(named) => nested_runtime(named)?,
        None => newest_nested(runtime.as_deref())?,
    };
    Ok((
        nested.join("tilo-session").join("agent.sock"),
        Some(nested.join("control.sock")),
    ))
}

/// The runtime directory of the nested session `named` names: its --ready file, its runtime
/// directory or its control socket.
fn nested_runtime(named: &Path) -> Result<PathBuf, String> {
    if named.is_dir() {
        return Ok(named.to_path_buf());
    }
    if named
        .extension()
        .is_some_and(|extension| extension == "sock")
    {
        return named
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "a control socket has a directory".to_owned());
    }
    let text =
        std::fs::read_to_string(named).map_err(|error| format!("{}: {error}", named.display()))?;
    let ready: Json =
        serde_json::from_str(&text).map_err(|error| format!("{}: {error}", named.display()))?;
    ready
        .get("runtime")
        .and_then(Json::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{}: names no runtime directory", named.display()))
}

/// The nested session started last whose door is up.
fn newest_nested(runtime: Option<&Path>) -> Result<PathBuf, String> {
    let runtime = runtime.ok_or("XDG_RUNTIME_DIR is unset")?;
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(runtime)
        .map_err(|error| format!("{}: {error}", runtime.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("tilo-nested-"))
                && path.join("tilo-session/agent.sock").exists()
        })
        .filter_map(|path| Some((path.metadata().ok()?.modified().ok()?, path)))
        .collect();
    found.sort();
    found.pop().map(|(_, path)| path).ok_or_else(|| {
        "no nested session with an agent door is running; start one (dist/dev/nested-session.sh), \
         or name one with --session, --door PATH or --real"
            .to_owned()
    })
}

/// Run the REPL until its input ends.
///
/// # Errors
/// The session's door is not there, did not admit the REPL, or runs no programs.
pub fn repl(options: &ReplOptions) -> Result<(), String> {
    let mut editor = Editor::new(Some(crate::state_dir().join("repl-history")));
    style::set(options.color.unwrap_or_else(|| editor.is_terminal()));
    let (socket, control) = locate(options)?;
    let control = control.filter(|path| path.exists()).map(Control::new);
    let door = Door::open(&socket, control.as_ref(), &|line| eprintln!("{line}"))?;
    if !door.serves("tilo") {
        return Err(format!(
            "the session's door at {} has no `tilo` tool, so it runs no tilolang programs (it \
             serves {}); the session runs a Tilo from before programs on the door",
            socket.display(),
            if door.tools.is_empty() {
                "nothing".to_owned()
            } else {
                door.tools.join(", ")
            }
        ));
    }
    let mut session = Session {
        door,
        catalogue: Catalogue::default(),
        names: Vec::new(),
        previewed: None,
    };
    session.survey()?;
    println!(
        "{}  {}",
        paint(Role::Title, "tilolang"),
        paint(
            Role::Note,
            &format!(
                "on {}; apps: {}  (:help)",
                socket.display(),
                session.catalogue.apps.join(", ")
            )
        )
    );
    let mut pending = String::new();
    loop {
        let prompt = if pending.is_empty() {
            paint(Role::Prompt, "tl> ")
        } else {
            "... ".to_owned()
        };
        let line = match editor.read(&prompt, &session) {
            Ok(Read::Line(line)) => line,
            Ok(Read::Interrupted) => {
                pending.clear();
                continue;
            }
            Ok(Read::Eof) => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        if !pending.is_empty() {
            pending.push('\n');
        }
        pending.push_str(&line);
        if open_brackets(&pending) > 0 {
            continue;
        }
        let entry = std::mem::take(&mut pending);
        editor.remember(&entry);
        match parse_entry(&entry) {
            Entry::Nothing => {}
            Entry::Command(name, _) => {
                if !session.command(&name) {
                    return Ok(());
                }
            }
            Entry::Confirm => match session.previewed.take() {
                Some(program) => session.run(&program, true),
                None => Session::say(&paint(Role::Note, "no preview waits for confirm")),
            },
            Entry::Program(program) => session.run(&program, false),
        }
    }
}

/// How many brackets `text` leaves open, outside strings.
fn open_brackets(text: &str) -> i64 {
    let mut depth: i64 = 0;
    let mut in_string = false;
    let mut escaped = false;
    for character in text.chars() {
        if in_string {
            match character {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '(' | '[' | '{' => depth = depth.saturating_add(1),
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    depth
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_is_a_command_a_confirm_or_a_program() {
        assert_eq!(parse_entry("  "), Entry::Nothing);
        assert_eq!(
            parse_entry(":apps now"),
            Entry::Command("apps".into(), "now".into())
        );
        assert_eq!(parse_entry("confirm"), Entry::Confirm);
        assert_eq!(
            parse_entry(" undo task "),
            Entry::Program("undo task".into())
        );
    }

    #[test]
    fn a_let_line_names_what_it_binds() {
        assert_eq!(
            bound_name("let pinned: List<notes.Note> = notes.notes.filter(.pinned)"),
            Some("pinned")
        );
        assert_eq!(bound_name("a == b"), None);
    }

    #[test]
    fn an_entry_continues_while_a_bracket_is_open() {
        assert_eq!(open_brackets("notes.notes.filter(n -> {"), 2);
        assert_eq!(open_brackets("\"(\""), 0);
    }

    #[test]
    fn a_types_fields_are_read_from_its_signature() {
        assert_eq!(
            fields_of("type Note {\n    title: Text\n    pinned: Bool = false\n}"),
            vec!["title", "pinned"]
        );
    }
}
