//! `tilolang repl`: tilolang typed against one of the person's workspaces, as them.
//!
//! The REPL reaches the workspace through the `tilo` command, run as its own program
//! ([`tilo`]): `tilo mcp --as-me` serves Tilo's tools over its standard input and output, as the
//! person, wherever `tilo do` reaches (this login's desktop, this computer's daemon with the key
//! `tilo login` made of it, or the key file `--key-file` names), or in a nested Tilo session
//! running on this computer (`--session`), whose API socket it is pointed at. What the REPL
//! changes is the person's, attributed to them, in history and undo. Without `tilo` installed,
//! the REPL says what to install.
//! Each entry is a `tilo` program run in one program session the REPL carries from entry to
//! entry: `let`s kept between entries, references (`n3`) naming what answers showed, one step
//! per entry with a preview before a bulk or destructive change, and `undo`. `ls` and `find` are
//! the workspace's listing and search. The REPL adds line editing, history, completion and
//! colour.

mod editor;
mod render;
pub mod style;
pub mod tilo;

use std::ffi::OsString;
use std::path::PathBuf;

use serde_json::{Value as Json, json};

use editor::{Complete, Editor, Read};
use style::{Role, paint};

#[derive(Clone, Debug, Default)]
pub struct ReplOptions {
    /// The workspace, by name or id (`--workspace`).
    pub workspace: Option<String>,
    /// A key file to act as (`--key-file`), instead of this computer's login.
    pub key_file: Option<PathBuf>,
    /// A nested session running on this computer to work in (`--session`): the one it names, or
    /// with `None` the only one running.
    pub session: Option<Option<PathBuf>>,
    /// Colour: `None` for when the output is a terminal.
    pub color: Option<bool>,
}

/// What a person reads when nothing on this computer signs them in.
pub const NOT_SIGNED_IN: &str = "You're not signed in to Tilo on this computer. Sign in with \
     `tilo login`, then start `tilolang repl` again.";

/// What the REPL knows of the session's code, for completion: every app's declarations by their
/// names, and the fields of each type.
#[derive(Default)]
struct Catalogue {
    apps: Vec<String>,
    /// `notes.pin`, `notes.Note`, ...
    members: Vec<String>,
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

const METHODS: &[&str] = &[
    "filter", "map", "sort_by", "count", "first", "last", "len", "any", "all", "find", "matching",
];

/// One workspace as the REPL works in it: the calls it makes there, and what completion offers.
pub struct Repl {
    /// The REPL's own `tilo`: its program session is no other REPL's.
    tilo: tilo::Tilo,
    catalogue: Catalogue,
    names: Vec<String>,
}

/// What one entry did: lines to print, or the end of the REPL.
#[derive(Debug, PartialEq, Eq)]
pub enum Said {
    Lines(Vec<String>),
    Quit,
}

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Nothing,
    Command(String),
    /// `ls [path]`: what the workspace holds, or what a path holds.
    List(String),
    /// `find words`: what the workspace holds that matches.
    Find(String),
    Program(String),
}

