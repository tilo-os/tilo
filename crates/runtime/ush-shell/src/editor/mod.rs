//! The line editor: the keys a person types at the prompt, with readline's
//! emacs bindings, recall of the person's history, and Tab completion.
//!
//! It reads keys from the shell's standard input once the host has put its
//! terminal into taking them one at a time ([`Host::edit_mode`]); a host
//! with no terminal answers `false`, and lines are read as they come.
//! Nothing here forks, execs or touches a descriptor: the same editor runs
//! on a device, on a workspace's host, and in a browser.
//!
//! # The person's history
//!
//! The history is the person's own record (`terminal.history`), which
//! follows them across sessions and devices; the shell keeps no file of its
//! own. A host that holds the record answers [`Host::history`] itself. In a
//! guest the record is Terminal's, so the editor asks Terminal on the
//! session's terminal, with the key Terminal marks the session's runs with:
//!
//! ```text
//! shell → ESC ] 133 ; H ; k=<key> ; s=<skip> ; n=<limit> ; q=<b64> BEL
//! input ← ESC ] 133 ; H ; k=<key> ; s=<skip> ; q=<b64> ; e=<b64>,<b64>… [; end=1] BEL
//! ```
//!
//! The answer names the newest command lines past the `skip` newest that
//! contain `q`, newest first; `end` says there are no older ones. Keys typed
//! meanwhile keep their order. The lines this shell ran come first, so a
//! line is recalled before Terminal has recorded its run.
//!
//! # A run asked for
//!
//! A command line `run` asked for arrives as a mark of its own when the
//! shell said at its prompt that it reads them (`in=1` on the prompt mark):
//!
//! ```text
//! input ← ESC ] 133 ; R ; k=<key> ; cmd=<b64> [; agent=1] BEL
//! ```
//!
//! It replaces whatever was typed, shows as if typed, and runs; `agent=1`
//! says an agent asked, and its output takes the agent rendering.

pub(crate) mod complete;
pub(crate) mod keys;
pub(crate) mod line;
pub(crate) mod render;

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use base64::Engine as _;

use crate::host::HistoryQuery;
use crate::io::{Input, Output};
use crate::shell::Shell;
use keys::{Decoder, Key};
use line::{KillRing, Line, Word};
use render::Drawn;

/// How many command lines one history answer holds at most. Terminal may
/// send fewer: an answer is kept small enough to pass the terminal's input
/// buffer in one piece.
pub const HISTORY_PAGE: usize = 64;

/// What reading a line came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Read {
    /// The person pressed Enter on this text.
    Line(String),
    /// `Ctrl+D` on an empty line, or the input ended.
    Eof,
    /// `Ctrl+C`: what was typed is dropped.
    Cancelled,
    /// A command line `run` asked for, and whether an agent asked.
    Requested { line: String, agent: bool },
}

/// The editor's state across lines: what lives as long as the session.
#[derive(Default)]
pub(crate) struct State {
    /// The key of Terminal's marks, when Terminal asked for them.
    pub(crate) runs_key: Option<String>,
    /// The session's terminal: where keys come from and the editor draws,
    /// whatever a command's own streams are.
    pub(crate) terminal: Option<(Input, Output)>,
    /// Bytes read from the terminal that are not this editor's yet: typed
    /// while the shell waited for an answer from Terminal.
    typeahead: VecDeque<u8>,
    decoder: Decoder,
    kills: KillRing,
    history: History,
}

#[derive(Default)]
struct History {
    /// The lines this shell ran, newest first.
    local: Vec<String>,
    /// The person's history as answered so far, newest first.
    remote: Vec<String>,
    remote_done: bool,
    /// What the last search looked for, and what was answered for it.
    search: String,
    search_remote: Vec<String>,
    search_done: bool,
    /// The question asked of Terminal and not answered yet.
    pending: Option<Query>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Query {
    skip: usize,
    matching: String,
}

impl History {
    /// Every line to recall, newest first: this shell's own, then the
    /// person's others.
    fn entries(&self) -> Vec<String> {
        merged(&self.local, &self.remote, "")
    }

    fn found(&self, text: &str) -> Vec<String> {
        let remote = if self.search == text {
            &self.search_remote[..]
        } else {
            &[]
        };
        merged(&self.local, remote, text)
    }

