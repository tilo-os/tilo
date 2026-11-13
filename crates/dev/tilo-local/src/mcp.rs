//! An MCP agent at a headless session's agent door, as `cargo headless --check` proves one works:
//! `tilo-runtime-mcp --connect`, the program an agent runs, presenting the session's agent key, is
//! admitted without anyone answering the door, lists what the workspace offers and runs an action.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long one answer may take.
const ANSWER: Duration = Duration::from_secs(60);

/// How long the session may take to have its person's root certify the key's grant after it first
/// answers: the door refuses the key, saying so, until it has.
const CERTIFIED: Duration = Duration::from_secs(60);

/// Where the agent connects from and with what.
pub struct Agent<'a> {
    /// The session's programs.
    pub bin: &'a Path,
    /// The session's runtime directory, its door under it.
    pub runtime: &'a Path,
    /// The file holding the session's agent key.
    pub key: &'a Path,
    /// A process in the session's sandbox, whose namespaces the agent joins (the door serves only
    /// programs in its own); `None` when the session runs in this process's.
    pub inside: Option<u32>,
}

/// One running agent program and its answers, line by line.
struct Connection {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
    next: u64,
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Connection {
    /// Send `method` with `params` (JSON text) and wait for its answer, a line of JSON.
    fn ask(&mut self, method: &str, params: &str) -> Result<String, String> {
        self.next = self.next.saturating_add(1);
        let id = self.next;
        let line =
            format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#);
        self.send(&line)?;
        let began = Instant::now();
        let id_field = format!(r#""id":{id}"#);
        loop {
            let left = ANSWER.saturating_sub(began.elapsed());
            let answer = self.lines.recv_timeout(left).map_err(|_| {
                format!(
                    "the agent door did not answer {method} within {}s",
                    ANSWER.as_secs()
                )
            })?;
            if answer.contains(&id_field) {
                return Ok(answer);
            }
        }
    }

    fn send(&mut self, line: &str) -> Result<(), String> {
        writeln!(self.stdin, "{line}")
            .and_then(|()| self.stdin.flush())
            .map_err(|error| format!("the agent program stopped reading: {error}"))
    }
}

impl Agent<'_> {
    /// Start the agent's program, connected to the session's door.
    fn connect(&self) -> Result<Connection, String> {
        let program = self.bin.join("tilo-runtime-mcp");
        let mut command = match self.inside {
            Some(pid) => {
                let mut command = Command::new("nsenter");
                command
                    .args(["--target", &pid.to_string()])
                    .args(["--user", "--mount", "--preserve-credentials", "--"])
                    .arg(&program);
                command
            }
            None => Command::new(&program),
        };
        let mut child = command
            .arg("--connect")
            .env(
                "TILO_AGENT_SOCK",
                self.runtime.join("tilo-session/agent.sock"),
            )
            .env("TILO_AGENT_KEY", self.key)
            .env("XDG_RUNTIME_DIR", self.runtime)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("could not start {}: {error}", program.display()))?;
        let stdin = child.stdin.take().ok_or("the agent program has no stdin")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("the agent program has no stdout")?;
        let (send, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    eprintln!("  tilo-runtime-mcp: {line}");
                }
            });
        }
        Ok(Connection {
            child,
            stdin,
            lines,
            next: 0,
        })
    }

    /// Connect and be admitted: the door answers `initialize` once it let the agent in, and with
    /// why not otherwise. A key whose grant the session has not certified yet is tried again.
    fn admitted(&self) -> Result<(Connection, String), String> {
        let began = Instant::now();
        loop {
            let mut connection = self.connect()?;
            let answer = connection.ask(
                "initialize",
                r#"{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"cargo headless check","version":"1"}}"#,
            )?;
            if answer.contains(r#""result""#) {
                connection.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)?;
                return Ok((connection, answer));
            }
            if answer.contains("not certified") && began.elapsed() < CERTIFIED {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
            return Err(format!("the agent door did not admit the agent: {answer}"));
        }
    }

    /// Admitted with the key, list the tools and the apps, and run an action; what it saw.
    pub fn prove(&self) -> Result<Vec<String>, String> {
        let began = Instant::now();
        let (mut connection, _) = self.admitted()?;
        let mut said = vec![format!(
            "an MCP agent presenting the session's agent key was admitted without a click ({:.1}s)",
            began.elapsed().as_secs_f64()
        )];
        let tools = connection.ask("tools/list", "{}")?;
        for tool in ["tilo_ls", "tilo_do"] {
            if !tools.contains(&format!(r#""{tool}""#)) {
                return Err(format!("the agent's tools have no {tool}: {tools}"));
            }
        }
        let listed = call(&mut connection, "tilo_ls", "{}")?;
        let apps: Vec<&str> = listed
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .collect();
        said.push(format!("it lists the apps (tilo_ls): {}", apps.join(" ")));
        let action = action_to_run(&apps)?;
        let ran = call(
            &mut connection,
            "tilo_do",
            &format!(r#"{{"path":"{action}"}}"#),
        )?;
        said.push(format!(
            "it runs an action (tilo_do {action}): {}",
            ran.lines().next().unwrap_or_default()
        ));
        Ok(said)
    }
}

/// The action the check runs: opening an app the session installed, which every installed app
/// offers an agent.
fn action_to_run(apps: &[&str]) -> Result<String, String> {
    apps.iter()
        .find(|app| **app == "notes")
        .or_else(|| apps.iter().find(|app| **app != "session"))
        .map(|app| format!("{app}#open"))
        .ok_or_else(|| format!("the agent sees no app to open: {}", apps.join(" ")))
}

/// Call `tool` with `arguments` (JSON text); its text, or why it failed.
fn call(connection: &mut Connection, tool: &str, arguments: &str) -> Result<String, String> {
    let answer = connection.ask(
        "tools/call",
        &format!(r#"{{"name":"{tool}","arguments":{arguments}}}"#),
    )?;
    if !answer.contains(r#""result""#) || answer.contains(r#""isError":true"#) {
        return Err(format!("{tool} {arguments} failed: {answer}"));
    }
    Ok(text_of(&answer))
}

/// The first text content of a `tools/call` answer, unescaped.
fn text_of(answer: &str) -> String {
    let Some((_, after)) = answer.split_once(r#""text":"#) else {
        return answer.to_owned();
    };
    let mut text = String::new();
    let mut chars = after.trim_start().strip_prefix('"').unwrap_or("").chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => text.push('\n'),
                Some('t') => text.push('\t'),
                Some(other) => text.push(other),
                None => break,
            },
            other => text.push(other),
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{action_to_run, text_of};

    #[test]
    fn a_tool_answers_text_is_read() {
        let answer = r#"{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"notes  Notes\nsession \"x\""}]}}"#;
        assert_eq!(text_of(answer), "notes  Notes\nsession \"x\"");
    }

    #[test]
    fn the_check_opens_notes_or_another_app() {
        assert_eq!(
            action_to_run(&["session", "notes"]).as_deref(),
            Ok("notes#open")
        );
        assert_eq!(
            action_to_run(&["session", "chat"]).as_deref(),
            Ok("chat#open")
        );
        assert!(action_to_run(&["session"]).is_err());
    }
}
