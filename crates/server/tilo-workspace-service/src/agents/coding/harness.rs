//! What a coding harness is to the engine, and Claude Code's own ways, in one place: how it is
//! started without a person at a terminal, how it is told Tilo's MCP server and its model, how
//! it takes a message, and what the lines it prints mean.
//!
//! Claude Code runs printing and reading one JSON object a line (its `stream-json` formats), so
//! a message reaches it on its input while it works, and what it says arrives as it says it.
//! Its model is whatever answers at `ANTHROPIC_BASE_URL` in Anthropic's Messages protocol: this
//! computer's one model endpoint, answered by the workspace's model
//! ([`crate::agents::endpoint`]). The token beside it is a word, not a credential: the program
//! refuses to start without one, and the endpoint is reached only through its run's own secret
//! path.
//!
//! It asks nobody before it edits or runs something in its working copy: nobody sits at its
//! terminal, and its person let it work there. What it does in Tilo goes through Tilo's MCP
//! server, as the sub-agent's own turn, which asks its person where an action asks.

use serde_json::{Value, json};

/// Claude Code's id in a record.
pub const CLAUDE_CODE: &str = "claude-code";

/// The most of a command or a path a line of what it is doing shows.
const DOING_LENGTH: usize = 120;

/// What a harness is started for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Start {
    /// Its own id for a conversation it had before, to take it up again; none starts a new one.
    pub session: Option<String>,
    /// How its person wants it to work, beside its own instructions.
    pub instruction: Option<String>,
}

pub use crate::agents::endpoint::Endpoints;

/// A harness's program: what is started, and what it is handed in its environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// One thing a harness said, as its host reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Says {
    /// Its own id for this conversation: kept, so a later run takes it up.
    Session(String),
    /// Words for its person.
    Words(String),
    /// What it is doing now, in a few words: a line, not a message.
    Doing(String),
    /// It finished what it was asked: `ok`, or why not in `words`. It stays ready for the next
    /// message.
    Finished { ok: bool, words: String },
}

/// Reads what one run of a harness prints, line by line, in order.
pub trait Reads: Send {
    /// What the harness said with `line` of its output; nothing for a line that is not its
    /// protocol's.
    fn line(&mut self, line: &str) -> Vec<Says>;
}

/// One coding harness's own ways.
pub trait Harness: Send + Sync {
    /// Its id in a record.
    fn id(&self) -> &'static str;
    /// Its name as a person reads it.
    fn name(&self) -> &'static str;
    /// Its program for `start`, reaching its host at `endpoints`.
    fn program(&self, start: &Start, endpoints: &Endpoints) -> Program;
    /// `text` as one message on its input: its own way of taking a message while it works or
    /// between two pieces of work.
    fn message(&self, text: &str) -> Vec<u8>;
    /// A reader for one run's output.
    fn reads(&self) -> Box<dyn Reads>;
}

/// Claude Code, started as the program `command` names: `claude` on the computer's `PATH`
/// unless told another.
#[derive(Clone, Debug)]
pub struct ClaudeCode {
    command: Vec<String>,
}

impl Default for ClaudeCode {
    fn default() -> Self {
        Self {
            command: vec!["claude".to_owned()],
        }
    }
}

impl ClaudeCode {
    /// Claude Code started as `command`, its own arguments after it.
    #[must_use]
    pub fn at(command: Vec<String>) -> Self {
        Self { command }
    }
}

impl Harness for ClaudeCode {
    fn id(&self) -> &'static str {
        CLAUDE_CODE
    }

    fn name(&self) -> &'static str {
        "Claude Code"
    }

    fn program(&self, start: &Start, endpoints: &Endpoints) -> Program {
        let servers = json!({
            "mcpServers": {"tilo": {"type": "http", "url": endpoints.tilo}},
        });
        let mut argv = self.command.clone();
        argv.extend(
            [
                "--print",
                "--verbose",
                "--output-format",
                "stream-json",
                "--input-format",
                "stream-json",
                "--dangerously-skip-permissions",
                "--strict-mcp-config",
                "--mcp-config",
            ]
            .map(str::to_owned),
        );
        argv.push(servers.to_string());
        if let Some(session) = &start.session {
            argv.extend(["--resume".to_owned(), session.clone()]);
        }
        if let Some(instruction) = start.instruction.as_ref().filter(|said| !said.is_empty()) {
            argv.extend(["--append-system-prompt".to_owned(), instruction.clone()]);
        }
        let env = [
            ("ANTHROPIC_BASE_URL", endpoints.model.clone()),
            (
                "ANTHROPIC_AUTH_TOKEN",
                "answered-by-the-workspaces-model".to_owned(),
            ),
            // It reports nothing home and fetches no update of itself: the one address it asks
            // is its model's.
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1".to_owned()),
            ("DISABLE_AUTOUPDATER", "1".to_owned()),
        ]
        .map(|(name, value)| (name.to_owned(), value))
        .to_vec();
        Program { argv, env }
    }

    fn message(&self, text: &str) -> Vec<u8> {
        let mut line = json!({
            "type": "user",
            "message": {"role": "user", "content": [{"type": "text", "text": text}]},
        })
        .to_string()
        .into_bytes();
        line.push(b'\n');
        line
    }

    fn reads(&self) -> Box<dyn Reads> {
        Box::new(Lines::default())
    }
}