    /// Take an answer to `query`.
    fn answered(&mut self, query: &Query, entries: Vec<String>, done: bool) {
        if query.matching.is_empty() {
            if query.skip == self.remote.len() {
                self.remote.extend(entries);
                self.remote_done = done;
            }
        } else if query.matching == self.search && query.skip == self.search_remote.len() {
            self.search_remote.extend(entries);
            self.search_done = done;
        }
    }
}

fn merged(local: &[String], remote: &[String], text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    local
        .iter()
        .chain(remote)
        .filter(|line| line.contains(text))
        .filter(|line| seen.insert(line.as_str()))
        .cloned()
        .collect()
}

pub(crate) fn lock_state(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl State {
    /// A line the shell ran: recalled first from now on.
    pub(crate) fn ran(&mut self, text: &str) {
        let text = text.trim_end_matches('\n');
        if text.trim().is_empty() || self.history.local.first().is_some_and(|l| l == text) {
            return;
        }
        self.history.local.insert(0, text.to_owned());
    }

    /// A new prompt: the person's history is asked afresh, since runs may
    /// have been recorded since.
    fn fresh(&mut self) {
        let history = &mut self.history;
        history.remote.clear();
        history.remote_done = false;
        history.search.clear();
        history.search_remote.clear();
        history.search_done = false;
    }
}

/// The parts of a mark of Terminal's: its kind and its fields.
struct Mark<'a> {
    kind: &'a str,
    fields: Vec<(&'a str, &'a str)>,
}

impl<'a> Mark<'a> {
    fn parse(body: &'a str, key: &str) -> Option<Self> {
        let mut parts = body.split(';');
        let kind = parts.next()?;
        let fields: Vec<(&str, &str)> = parts.filter_map(|p| p.split_once('=')).collect();
        let mark = Self { kind, fields };
        (mark.get("k") == Some(key)).then_some(mark)
    }

    fn get(&self, name: &str) -> Option<&'a str> {
        self.fields
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| *v)
    }
}

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

fn unb64(text: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text)
        .ok()?;
    String::from_utf8(bytes).ok()
}

/// What an incoming mark asked for.
enum Incoming {
    History {
        query: Query,
        entries: Vec<String>,
        done: bool,
    },
    Run {
        line: String,
        agent: bool,
    },
}

fn incoming(body: &str, key: &str) -> Option<Incoming> {
    let mark = Mark::parse(body, key)?;
    match mark.kind {
        "H" => Some(Incoming::History {
            query: Query {
                skip: mark.get("s")?.parse().ok()?,
                matching: unb64(mark.get("q").unwrap_or(""))?,
            },
            entries: mark
                .get("e")
                .unwrap_or("")
                .split(',')
                .filter(|e| !e.is_empty())
                .filter_map(unb64)
                .collect(),
            done: mark.get("end") == Some("1"),
        }),
        "R" => Some(Incoming::Run {
            line: unb64(mark.get("cmd")?)?,
            agent: mark.get("agent") == Some("1"),
        }),
        _ => None,
    }
}

/// What the last key did, for the edits that join their predecessor (kills
/// in a row are one kill; typing in a row is one undo).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Last {
    Other,
    Typed,
    Killed,
    Yanked,
    Tab,
}

/// What a recall waits for an answer to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Waiting {
    /// Up was pressed this many times past the lines answered so far.
    Older(usize),
    Search,
}

/// Reverse incremental search (`Ctrl+R`).
struct Search {
    text: String,
    /// Which of the lines found is shown.
    index: usize,
    failed: bool,
    /// The line as it was when the search started, for `Ctrl+G`.
    original: Line,
}

/// One line being edited.
struct Edit {
    prompt: String,
    line: Line,
    drawn: Drawn,
    undo: Vec<Line>,
    last: Last,
    /// What the key before this one did.
    previous: Last,
    /// Which history entry the line shows, if one.
    position: Option<usize>,
    /// The line being typed before recall replaced it.
    saved: Option<Line>,
    search: Option<Search>,
    waiting: Option<Waiting>,
    /// Enter was pressed while an answer was awaited: the line is accepted
    /// once it arrives, so the answer is never read by a command.
    accept_when_answered: bool,
    /// What was typed after that Enter, kept for whatever reads next.
    after: Vec<u8>,
}