fn parse_entry(text: &str) -> Entry {
    let text = text.trim();
    if text.is_empty() {
        return Entry::Nothing;
    }
    if let Some(command) = text.strip_prefix(':') {
        let name = command.split_whitespace().next().unwrap_or_default();
        return Entry::Command(name.to_owned());
    }
    // `ls` and `find text` are no tilolang (`find(…)` is, and stays a program).
    if text == "ls" {
        return Entry::List(String::new());
    }
    if let Some(path) = text.strip_prefix("ls ") {
        return Entry::List(path.trim().to_owned());
    }
    if let Some(query) = text.strip_prefix("find ")
        && !query.trim_start().starts_with('(')
    {
        return Entry::Find(query.trim().to_owned());
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

fn error_line(error: &str) -> Vec<String> {
    vec![format!("{} {error}", paint(Role::Error, "error:"))]
}

impl Repl {
    /// The REPL in the workspace `tilo` acts in, in a program session of its own: it learns the
    /// apps the workspace runs, for completion.
    ///
    /// # Errors
    /// The workspace does not answer, or does not list its apps.
    pub fn open(tilo: tilo::Tilo) -> Result<Self, String> {
        let mut repl = Self {
            tilo,
            catalogue: Catalogue::default(),
            names: Vec::new(),
        };
        // Every installed app, from the packages the workspace runs programs against: whether a
        // window of it is open does not matter.
        let listed = repl.program("catalogue")?;
        let catalogue: Json = serde_json::from_str(text_of(&listed).trim())
            .map_err(|_| "the workspace did not list its apps".to_owned())?;
        for (alias, app) in catalogue.as_object().into_iter().flatten() {
            repl.catalogue.learn(alias, &app["signatures"]);
            repl.catalogue.apps.push(alias.clone());
        }
        Ok(repl)
    }

    /// The apps the workspace's programs may name.
    #[must_use]
    pub fn apps(&self) -> &[String] {
        &self.catalogue.apps
    }

    /// Answer the entry `text`.
    pub fn entry(&mut self, text: &str) -> Said {
        Said::Lines(match parse_entry(text) {
            Entry::Nothing => Vec::new(),
            Entry::Command(name) => match name.as_str() {
                "q" | "quit" | "exit" => return Said::Quit,
                "h" | "help" => HELP.lines().map(str::to_owned).collect(),
                "apps" => vec![self.catalogue.apps.join("  ")],
                other => vec![paint(
                    Role::Error,
                    &format!("no command :{other}; :help lists them"),
                )],
            },
            Entry::List(path) => self.list(&path),
            Entry::Find(query) => self.find(&query),
            Entry::Program(program) => self.run(&program),
        })
    }

    /// Run `source` in the REPL's program session: the answer.
    fn program(&mut self, source: &str) -> Result<Json, String> {
        self.tilo.call("tilo", json!({"program": source}))
    }

    /// Send `program` and say its answer.
    fn run(&mut self, program: &str) -> Vec<String> {
        let answer = match self.program(program) {
            Ok(answer) => answer,
            Err(error) => {
                // A session the workspace no longer keeps is gone with its lets: the names it
                // completed go with it.
                if error.contains("program session ended") {
                    self.names.clear();
                }
                return error_line(&error);
            }
        };
        let status = answer.get("status").and_then(Json::as_str).unwrap_or("");
        let failed = status == "failed";
        let mut said = Vec::new();
        for line in lines_of(&answer) {
            if let Some(shown) = render::line_ref(&line)
                && !self.names.iter().any(|name| name == shown)
            {
                self.names.push(shown.to_owned());
            }
            said.push(render::api_line(&line, failed));
        }
        if !failed {
            for name in program.lines().filter_map(bound_name) {
                if !self.names.iter().any(|known| known == name) {
                    self.names.push(name.to_owned());
                }
            }
        }
        said
    }

    /// `ls [path]`: the workspace's listing, as its host words it.
    fn list(&mut self, path: &str) -> Vec<String> {
        let arguments = if path.is_empty() {
            json!({})
        } else {
            json!({"path": path})
        };
        match self.tilo.call("tilo_ls", arguments) {
            Ok(listed) => text_of(&listed).lines().map(str::to_owned).collect(),
            Err(error) => error_line(&error),
        }
    }

    /// `find words`: what the workspace holds that matches, best first.
    fn find(&mut self, query: &str) -> Vec<String> {
        match self.tilo.call("tilo_find", json!({"query": query})) {
            Ok(found) => {
                let hits: Vec<String> = found
                    .pointer("/result/hits")
                    .and_then(Json::as_array)
                    .into_iter()
                    .flatten()
                    .map(|hit| {
                        let field = |name: &str| hit.get(name).and_then(Json::as_str).unwrap_or("");
                        format!("{}  {}", field("path"), field("label"))
                    })
                    .collect();
                if hits.is_empty() {
                    vec![paint(Role::Note, &format!("nothing matches {query:?}"))]
                } else {
                    hits
                }
            }
            Err(error) => error_line(&error),
        }
    }
}

/// The words of an answer: what a person reads.
fn text_of(answer: &Json) -> &str {
    answer.get("text").and_then(Json::as_str).unwrap_or("")
}

/// The lines an entry shows: the program's own as the model reads them — a value's resources by
/// the references a later entry names them by — then the answer's own lines (what it did), which
/// a person reads. An answer with no lines of its own is its text, a note or a refusal.
fn lines_of(answer: &Json) -> Vec<String> {
    let lines = |key: &str| -> Vec<String> {
        answer
            .get(key)
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(Json::as_str)
            .flat_map(str::lines)
            .map(str::to_owned)
            .collect()
    };
    let output = lines("output");
    if output.is_empty() {
        return text_of(answer).lines().map(str::to_owned).collect();
    }
    let person = lines("person_output");
    let mut said = output;
    if !person.is_empty() {
        // The answer's text is the person's lines of the program first, then its own.
        said.extend(
            text_of(answer)
                .lines()
                .skip(person.len())
                .map(str::to_owned),
        );
    }
    said
}

const HELP: &str = "\
tilolang, run in the workspace as you:
  notes.notebook.notes         a value: printed, each resource by a reference (n3)
  notes.pin(n3)                an action of the app, applied as you
  let pinned = notes.notebook.notes.filter(.pinned)
                               kept for the entries after it
  for n in pinned { notes.pin(n) }
                               one step: applied whole or not at all
  undo                         take back the last step
  undo task / history task     take back / list every step of this REPL
  ls [path]                    list the apps, or what a path holds
  find words                   find what the workspace holds
  :apps  :help  :quit";

impl Complete for Repl {
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
                .chain(["let", "undo", "true", "false", "none"].map(str::to_owned))
            {
                offer(name);
            }
        }
        candidates.sort();
        (start, candidates)
    }
}