/// One run's lines, read in order.
#[derive(Debug, Default)]
struct Lines {
    /// The id it last said, so it is told once.
    session: Option<String>,
}

impl Lines {
    fn session(&mut self, event: &Value, said: &mut Vec<Says>) {
        let Some(session) = event.get("session_id").and_then(Value::as_str) else {
            return;
        };
        if self.session.as_deref() != Some(session) {
            self.session = Some(session.to_owned());
            said.push(Says::Session(session.to_owned()));
        }
    }
}

/// `text` cut to what a line shows, at a character's edge, on one line.
fn short(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    let mut end = line.len().min(DOING_LENGTH);
    while !line.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    let cut = line.get(..end).unwrap_or_default();
    if cut.len() < text.trim().len() {
        format!("{cut}…")
    } else {
        cut.to_owned()
    }
}

/// What a use of one of its tools is, in a few words: the tool, and the command, file or
/// question it is used on where its input names one.
fn doing(tool: &Value) -> String {
    let name = tool.get("name").and_then(Value::as_str).unwrap_or("a tool");
    let on = [
        "command",
        "file_path",
        "path",
        "pattern",
        "url",
        "query",
        "description",
    ]
    .iter()
    .find_map(|field| {
        tool.pointer(&format!("/input/{field}"))
            .and_then(Value::as_str)
    });
    match on {
        Some(on) if !on.trim().is_empty() => format!("{name}: {}", short(on)),
        _ => name.to_owned(),
    }
}