impl Edit {
    fn shown_prompt(&self) -> String {
        match &self.search {
            Some(search) => format!(
                "({}reverse-i-search)'{}': ",
                if search.failed { "failed " } else { "" },
                search.text
            ),
            None => self.prompt.clone(),
        }
    }

    /// Keep the line as it is before an edit, for `Ctrl+_`: characters
    /// typed in a row are one step.
    fn checkpoint(&mut self, typing: bool) {
        if !(typing && self.previous == Last::Typed) {
            self.undo.push(self.line.clone());
        }
    }
}

impl Shell {
    /// The editor's state, shared with the shell's copies.
    pub(crate) fn editor(&self) -> Arc<Mutex<State>> {
        Arc::clone(&self.editor)
    }

    /// Read one line at `prompt` with the editor, or `None` when the host
    /// has no terminal to edit on (lines are then read as they come).
    pub(crate) fn edit_line(&self, prompt: &str, first: bool) -> Option<Read> {
        let input = self.input(0)?;
        let screen = self.output(2)?;
        if !self.host.edit_mode(true) {
            return None;
        }
        let shared = self.editor();
        let mut state = lock_state(&shared);
        if first {
            state.fresh();
        }
        let _ = screen.write_str("\x1b[?2004h");
        let mut edit = Edit {
            prompt: prompt.to_owned(),
            line: Line::default(),
            drawn: Drawn::default(),
            undo: Vec::new(),
            last: Last::Other,
            previous: Last::Other,
            position: None,
            saved: None,
            search: None,
            waiting: None,
            accept_when_answered: false,
            after: Vec::new(),
        };
        self.redraw(&screen, &mut edit);
        let read = self.edit_keys(&mut state, &input, &screen, &mut edit);
        drop(state);
        let _ = screen.write_str("\x1b[?2004l");
        self.host.edit_mode(false);
        Some(read)
    }

    fn redraw(&self, screen: &Output, edit: &mut Edit) {
        let (out, drawn) = render::draw(
            edit.drawn,
            &edit.shown_prompt(),
            &edit.line.text(),
            edit.line.cursor(),
            self.host.columns(),
        );
        edit.drawn = drawn;
        let _ = screen.write_str(&out);
    }

    /// Move below what is drawn, leaving it on the screen.
    fn leave(&self, screen: &Output, edit: &Edit) {
        let out = render::finish(
            edit.drawn,
            &edit.shown_prompt(),
            &edit.line.text(),
            self.host.columns(),
        );
        let _ = screen.write_str(&out);
    }