/// [`NOT_SIGNED_IN`], and how to work in a nested session when one is running here.
fn not_signed_in() -> String {
    if crate::session::Running::all().is_empty() {
        NOT_SIGNED_IN.to_owned()
    } else {
        format!(
            "{NOT_SIGNED_IN} A Tilo session you started is running here: \
             `tilolang repl --session` works in it."
        )
    }
}

/// The nested session `named` names, else the only one running, else the one the person picks.
fn attach(
    editor: &Editor,
    named: Option<&std::path::Path>,
) -> Result<crate::session::Running, String> {
    if let Some(named) = named {
        return crate::session::Running::named(named);
    }
    let mut all = crate::session::Running::all();
    match all.len() {
        0 => Err(
            "No nested Tilo session is open on this computer: start one with \
                  dist/dev/nested-session.sh."
                .to_owned(),
        ),
        1 => Ok(all.remove(0)),
        _ => {
            let states: Vec<String> = all
                .iter()
                .map(|running| running.state.display().to_string())
                .collect();
            let index = pick(editor, "Which session?", &states)?;
            Ok(all.swap_remove(index))
        }
    }
}

/// The workspace the REPL works in when `tilo` acts in none of its own: the person's one
/// workspace, or the one they pick: its id and name.
fn workspace(tilo: &mut tilo::Tilo, editor: &Editor) -> Result<(String, String), String> {
    let listed = tilo.call(
        "tilo_do",
        json!({"path": "account/workspaces#list", "args": {}}),
    )?;
    let mut all: Vec<(String, String)> = listed
        .pointer("/result/workspaces")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let id = row.get("id")?.as_str()?.to_owned();
            let name = row
                .get("name")
                .and_then(Json::as_str)
                .unwrap_or(&id)
                .to_owned();
            Some((id, name))
        })
        .collect();
    match all.len() {
        0 => Err(
            "You have no Tilo workspace yet: make one in Tilo, then start the REPL again."
                .to_owned(),
        ),
        1 => Ok(all.remove(0)),
        _ => {
            let names: Vec<String> = all.iter().map(|(_, name)| name.clone()).collect();
            Ok(all.swap_remove(pick(editor, "Which workspace?", &names)?))
        }
    }
}

/// Ask the person `question`, answered with one of `names`: its index.
fn pick(editor: &Editor, question: &str, names: &[String]) -> Result<usize, String> {
    println!("{question}");
    for (number, name) in names.iter().enumerate() {
        println!("  {}  {name}", number.saturating_add(1));
    }
    loop {
        let line = match editor.read("number: ", &NoCompletion) {
            Ok(Read::Line(line)) => line,
            Ok(Read::Interrupted | Read::Eof) => return Err("nothing picked".to_owned()),
            Err(error) => return Err(error.to_string()),
        };
        let picked = line
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|number| number.checked_sub(1))
            .filter(|index| *index < names.len());
        if let Some(picked) = picked {
            return Ok(picked);
        }
        println!("a number from 1 to {}", names.len());
    }
}