impl Reads for Lines {
    fn line(&mut self, line: &str) -> Vec<Says> {
        // A line that is no object of its protocol is a warning it printed, or another
        // program's words: not something it said.
        let Ok(event @ Value::Object(_)) = serde_json::from_str::<Value>(line.trim()) else {
            return Vec::new();
        };
        let mut said = Vec::new();
        self.session(&event, &mut said);
        match event.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                // What a helper it started itself says is its own business; what that helper
                // does still shows as what it is doing.
                let own = event.get("parent_tool_use_id").is_none_or(Value::is_null);
                let blocks = event.pointer("/message/content").and_then(Value::as_array);
                for block in blocks.into_iter().flatten() {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") if own => {
                            let text = block
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            if !text.trim().is_empty() {
                                said.push(Says::Words(text.trim().to_owned()));
                            }
                        }
                        Some("tool_use") => said.push(Says::Doing(doing(block))),
                        _ => {}
                    }
                }
            }
            Some("result") => {
                let ok = !event
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let words = event
                    .get("result")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|words| !words.is_empty())
                    .or_else(|| event.get("subtype").and_then(Value::as_str))
                    .unwrap_or_default()
                    .to_owned();
                said.push(Says::Finished { ok, words });
            }
            _ => {}
        }
        said
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(lines: &[Value]) -> Vec<Says> {
        let mut reader = ClaudeCode::default().reads();
        lines
            .iter()
            .flat_map(|line| reader.line(&line.to_string()))
            .collect()
    }

    fn endpoints() -> Endpoints {
        Endpoints {
            model: "http://127.0.0.1:9/run/model".to_owned(),
            tilo: "http://127.0.0.1:9/run/mcp".to_owned(),
        }
    }

    tilo_test_env::scenario! {
        fn claude_codes_own_ways_are_read_and_written_in_one_place;

        phase it_is_started_to_print_and_read_lines_with_its_endpoints;
        {
            let program = ClaudeCode::default().program(&Start::default(), &endpoints());
            assert_eq!(program.argv.first().map(String::as_str), Some("claude"));
            for flag in [
                "--print",
                "--verbose",
                "--dangerously-skip-permissions",
                "--strict-mcp-config",
            ] {
                assert!(program.argv.iter().any(|argument| argument == flag), "{flag}");
            }
            let after = |flag: &str| {
                let at = program.argv.iter().position(|argument| argument == flag).unwrap();
                program.argv.get(at.saturating_add(1)).unwrap().clone()
            };
            assert_eq!(after("--output-format"), "stream-json");
            assert_eq!(after("--input-format"), "stream-json");
            let servers: Value = serde_json::from_str(&after("--mcp-config")).unwrap();
            assert_eq!(
                servers.pointer("/mcpServers/tilo"),
                Some(&json!({"type": "http", "url": "http://127.0.0.1:9/run/mcp"}))
            );
            assert!(program.env.contains(&(
                "ANTHROPIC_BASE_URL".to_owned(),
                "http://127.0.0.1:9/run/model".to_owned()
            )));
            assert!(!program.argv.iter().any(|argument| argument == "--resume"));
        }

        phase a_conversation_it_had_is_taken_up_and_its_persons_instruction_added;
        {
            let program = ClaudeCode::at(vec!["bash".into(), "stand-in".into()]).program(
                &Start {
                    session: Some("4f0e".into()),
                    instruction: Some("Work on a branch of your own.".into()),
                },
                &endpoints(),
            );
            assert_eq!(program.argv.get(..2).unwrap(), ["bash", "stand-in"]);
            let after = |flag: &str| {
                let at = program.argv.iter().position(|argument| argument == flag).unwrap();
                program.argv.get(at.saturating_add(1)).unwrap().clone()
            };
            assert_eq!(after("--resume"), "4f0e");
            assert_eq!(after("--append-system-prompt"), "Work on a branch of your own.");
        }

        phase a_message_is_one_line_of_its_input;
        {
            let line = ClaudeCode::default()
                .message("Also check the sign-in change.\nIt may be involved.");
            let (end, written) = line.split_last().unwrap();
            assert_eq!(*end, b'\n');
            assert!(!written.contains(&b'\n'), "a line break in the words travels as its escape");
            let message: Value = serde_json::from_slice(&line).unwrap();
            assert_eq!(message["type"], "user");
            assert_eq!(
                message.pointer("/message/content/0/text").and_then(Value::as_str),
                Some("Also check the sign-in change.\nIt may be involved.")
            );
        }

        phase what_it_prints_is_read_as_words_work_and_an_end;
        {
            let said = read(&[
                json!({"type": "system", "subtype": "init", "session_id": "4f0e", "tools": []}),
                json!({"type": "assistant", "session_id": "4f0e", "parent_tool_use_id": null,
                    "message": {"role": "assistant", "content": [
                        {"type": "text", "text": "Three deploys since Monday.\n"},
                        {"type": "tool_use", "id": "t1", "name": "Bash",
                            "input": {"command": "git log --since=monday\ngit status"}},
                    ]}}),
                json!({"type": "assistant", "session_id": "4f0e", "parent_tool_use_id": "t9",
                    "message": {"role": "assistant", "content": [
                        {"type": "text", "text": "a helper's own words"},
                        {"type": "tool_use", "id": "t2", "name": "Read",
                            "input": {"file_path": "src/sign_in.rs"}},
                    ]}}),
                json!({"type": "result", "subtype": "success", "is_error": false,
                    "session_id": "4f0e", "result": "The drop starts with build 4812."}),
            ]);
            assert_eq!(
                said,
                [
                    Says::Session("4f0e".into()),
                    Says::Words("Three deploys since Monday.".into()),
                    Says::Doing("Bash: git log --since=monday…".into()),
                    Says::Doing("Read: src/sign_in.rs".into()),
                    Says::Finished { ok: true, words: "The drop starts with build 4812.".into() },
                ]
            );
        }

        phase a_failure_says_why_and_what_is_no_line_of_its_own_says_nothing;
        {
            let mut reader = ClaudeCode::default().reads();
            assert!(reader.line("npm warn deprecated something").is_empty());
            assert!(reader.line("[1, 2]").is_empty());
            assert_eq!(
                reader.line(
                    &json!({"type": "result", "subtype": "error_max_turns", "is_error": true,
                        "session_id": "s2"})
                    .to_string()
                ),
                [
                    Says::Session("s2".into()),
                    Says::Finished { ok: false, words: "error_max_turns".into() },
                ]
            );
        }
    }
}