    /// The next key, and the bytes it came as.
    fn next_key(state: &mut State, input: &Input) -> Option<(Key, Vec<u8>)> {
        let mut bytes = Vec::new();
        loop {
            let byte = if let Some(byte) = state.typeahead.pop_front() {
                byte
            } else {
                let mut byte = [0_u8; 1];
                match input.read(&mut byte) {
                    Ok(0) => return None,
                    Ok(_) => byte[0],
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => return None,
                }
            };
            bytes.push(byte);
            if let Some(key) = state.decoder.push(byte) {
                return Some((key, bytes));
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one match over the keys the editor binds reads best whole"
    )]
    fn edit_keys(
        &self,
        state: &mut State,
        input: &Input,
        screen: &Output,
        edit: &mut Edit,
    ) -> Read {
        loop {
            let Some((key, bytes)) = Self::next_key(state, input) else {
                if edit.line.is_empty() {
                    self.leave(screen, edit);
                    return Read::Eof;
                }
                self.leave(screen, edit);
                return Read::Line(edit.line.text());
            };
            if let Key::Mark(body) = &key {
                let Some(key) = state.runs_key.clone() else {
                    continue;
                };
                match incoming(body, &key) {
                    Some(Incoming::History {
                        query,
                        entries,
                        done,
                    }) => {
                        if state.history.pending.as_ref() == Some(&query) {
                            state.history.pending = None;
                        }
                        state.history.answered(&query, entries, done);
                        match edit.waiting.take() {
                            Some(Waiting::Older(times)) => {
                                for left in (1..=times).rev() {
                                    self.recall(state, edit, true);
                                    if edit.waiting.is_some() {
                                        edit.waiting = Some(Waiting::Older(left));
                                        break;
                                    }
                                }
                            }
                            Some(Waiting::Search) => self.search_step(state, edit, false),
                            None => {}
                        }
                        if edit.accept_when_answered && state.history.pending.is_none() {
                            self.redraw(screen, edit);
                            self.leave(screen, edit);
                            // What was typed after Enter is the next
                            // reader's, in the order it was typed.
                            for byte in std::mem::take(&mut edit.after).into_iter().rev() {
                                state.typeahead.push_front(byte);
                            }
                            return Read::Line(edit.line.text());
                        }
                        self.redraw(screen, edit);
                    }
                    Some(Incoming::Run { line, agent }) => {
                        edit.search = None;
                        edit.line = Line::new(&line);
                        self.redraw(screen, edit);
                        self.leave(screen, edit);
                        return Read::Requested { line, agent };
                    }
                    None => {}
                }
                continue;
            }
            if edit.accept_when_answered {
                // Typed after Enter: it belongs to whatever reads next.
                edit.after.extend(bytes);
                continue;
            }
            if edit.search.is_some() {
                match self.search_key(state, edit, &key) {
                    SearchKey::Handled => {
                        self.redraw(screen, edit);
                        continue;
                    }
                    SearchKey::Leave => {}
                }
            }
            let last = edit.last;
            edit.previous = last;
            edit.last = Last::Other;
            match key {
                Key::Char(c) => {
                    edit.checkpoint(true);
                    edit.line.insert(c.encode_utf8(&mut [0; 4]));
                    edit.last = Last::Typed;
                }
                Key::Paste(text) => {
                    edit.checkpoint(false);
                    edit.line.insert(&text);
                }
                Key::Enter => {
                    if state.history.pending.is_some() {
                        edit.accept_when_answered = true;
                        continue;
                    }
                    self.redraw(screen, edit);
                    self.leave(screen, edit);
                    return Read::Line(edit.line.text());
                }
                Key::Ctrl('c') => {
                    let _ = screen.write_str("^C");
                    self.leave(screen, edit);
                    return Read::Cancelled;
                }
                Key::Ctrl('d') => {
                    if edit.line.is_empty() {
                        self.leave(screen, edit);
                        return Read::Eof;
                    }
                    edit.checkpoint(false);
                    edit.line.delete_forward();
                }
                Key::Delete => {
                    edit.checkpoint(false);
                    edit.line.delete_forward();
                }
                Key::Backspace | Key::Ctrl('h') => {
                    edit.checkpoint(false);
                    edit.line.delete_back();
                }
                Key::Ctrl('a') | Key::Home => edit.line.home(),
                Key::Ctrl('e') | Key::End => edit.line.end(),
                Key::Ctrl('b') | Key::Left => edit.line.left(),
                Key::Ctrl('f') | Key::Right => edit.line.right(),
                Key::Alt('b') | Key::WordLeft => edit.line.word_left(),
                Key::Alt('f') | Key::WordRight => edit.line.word_right(),
                Key::Ctrl('k') => {
                    edit.checkpoint(false);
                    let taken = edit.line.kill_to_end();
                    state.kills.kill(taken, last == Last::Killed, true);
                    edit.last = Last::Killed;
                }
                Key::Ctrl('u') => {
                    edit.checkpoint(false);
                    let taken = edit.line.kill_to_start();
                    state.kills.kill(taken, last == Last::Killed, false);
                    edit.last = Last::Killed;
                }
                Key::Ctrl('w') => {
                    edit.checkpoint(false);
                    let taken = edit.line.kill_word_back(Word::Whitespace);
                    state.kills.kill(taken, last == Last::Killed, false);
                    edit.last = Last::Killed;
                }
                Key::AltBackspace => {
                    edit.checkpoint(false);
                    let taken = edit.line.kill_word_back(Word::Alnum);
                    state.kills.kill(taken, last == Last::Killed, false);
                    edit.last = Last::Killed;
                }
                Key::Alt('d') => {
                    edit.checkpoint(false);
                    let taken = edit.line.kill_word_forward();
                    state.kills.kill(taken, last == Last::Killed, true);
                    edit.last = Last::Killed;
                }
                Key::Ctrl('y') => {
                    if let Some(text) = state.kills.yank().map(str::to_owned) {
                        edit.checkpoint(false);
                        edit.line.insert(&text);
                        edit.last = Last::Yanked;
                    }
                }
                Key::Alt('y') if last == Last::Yanked => {
                    let before = state.kills.yank_len();
                    if let Some(text) = state.kills.rotate().map(str::to_owned) {
                        let start = edit.line.cursor().saturating_sub(before);
                        edit.line.replace_back_to(start, &text);
                        edit.last = Last::Yanked;
                    }
                }
                Key::Ctrl('t') => {
                    edit.checkpoint(false);
                    edit.line.transpose();
                }
                Key::Alt('u') => {
                    edit.checkpoint(false);
                    edit.line.case_word(str::to_uppercase);
                }
                Key::Alt('l') => {
                    edit.checkpoint(false);
                    edit.line.case_word(str::to_lowercase);
                }
                Key::Alt('c') => {
                    edit.checkpoint(false);
                    edit.line.case_word(capitalized);
                }
                Key::Ctrl('_') => {
                    if let Some(previous) = edit.undo.pop() {
                        edit.line = previous;
                    } else {
                        let _ = screen.write_str("\x07");
                    }
                }
                Key::Ctrl('l') => {
                    let _ = screen.write_str("\x1b[H\x1b[2J");
                    edit.drawn = Drawn::default();
                }
                Key::Up | Key::Ctrl('p') => match edit.waiting {
                    // Still waiting for older lines: one more to go back.
                    Some(Waiting::Older(times)) => {
                        edit.waiting = Some(Waiting::Older(times.saturating_add(1)));
                    }
                    _ => self.recall(state, edit, true),
                },
                Key::Down | Key::Ctrl('n') => self.recall(state, edit, false),
                Key::Alt('<') => {
                    while edit.position.is_some() {
                        self.recall(state, edit, false);
                    }
                }
                Key::Ctrl('r') => {
                    edit.search = Some(Search {
                        text: String::new(),
                        index: 0,
                        failed: false,
                        original: edit.line.clone(),
                    });
                }
                Key::Tab => {
                    self.tab(screen, edit, last == Last::Tab);
                    edit.last = Last::Tab;
                }
                _ => {
                    let _ = screen.write_str("\x07");
                }
            }
            self.redraw(screen, edit);
        }
    }

    /// Up (`older`) or Down: the next line of the person's history, asking
    /// for more of it when the lines answered so far run out.
    fn recall(&self, state: &mut State, edit: &mut Edit, older: bool) {
        let entries = state.history.entries();
        let next = match (edit.position, older) {
            (None, false) => return,
            (None, true) => 0,
            (Some(at), true) => at.saturating_add(1),
            (Some(0), false) => {
                edit.position = None;
                edit.line = edit.saved.take().unwrap_or_default();
                return;
            }
            (Some(at), false) => at.saturating_sub(1),
        };
        if let Some(entry) = entries.get(next) {
            if edit.position.is_none() {
                edit.saved = Some(edit.line.clone());
            }
            edit.position = Some(next);
            edit.line = Line::new(entry);
            return;
        }
        if !state.history.remote_done {
            let query = Query {
                skip: state.history.remote.len(),
                matching: String::new(),
            };
            if self.ask_history(state, query) {
                // Answered at once by the host: try again with what it said.
                if state.history.entries().len() > entries.len() {
                    self.recall(state, edit, older);
                }
            } else {
                edit.waiting = Some(Waiting::Older(1));
            }
        }
    }

    /// Ask for the history `query` names. Whether it was answered now (by
    /// the host, or because there is nobody to ask); otherwise Terminal's
    /// answer arrives on the input.
    fn ask_history(&self, state: &mut State, query: Query) -> bool {
        let asked = HistoryQuery {
            skip: query.skip,
            limit: HISTORY_PAGE,
            matching: query.matching.clone(),
        };
        if let Some(found) = self.host.history(&asked) {
            let done = found.len() < HISTORY_PAGE;
            state.history.answered(&query, found, done);
            return true;
        }
        let Some(key) = state.runs_key.clone() else {
            state.history.answered(&query, Vec::new(), true);
            return true;
        };
        if state.history.pending.is_some() {
            return false;
        }
        if let Some(screen) = self.output(2) {
            let _ = screen.write_str(&format!(
                "\x1b]133;H;k={key};s={};n={HISTORY_PAGE};q={}\x07",
                query.skip,
                b64(&query.matching)
            ));
        }
        state.history.pending = Some(query);
        false
    }

    /// Find the line the search shows: the `index`th line holding its text,
    /// asking for more when the lines answered so far run out. `fresh` when
    /// the text changed.
    fn search_step(&self, state: &mut State, edit: &mut Edit, fresh: bool) {
        let Some(search) = edit.search.as_mut() else {
            return;
        };
        let text = search.text.clone();
        if fresh && state.history.search != text {
            state.history.search.clone_from(&text);
            state.history.search_remote.clear();
            state.history.search_done = text.is_empty();
        }
        let found = if text.is_empty() {
            state.history.entries()
        } else {
            state.history.found(&text)
        };
        if let Some(line) = found.get(search.index) {
            search.failed = false;
            let mut shown = Line::new(line);
            // The cursor goes to the match, as readline puts it.
            if let Some(at) = line.find(&text) {
                shown.home();
                for _ in 0..line.get(..at).unwrap_or_default().chars().count() {
                    shown.right();
                }
            }
            edit.line = shown;
            return;
        }
        let done = if text.is_empty() {
            state.history.remote_done
        } else {
            state.history.search_done
        };
        if done {
            search.failed = true;
            search.index = found.len().saturating_sub(1).min(search.index);
            return;
        }
        let query = Query {
            skip: if text.is_empty() {
                state.history.remote.len()
            } else {
                state.history.search_remote.len()
            },
            matching: text,
        };
        if self.ask_history(state, query) {
            self.search_step(state, edit, false);
        } else {
            edit.waiting = Some(Waiting::Search);
        }
    }

    fn search_key(&self, state: &mut State, edit: &mut Edit, key: &Key) -> SearchKey {
        let Some(search) = edit.search.as_mut() else {
            return SearchKey::Leave;
        };
        match key {
            Key::Char(c) => {
                search.text.push(*c);
                search.index = 0;
                self.search_step(state, edit, true);
                SearchKey::Handled
            }
            Key::Backspace => {
                search.text.pop();
                search.index = 0;
                self.search_step(state, edit, true);
                SearchKey::Handled
            }
            Key::Ctrl('r') => {
                if !search.failed || search.text.is_empty() {
                    search.index = search.index.saturating_add(1);
                }
                self.search_step(state, edit, false);
                SearchKey::Handled
            }
            Key::Ctrl('s') => {
                search.index = search.index.saturating_sub(1);
                self.search_step(state, edit, false);
                SearchKey::Handled
            }
            Key::Ctrl('g' | 'c') => {
                let original = search.original.clone();
                edit.search = None;
                edit.line = original;
                edit.waiting = None;
                SearchKey::Handled
            }
            // Enter while the search waits for an answer: the line it finds
            // then is the one that runs.
            Key::Enter if edit.waiting == Some(Waiting::Search) => SearchKey::Leave,
            _ => {
                // Any other key ends the search on the line it found and
                // does what it does there.
                edit.search = None;
                edit.waiting = None;
                SearchKey::Leave
            }
        }
    }

    /// Tab: complete the word before the cursor; a second Tab lists what it
    /// could become.
    fn tab(&self, screen: &Output, edit: &mut Edit, again: bool) {
        let before = edit.line.before();
        let found = self.complete(&before);
        if found.candidates.is_empty() {
            let _ = screen.write_str("\x07");
            return;
        }
        let typed: String = before.chars().skip(found.start).collect();
        let mut insert = found.insert.clone();
        if found.whole {
            insert.push(' ');
        }
        if !insert.is_empty() && insert != typed && insert.starts_with(typed.trim_end()) {
            edit.checkpoint(false);
            edit.line.replace_back_to(found.start, &insert);
            return;
        }
        if !again {
            let _ = screen.write_str("\x07");
            return;
        }
        self.leave(screen, edit);
        let _ = screen.write_str(&render::table(&found.candidates, self.host.columns()));
        edit.drawn = Drawn::default();
    }
}

impl Shell {
    /// Every line of the person's history, newest first, this shell's own
    /// lines first: what `history` lists.
    pub(crate) fn whole_history(&self) -> Vec<String> {
        let shared = self.editor();
        let mut state = lock_state(&shared);
        let mut remote: Vec<String> = Vec::new();
        loop {
            let query = HistoryQuery {
                skip: remote.len(),
                limit: HISTORY_PAGE,
                matching: String::new(),
            };
            let (page, done) = if let Some(page) = self.host.history(&query) {
                let done = page.len() < HISTORY_PAGE;
                (page, done)
            } else if let Some(answer) = self.ask_terminal_now(&mut state, &query) {
                answer
            } else {
                break;
            };
            let empty = page.is_empty();
            remote.extend(page);
            if done || empty {
                break;
            }
        }
        merged(&state.history.local, &remote, "")
    }

