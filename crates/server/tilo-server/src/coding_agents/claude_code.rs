//! Claude Code's own ways, and nothing of them anywhere else: how it is
//! started without a person at a terminal, how it is told Tilo's MCP server,
//! how it takes a message, and what the lines it prints mean.
//!
//! It runs printing and reading one JSON object a line (its `stream-json`
//! formats), so a message reaches it on its input while it works, and what it
//! says arrives as it says it. Its model is whatever answers at
//! `ANTHROPIC_BASE_URL` in Anthropic's Messages protocol: the run's `model`
//! endpoint. The token beside it is a word, not a credential: the program
//! refuses to start without one, the endpoint never reads it, and the
//! launcher drops it before the request leaves the machine.
//!
//! It asks nobody before it edits or runs something in its working copy:
//! nobody sits at its terminal, the machine is the workspace's own, and its
//! person let it run there. What it does in Tilo goes through Tilo's MCP
//! server, which asks its person where an action asks.

use serde_json::{Value, json};

use super::harness::{Harness, MODEL, ModelProtocol, Program, Reads, Says, Start, TILO};
use crate::workspaces::person_model::Interaction;
use ush::served_runs::wire::placeholder;

pub const ID: &str = "claude-code";

/// The most of a command or a path a line of what it is doing shows.
const DOING_LENGTH: usize = 120;

/// The version of Claude Code a machine starts, written once.
macro_rules! version {
    () => {
        "2.1.326"
    };
}

/// The one version of Claude Code this server starts in a machine: the place to change it.
/// A machine that does not hold exactly this one fetches it from npm's registry by this
/// number at its first start and keeps it in a folder of that number, so a machine that
/// once fetched another never goes on starting that. An image that ships the program puts
/// it in the same folder.
pub const VERSION: &str = version!();

/// Starts the program, installing it for the machine's own user where this version of it
/// is not there yet. What the installer prints goes to standard error, so standard output
/// stays the program's own lines.
const STARTS: &str = concat!(
    "at=\"$HOME/.local/share/claude-code/",
    version!(),
    "\"; [ -x \"$at/bin/claude\" ] \
     || npm install --global --prefix \"$at\" @anthropic-ai/claude-code@",
    version!(),
    " >&2 || exit 127; exec \"$at/bin/claude\" \"$@\""
);

#[derive(Clone, Copy, Debug, Default)]
pub struct ClaudeCode;

impl Harness for ClaudeCode {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Claude Code"
    }

    fn model_protocol(&self) -> ModelProtocol {
        ModelProtocol::AnthropicMessages
    }

    fn program(&self, start: &Start) -> Program {
        let servers = json!({
            "mcpServers": {
                "tilo": {"type": "http", "url": format!("{}/mcp", placeholder(TILO))},
            },
        });
        let mut argv: Vec<String> = [
            "sh",
            "-c",
            STARTS,
            "claude",
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
        .map(str::to_owned)
        .to_vec();
        argv.push(servers.to_string());
        if let Some(session) = &start.session {
            argv.extend(["--resume".to_owned(), session.clone()]);
        }
        if let Some(instruction) = start.instruction.as_ref().filter(|said| !said.is_empty()) {
            argv.extend(["--append-system-prompt".to_owned(), instruction.clone()]);
        }
        let env = [
            ("ANTHROPIC_BASE_URL", placeholder(MODEL)),
            (
                "ANTHROPIC_AUTH_TOKEN",
                "answered-by-the-workspaces-host".to_owned(),
            ),
            // It reports nothing home and fetches no update of itself: the
            // one address it asks is its model's.
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1".to_owned()),
            ("DISABLE_AUTOUPDATER", "1".to_owned()),
        ]
        .map(|(name, value)| (name.to_owned(), value))
        .to_vec();
        Program {
            argv,
            env,
            endpoints: vec![MODEL.to_owned(), TILO.to_owned()],
        }
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

    fn kind_of(&self, model: &str) -> Interaction {
        if model.contains("haiku") {
            Interaction::Classifier
        } else {
            Interaction::Subtask
        }
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

/// What a use of one of its tools is, in a few words: the tool, and the
/// command, file or question it is used on where its input names one.
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
        // A line that is no object of its protocol is a warning it printed,
        // or another program's words: not something it said.
        let Ok(event @ Value::Object(_)) = serde_json::from_str::<Value>(line.trim()) else {
            return Vec::new();
        };
        let mut said = Vec::new();
        self.session(&event, &mut said);
        match event.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                // What a helper it started itself says is its own business;
                // what that helper does still shows as what it is doing.
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
        let mut reader = ClaudeCode.reads();
        lines
            .iter()
            .flat_map(|line| reader.line(&line.to_string()))
            .collect()
    }

    tilo_test_env::scenario! {
        fn claude_codes_own_ways_are_read_and_written_in_one_place;

        phase it_is_started_to_print_and_read_lines_with_its_endpoints_as_placeholders;
        {
            let program = ClaudeCode.program(&Start::default());
            assert_eq!(program.endpoints, ["model", "tilo"]);
            assert_eq!(program.argv.get(..4).unwrap(), ["sh", "-c", STARTS, "claude"]);
            // The one version named here is the one fetched and the one started: nothing
            // without a number is asked of the registry.
            assert_eq!(
                STARTS,
                format!(
                    "at=\"$HOME/.local/share/claude-code/{VERSION}\"; [ -x \"$at/bin/claude\" ] \
                     || npm install --global --prefix \"$at\" \
                     @anthropic-ai/claude-code@{VERSION} >&2 || exit 127; \
                     exec \"$at/bin/claude\" \"$@\""
                )
            );
            assert!(
                VERSION.split('.').count() == 3
                    && VERSION.split('.').all(|part| part.parse::<u32>().is_ok()),
                "an exact version, never a range or a tag: {VERSION}"
            );
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
                Some(&json!({"type": "http", "url": "{endpoint:tilo}/mcp"}))
            );
            assert!(
                program
                    .env
                    .contains(&("ANTHROPIC_BASE_URL".to_owned(), "{endpoint:model}".to_owned()))
            );
            assert!(!program.argv.iter().any(|argument| argument == "--resume"));
        }

        phase a_conversation_it_had_is_taken_up_and_its_persons_instruction_added;
        {
            let program = ClaudeCode.program(&Start {
                session: Some("4f0e".into()),
                instruction: Some("Work on a branch of your own.".into()),
            });
            let after = |flag: &str| {
                let at = program.argv.iter().position(|argument| argument == flag).unwrap();
                program.argv.get(at.saturating_add(1)).unwrap().clone()
            };
            assert_eq!(after("--resume"), "4f0e");
            assert_eq!(after("--append-system-prompt"), "Work on a branch of your own.");
        }

        phase a_message_is_one_line_of_its_input;
        {
            let line = ClaudeCode.message("Also check the sign-in change.\nIt may be involved.");
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
                json!({"type": "user", "session_id": "4f0e", "message": {"role": "user",
                    "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "…"}]}}),
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
            let mut reader = ClaudeCode.reads();
            assert!(reader.line("npm warn deprecated something").is_empty());
            assert!(reader.line("[1, 2]").is_empty());
            assert!(reader.line("").is_empty());
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

        phase its_small_model_is_a_quick_judgment_and_any_other_a_part_of_a_task;
        {
            assert_eq!(ClaudeCode.kind_of("claude-haiku-4-5"), Interaction::Classifier);
            assert_eq!(ClaudeCode.kind_of("claude-opus-5"), Interaction::Subtask);
            assert_eq!(ClaudeCode.kind_of(""), Interaction::Subtask);
        }
    }
}
