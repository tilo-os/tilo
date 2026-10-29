//! The interactive shell: prompt, read a command (over as many lines as it
//! takes), run it, and mark the run for Terminal.
//!
//! The marks are the ones `docs/app-model/ush.md` ("Command runs")
//! describes, so Terminal records a run the same way whichever shell ran it:
//!
//! ```text
//! ESC ] 133 ; A ; k=<key> BEL                                   the prompt is shown
//! ESC ] 133 ; C ; k=<key> ; cmd=<b64> ; cwd=<b64> ; t=<n> BEL   a command line starts
//! ESC ] 133 ; D ; <status> ; k=<key> ; t=<n> BEL                it ended with <status>
//! ```
//!
//! `t` counts the command lines that ran something, so a line that runs
//! nothing (an empty one, `exit`) marks no run.

use base64::Engine as _;

use crate::shell::{Flow, Shell};

/// What the interactive loop needs beyond the shell.
#[derive(Debug, Clone, Default)]
pub struct Session {
    /// The key Terminal marks this session's runs with, if it asked for
    /// marks.
    pub runs_key: Option<String>,
}

impl Shell {
    /// Run the shell interactively on its standard streams until the input
    /// ends or `exit`. Answers the final status.
    pub fn interactive(&mut self, session: &Session) -> i32 {
        self.interactive = true;
        let mut runs: u64 = 0;
        // Each command line is a run of its own; this names the session's.
        let session_id = format!("ush-{}-{}", self.host.pid(), self.host.now_ms());
        let mut lines: u64 = 0;
        loop {
            if let Some(key) = &session.runs_key {
                self.out(&format!("\x1b]133;A;k={key}\x07"));
            }
            let Some(text) = self.read_command() else {
                return self.exit_trap(self.last_status);
            };
            let line = text.trim_end_matches('\n');
            if line.trim().is_empty() {
                continue;
            }
            let marks = session.runs_key.as_ref().filter(|_| !runs_nothing(line));
            if let Some(key) = marks {
                runs = runs.saturating_add(1);
                let b64 = base64::engine::general_purpose::STANDARD;
                self.out(&format!(
                    "\x1b]133;C;k={key};cmd={};cwd={};t={runs}\x07",
                    b64.encode(line),
                    b64.encode(self.cwd.to_string_lossy().as_bytes()),
                ));
            }
            lines = lines.saturating_add(1);
            self.set_run(Some(crate::host::Run {
                id: format!("{session_id}-{lines}"),
                label: line.to_string(),
            }));
            let result = self.eval_text(&text);
            self.set_run(None);
            self.host.clear_interrupt();
            let status = match result {
                Err(Flow::Exit(status)) => {
                    if let Some(key) = marks {
                        self.out(&format!("\x1b]133;D;{status};k={key};t={runs}\x07"));
                    }
                    return self.exit_trap(status);
                }
                Ok(status)
                | Err(Flow::Fatal(status) | Flow::Return(status) | Flow::Error(status)) => status,
                Err(Flow::Break(_) | Flow::Continue(_)) => self.last_status,
            };
            if status == 130 {
                self.run_trap("INT");
            }
            self.last_status = status;
            if let Some(key) = marks {
                self.out(&format!("\x1b]133;D;{status};k={key};t={runs}\x07"));
            }
        }
    }

    fn run_trap(&mut self, signal: &str) {
        if let Some(action) = self.traps.get(signal).cloned() {
            let _ = self.eval_text(&action);
        }
    }

    /// Prompt and read one complete command, asking for more lines while
    /// it is unfinished. `None` at the end of the input.
    fn read_command(&mut self) -> Option<String> {
        let mut text = String::new();
        let mut prompt = self.vars.scalar("PS1").unwrap_or_default();
        loop {
            let shown = self.prompt(&prompt);
            if let Some(err) = self.output(2) {
                let _ = err.write_str(&shown);
            }
            let input = self.input(0)?;
            match input.read_line_until(b'\n') {
                Ok(Some(line)) => {
                    text.push_str(&String::from_utf8_lossy(&line));
                    text.push('\n');
                }
                Ok(None) => {
                    if text.is_empty() {
                        return None;
                    }
                    return Some(text);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                    // Ctrl-C at the prompt drops what was typed.
                    self.host.clear_interrupt();
                    if let Some(err) = self.output(2) {
                        let _ = err.write_str("\n");
                    }
                    text.clear();
                    prompt = self.vars.scalar("PS1").unwrap_or_default();
                    continue;
                }
                Err(_) => return None,
            }
            if !incomplete(&text) {
                return Some(text);
            }
            prompt = self.vars.scalar("PS2").unwrap_or_default();
        }
    }

    /// A prompt string with its backslash escapes and expansions done.
    pub(crate) fn prompt(&mut self, template: &str) -> String {
        let mut out = String::new();
        let mut chars = template.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('u') => out.push_str(&self.vars.scalar("USER").unwrap_or_default()),
                Some('h') => {
                    let host = self.host.hostname();
                    out.push_str(host.split('.').next().unwrap_or(&host));
                }
                Some('H') => out.push_str(&self.host.hostname()),
                Some('w') => out.push_str(&self.tilde_cwd()),
                Some('W') => {
                    let w = self.tilde_cwd();
                    out.push_str(if w == "/" {
                        "/"
                    } else {
                        w.rsplit('/').next().unwrap_or(&w)
                    });
                }
                Some('$') => out.push('$'),
                Some('n') => out.push('\n'),
                Some('s') => out.push_str("ush"),
                Some('e') => out.push('\x1b'),
                Some('a') => out.push('\x07'),
                Some('[' | ']') => {}
                Some('\\') | None => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
            }
        }
        if out.contains(['$', '`']) {
            let saved = self.last_status;
            let expanded = self.expand_string(&out).unwrap_or(out);
            self.last_status = saved;
            return expanded;
        }
        out
    }

    fn tilde_cwd(&self) -> String {
        let cwd = self.cwd.to_string_lossy().into_owned();
        match self.vars.scalar("HOME") {
            Some(home) if !home.is_empty() && home != "/" => match cwd.strip_prefix(&home) {
                Some("") => "~".into(),
                Some(rest) if rest.starts_with('/') => format!("~{rest}"),
                _ => cwd,
            },
            _ => cwd,
        }
    }
}

/// A line that runs nothing Terminal records: `exit`.
fn runs_nothing(line: &str) -> bool {
    let first = line.split_whitespace().next().unwrap_or("");
    first == "exit"
}

/// Whether `text` is an unfinished command that more lines complete: an
/// open quote, `if` without `fi`, a here document without its end.
pub fn incomplete(text: &str) -> bool {
    match Shell::parse(text) {
        Err(brush_parser::ParseError::ParsingAtEndOfInput) => true,
        Err(brush_parser::ParseError::Tokenizing { inner, .. }) => inner.is_incomplete(),
        Ok(_) | Err(brush_parser::ParseError::ParsingNear(_)) => false,
    }
}