    /// Ask Terminal for one page of the history and wait for its answer,
    /// keeping whatever else was typed meanwhile for the editor. `None`
    /// where there is no Terminal to ask.
    fn ask_terminal_now(
        &self,
        state: &mut State,
        query: &HistoryQuery,
    ) -> Option<(Vec<String>, bool)> {
        let key = state.runs_key.clone()?;
        let (input, screen) = state.terminal.clone()?;
        if !self.host.edit_mode(true) {
            return None;
        }
        let _ = screen.write_str(&format!(
            "\x1b]133;H;k={key};s={};n={};q={}\x07",
            query.skip,
            query.limit,
            b64(&query.matching)
        ));
        let mut decoder = Decoder::default();
        let mut held = Vec::new();
        let answer = loop {
            let mut byte = [0_u8; 1];
            match input.read(&mut byte) {
                Ok(0) => break None,
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break None,
            }
            held.push(byte[0]);
            match decoder.push(byte[0]) {
                Some(Key::Mark(body)) => {
                    if let Some(Incoming::History {
                        query: answered,
                        entries,
                        done,
                    }) = incoming(&body, &key)
                        && answered.skip == query.skip
                        && answered.matching == query.matching
                    {
                        // What is held is this mark, whole: every key
                        // before it went to the editor already.
                        held.clear();
                        break Some((entries, done));
                    }
                    state.typeahead.extend(std::mem::take(&mut held));
                }
                Some(_) => state.typeahead.extend(std::mem::take(&mut held)),
                None => {}
            }
        };
        state.typeahead.extend(held);
        self.host.edit_mode(false);
        answer
    }
}

/// What a key did to the search: took part in it, or ended it and is to
/// do what it does on the line.
enum SearchKey {
    Handled,
    Leave,
}

fn capitalized(word: &str) -> String {
    let mut out = String::new();
    let mut first = true;
    for c in word.chars() {
        if first && c.is_alphanumeric() {
            out.extend(c.to_uppercase());
            first = false;
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminals_answers_are_read_only_with_the_key() {
        let body = format!(
            "H;k=key;s=0;q={};e={},{};end=1",
            b64(""),
            b64("ls"),
            b64("pwd")
        );
        match incoming(&body, "key") {
            Some(Incoming::History {
                query,
                entries,
                done,
            }) => {
                assert_eq!(query.skip, 0);
                assert_eq!(entries, ["ls", "pwd"]);
                assert!(done);
            }
            _ => panic!("a history answer"),
        }
        assert!(incoming(&body, "other").is_none());
        let run = format!("R;k=key;cmd={};agent=1", b64("ls -R"));
        assert!(matches!(
            incoming(&run, "key"),
            Some(Incoming::Run { line, agent: true }) if line == "ls -R"
        ));
    }

    #[test]
    fn this_shells_lines_come_first_and_once() {
        let history = History {
            local: vec!["git status".into(), "ls".into()],
            remote: vec!["ls".into(), "cargo build".into()],
            ..History::default()
        };
        assert_eq!(history.entries(), ["git status", "ls", "cargo build"]);
    }
}
