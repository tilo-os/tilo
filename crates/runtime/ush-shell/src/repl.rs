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

use crate::editor::Read;
use crate::shell::{Flow, Shell};

/// One command line read at the prompt.
struct Command {
    text: String,
    /// An agent asked for it: its output takes the agent rendering.
    agent: bool,
}

impl Command {
    const fn typed(text: String) -> Self {
        Self { text, agent: false }
    }
}

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
        {
            let mut editor = crate::editor::lock_state(&self.editor);
            editor.runs_key.clone_from(&session.runs_key);
            editor.terminal = self.input(0).zip(self.output(2));
        }
        self.marks = session
            .runs_key
            .clone()
            .zip(self.output(1))
            .map(|(key, out)| std::sync::Arc::new(crate::jobs::Marks::new(key, out)));
        // Only a shell that edits its lines reads Terminal's marks on its
        // input; one reading lines as they come takes them as typed.
        let edits = self.host.edit_mode(true);
        if edits {
            self.host.edit_mode(false);
        }
        let reads = if edits { ";in=1" } else { "" };
        // Each command line is a run of its own; this names the session's.
        let session_id = format!("ush-{}-{}", self.host.pid(), self.host.now_ms());
        let mut lines: u64 = 0;
        loop {
            self.report_jobs();
            if let Some(key) = &session.runs_key {
                // `in=1`: this shell reads Terminal's marks on its input.
                self.out(&format!("\x1b]133;A;k={key}{reads}\x07"));
            }
            let Some(Command { text, agent }) = self.read_command() else {
                return self.exit_trap(self.last_status);
            };
            crate::editor::lock_state(&self.editor).ran(&text);
            let line = text.trim_end_matches('\n');
            if line.trim().is_empty() {
                continue;
            }
            let marks = self.marks.clone().filter(|_| !runs_nothing(line));
            let number = marks.as_ref().map(|marks| {
                let number = marks.next();
                marks.started(number, line, &self.cwd, None);
                number
            });
            lines = lines.saturating_add(1);
            self.set_run(Some(crate::host::Run {
                id: format!("{session_id}-{lines}"),
                label: line.to_string(),
            }));
            let render = self.render;
            if agent {
                self.render = crate::commands::Render::Agent;
            }
            let result = self.run_in_foreground(&text);
            self.render = render;
            self.set_run(None);
            self.host.clear_interrupt();
            let status = match result {
                Err(Flow::Exit(status)) => {
                    if let (Some(marks), Some(number)) = (&marks, number) {
                        marks.ended(number, status, None);
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
            if let (Some(marks), Some(number)) = (&marks, number) {
                marks.ended(number, status, None);
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
    fn read_command(&mut self) -> Option<Command> {
        let mut text = String::new();
        let mut prompt = self.vars.scalar("PS1").unwrap_or_default();
        loop {
            let shown = self.prompt(&prompt);
            let line = match self.edit_line(&shown, text.is_empty()) {
                Some(Read::Line(line)) => line,
                Some(Read::Eof) => {
                    if text.is_empty() {
                        return None;
                    }
                    return Some(Command::typed(text));
                }
                Some(Read::Cancelled) => {
                    self.last_status = 130;
                    text.clear();
                    prompt = self.vars.scalar("PS1").unwrap_or_default();
                    continue;
                }
                Some(Read::Requested { line, agent }) => {
                    // Asked for at the prompt: whatever was half typed,
                    // over however many lines, is not part of it.
                    return Some(Command {
                        text: format!("{line}\n"),
                        agent,
                    });
                }
                None => {
                    // No terminal to edit on: lines as they come.
                    if let Some(err) = self.output(2) {
                        let _ = err.write_str(&shown);
                    }
                    let Ok(line) = self.read_cooked()? else {
                        text.clear();
                        prompt = self.vars.scalar("PS1").unwrap_or_default();
                        continue;
                    };
                    line
                }
            };
            text.push_str(&line);
            text.push('\n');
            if !incomplete(&text) {
                return Some(Command::typed(text));
            }
            prompt = self.vars.scalar("PS2").unwrap_or_default();
        }
    }

    /// One line as the terminal's line discipline hands it over: `None` at
    /// the end of the input, `Err` when Ctrl-C dropped what was typed.
    fn read_cooked(&self) -> Option<Result<String, ()>> {
        let input = self.input(0)?;
        match input.read_line_until(b'\n') {
            Ok(Some(line)) => Some(Ok(String::from_utf8_lossy(&line).into_owned())),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                // Ctrl-C at the prompt drops what was typed.
                self.host.clear_interrupt();
                if let Some(err) = self.output(2) {
                    let _ = err.write_str("\n");
                }
                Some(Err(()))
            }
            Ok(None) | Err(_) => None,
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
    // A line ending in a backslash goes on on the next one.
    let line = text.strip_suffix('\n').unwrap_or(text);
    let trailing = line.chars().rev().take_while(|c| *c == '\\').count();
    if trailing % 2 == 1 {
        return true;
    }
    match Shell::parse(text) {
        Err(brush_parser::ParseError::ParsingAtEndOfInput) => true,
        Err(brush_parser::ParseError::Tokenizing { inner, .. }) => inner.is_incomplete(),
        Ok(_) | Err(brush_parser::ParseError::ParsingNear(_)) => false,
    }
}