struct NoCompletion;

impl Complete for NoCompletion {
    fn complete(&self, _line: &str, cursor: usize) -> (usize, Vec<String>) {
        (cursor, Vec::new())
    }
}

pub fn repl(options: &ReplOptions) -> Result<(), String> {
    let mut editor = Editor::new(Some(crate::state_dir().join("repl-history")));
    style::set(options.color.unwrap_or_else(|| editor.is_terminal()));
    // `tilo`'s own flags, then its server; a nested session is named by its API socket.
    let mut args: Vec<OsString> = Vec::new();
    let mut socket = None;
    if let Some(named) = &options.session {
        socket = Some(attach(&editor, named.as_deref())?.socket);
    } else if let Some(key_file) = &options.key_file {
        args.extend(["--key-file".into(), key_file.into()]);
    }
    if let Some(workspace) = &options.workspace {
        args.extend(["--workspace".into(), workspace.into()]);
    }
    args.extend(["mcp".into(), "--as-me".into()]);
    let program = tilo::find().ok_or_else(|| tilo::TILO_MISSING.to_owned())?;
    let envs: Vec<(&str, &std::path::Path)> = socket
        .as_deref()
        .map(|socket| ("TILO_API_SOCKET", socket))
        .into_iter()
        .collect();
    let (mut tilo, served) =
        tilo::Tilo::start(&program, &args, &envs).map_err(|refused| match refused {
            tilo::Refused::NotSignedIn => not_signed_in(),
            tilo::Refused::Said(said) => said,
        })?;
    let name = if let Some((_, name)) = served {
        name
    } else {
        let (workspace, name) = workspace(&mut tilo, &editor)?;
        tilo.workspace = Some(workspace);
        name
    };
    let mut repl = Repl::open(tilo)?;
    println!(
        "{}  {}",
        paint(Role::Title, "tilolang"),
        paint(
            Role::Note,
            &format!("in {name}; apps: {}  (:help)", repl.apps().join(", "))
        )
    );
    let mut pending = String::new();
    loop {
        let prompt = if pending.is_empty() {
            paint(Role::Prompt, "tl> ")
        } else {
            "... ".to_owned()
        };
        let line = match editor.read(&prompt, &repl) {
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
        match repl.entry(&entry) {
            Said::Quit => return Ok(()),
            Said::Lines(lines) => {
                for line in lines {
                    println!("{line}");
                }
            }
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

    tilo_test_env::scenario! {
        fn an_entry_reads_as_what_the_person_meant;

        phase an_entry_is_a_command_a_listing_or_a_program;
        {
            assert_eq!(parse_entry("  "), Entry::Nothing);
            assert_eq!(parse_entry(":apps now"), Entry::Command("apps".into()));
            assert_eq!(parse_entry("ls"), Entry::List(String::new()));
            assert_eq!(parse_entry("ls notes/"), Entry::List("notes/".into()));
            assert_eq!(parse_entry("find groceries"), Entry::Find("groceries".into()));
            assert_eq!(
                parse_entry("find (\"x\")"),
                Entry::Program("find (\"x\")".into())
            );
            assert_eq!(
                parse_entry(" undo task "),
                Entry::Program("undo task".into())
            );
        }

        phase a_let_line_names_what_it_binds;
        {
            assert_eq!(
                bound_name("let pinned: List<notes.Note> = notes.notebook.notes.filter(.pinned)"),
                Some("pinned")
            );
            assert_eq!(bound_name("a == b"), None);
        }

        phase an_entry_continues_while_a_bracket_is_open;
        {
            assert_eq!(open_brackets("notes.notebook.notes.filter(n -> {"), 2);
            assert_eq!(open_brackets("\"(\""), 0);
        }

        phase a_types_fields_are_read_from_its_signature;
        {
            assert_eq!(
                fields_of("type Note {\n    title: Text\n    pinned: Bool = false\n}"),
                vec!["title", "pinned"]
            );
        }
    }
}
