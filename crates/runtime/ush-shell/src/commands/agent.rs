//! The agent rendering ([`Render::Agent`](super::Render::Agent)): what a
//! command found, in as few tokens as it takes to act on it.
//!
//! The rules every command's agent rendering keeps:
//!
//! - **Grouped.** Names are grouped by directory, one line per directory:
//!   `src/commands/: agent.rs find.rs ls.rs sub/`. Matches are grouped by
//!   file, a line naming the file and then one line per match, indented, with
//!   its line number: `  12: the matched line`. A diff is its hunks only.
//! - **Plain words, no JSON.** Names are separated by spaces; a name that
//!   would read ambiguously (a space, a quote, a trailing `/`, `@` or `*`, or
//!   nothing at all) is quoted as the shell quotes it (`'two words'`).
//!   Directories end with `/`, symlinks with `@`, executables with `*`, as
//!   `ls -F` marks them.
//! - **Exact counts, never a silent cut.** A long result stops at a limit
//!   and ends with how much it left out, exactly, and how to see it all:
//!   `… 1,234 more names (full: rerun with USH_RENDER=text)`. A line longer
//!   than [`LINE_BYTES`] keeps the part around its match and says how many
//!   bytes it cut on each side: `…[812 bytes cut]the match[3,400 bytes cut]…`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;

use super::{Ctx, Stop};

/// Names a listing shows (`ls`, `find`, `grep -l`). Two hundred names is a
/// few thousand tokens: enough to see the shape of a tree and act on it,
/// little enough that a listing of a whole repository does not crowd out
/// everything else an agent is reading. Past it, directories are counted.
pub const NAMES: usize = 200;

/// Lines a line-based result shows (`grep` matches, `diff` hunks), for the
/// same reason as [`NAMES`]: an agent reads every line it is given.
pub const LINES: usize = 200;

/// Directories shown by their count alone once [`NAMES`] are used up. Past
/// that, they are in the total only.
pub const COUNTED_GROUPS: usize = 50;

/// Bytes of one line shown (a match in a minified file can be a megabyte
/// long). Three hundred bytes holds a long line of code whole.
pub const LINE_BYTES: usize = 300;

/// How the full result is got.
pub const FULL: &str = "(full: rerun with USH_RENDER=text)";

/// `1234567` as `1,234,567`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len().saturating_add(digits.len() / 3));
    let len = digits.len();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && len.saturating_sub(i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The last line of a cut result: `… 1,234 more names (full: …)`.
pub fn more(left: u64, what: &str) -> String {
    format!("… {} more {what} {FULL}\n", thousands(left))
}

/// A name as one space-separated word: quoted as the shell quotes it when
/// it would otherwise read as something else.
pub fn quote(name: &str) -> Cow<'_, str> {
    let plain = !name.is_empty()
        && !name
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '\'' | '"' | '\\'))
        && !name.ends_with(['/', '@', '*', ':']);
    if plain {
        Cow::Borrowed(name)
    } else {
        Cow::Owned(format!("'{}'", name.replace('\'', "'\\''")))
    }
}

fn is_continuation(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

/// One line of text, cut to about [`LINE_BYTES`] around `keep` (a match's
/// byte range), saying how much it cut on each side.
pub fn clip(line: &[u8], keep: Option<(usize, usize)>) -> String {
    if line.len() <= LINE_BYTES {
        return String::from_utf8_lossy(line).into_owned();
    }
    let (s, e) = keep.unwrap_or((0, 0));
    let mut start = s.saturating_sub(LINE_BYTES / 4).min(line.len());
    let mut end = start.saturating_add(LINE_BYTES).max(e).min(line.len());
    while start > 0 && line.get(start).copied().is_some_and(is_continuation) {
        start = start.saturating_sub(1);
    }
    while end < line.len() && line.get(end).copied().is_some_and(is_continuation) {
        end = end.saturating_add(1);
    }
    let mut out = String::new();
    if start > 0 {
        let _ = write!(out, "…[{} bytes cut]", thousands(start as u64));
    }
    out.push_str(&String::from_utf8_lossy(
        line.get(start..end).unwrap_or_default(),
    ));
    let after = line.len().saturating_sub(end);
    if after > 0 {
        let _ = write!(out, "[{} bytes cut]…", thousands(after as u64));
    }
    out
}

/// `dir` as a group's header: `src/` for `src`, `/` for `/`.
pub fn header(dir: &str) -> String {
    let quoted = quote(dir.trim_end_matches('/'));
    if dir.chars().all(|c| c == '/') && !dir.is_empty() {
        "/".into()
    } else {
        format!("{quoted}/")
    }
}

/// One directory's names.
struct Group {
    dir: Option<String>,
    names: Vec<String>,
    /// Names of it left out.
    more: u64,
}

/// Names grouped by the directory they are in, up to a limit; past it only
/// counted. Groups keep the order they were first seen in.
pub struct Names {
    limit: usize,
    shown: usize,
    left: u64,
    groups: Vec<Group>,
    index: HashMap<Option<String>, usize>,
}

impl Names {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            shown: 0,
            left: 0,
            groups: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// The index of `dir`'s group, made if it is new.
    fn group(&mut self, dir: Option<&str>) -> usize {
        let key = dir.map(String::from);
        if let Some(&at) = self.index.get(&key) {
            return at;
        }
        let at = self.groups.len();
        self.groups.push(Group {
            dir: key.clone(),
            names: Vec::new(),
            more: 0,
        });
        self.index.insert(key, at);
        at
    }

    /// A directory that was listed, so it shows even with nothing in it.
    pub fn open(&mut self, dir: Option<&str>) {
        let _ = self.group(dir);
    }

    /// `name` (already marked, `sub/`) in `dir`.
    pub fn add(&mut self, dir: Option<&str>, name: &str, marker: &str) {
        let show = self.shown < self.limit;
        if show {
            self.shown = self.shown.saturating_add(1);
        } else {
            self.left = self.left.saturating_add(1);
        }
        let at = self.group(dir);
        if let Some(group) = self.groups.get_mut(at) {
            if show {
                group.names.push(format!("{}{marker}", quote(name)));
            } else {
                group.more = group.more.saturating_add(1);
            }
        }
    }

    /// A path as printed (`./src/main.rs`), grouped by the directory part.
    pub fn add_path(&mut self, shown: &str, marker: &str) {
        match shown.rsplit_once('/') {
            Some((dir, name)) if !name.is_empty() => {
                let dir = if dir.is_empty() { "/" } else { dir };
                self.add(Some(dir), name, marker);
            }
            _ => self.add(None, shown, marker),
        }
    }

    /// Whether anything was added or opened.
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Every group, one line each, and the line saying what was left out.
    pub fn render(&self, what: &str) -> String {
        let mut out = String::new();
        let mut counted = 0_usize;
        for g in &self.groups {
            let head = g.dir.as_deref().map(header);
            if g.names.is_empty() && g.more > 0 {
                if counted < COUNTED_GROUPS {
                    counted = counted.saturating_add(1);
                    let _ = writeln!(
                        out,
                        "{}: {} more",
                        head.as_deref().unwrap_or("."),
                        thousands(g.more)
                    );
                }
                continue;
            }
            if let Some(head) = &head {
                out.push_str(head);
                out.push(':');
                if g.names.is_empty() {
                    out.push_str(" (empty)");
                }
            }
            for (i, name) in g.names.iter().enumerate() {
                if i > 0 || head.is_some() {
                    out.push(' ');
                }
                out.push_str(name);
            }
            if g.more > 0 {
                let _ = write!(out, " +{} more", thousands(g.more));
            }
            out.push('\n');
        }
        if self.left > 0 {
            out.push_str(&more(self.left, what));
        }
        out
    }
}

/// Lines in groups (a file's matches, a diff's hunks), up to a limit; past
/// it only counted. Each line is written as it comes, its group's header
/// just before the group's first line shown.
pub struct Lines<'c, 'a> {
    ctx: &'c Ctx<'a>,
    limit: usize,
    shown: usize,
    left: u64,
    groups_cut: u64,
    /// The header of the group lines now go to, until its first is shown.
    pending: Option<String>,
    /// Whether the current group has lost a line already.
    cut_here: bool,
}

impl<'c, 'a> Lines<'c, 'a> {
    pub const fn new(ctx: &'c Ctx<'a>, limit: usize) -> Self {
        Self {
            ctx,
            limit,
            shown: 0,
            left: 0,
            groups_cut: 0,
            pending: None,
            cut_here: false,
        }
    }

    /// Start a group: its header, if it has one, goes out with its first
    /// line.
    pub fn group(&mut self, head: Option<String>) {
        self.pending = head;
        self.cut_here = false;
    }

    /// One line of the current group.
    pub fn line(&mut self, text: &str) -> Result<(), Stop> {
        if self.shown >= self.limit {
            self.left = self.left.saturating_add(1);
            if !self.cut_here {
                self.cut_here = true;
                self.groups_cut = self.groups_cut.saturating_add(1);
            }
            return Ok(());
        }
        self.shown = self.shown.saturating_add(1);
        let mut out = String::new();
        if let Some(head) = self.pending.take() {
            out.push_str(&head);
            out.push('\n');
        }
        out.push_str(text);
        out.push('\n');
        self.ctx.out_str(&out)
    }

    /// The line saying what was left out, if anything was: `… 12 more lines
    /// in 3 files (full: …)`.
    /// `group` is what a group is called, one and many (`("file",
    /// "files")`).
    pub fn finish(&self, what: &str, group: (&str, &str)) -> Result<(), Stop> {
        if self.left == 0 {
            return Ok(());
        }
        let noun = match self.groups_cut {
            0 => what.to_string(),
            1 => format!("{what} in 1 {}", group.0),
            n => format!("{what} in {} {}", thousands(n), group.1),
        };
        self.ctx.out_str(&more(self.left, &noun))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_have_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1234), "1,234");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn names_that_read_ambiguously_are_quoted() {
        assert_eq!(quote("a.rs"), "a.rs");
        assert_eq!(quote("two words"), "'two words'");
        assert_eq!(quote("it's"), "'it'\\''s'");
        assert_eq!(quote("x*"), "'x*'");
        assert_eq!(quote(""), "''");
    }

    #[test]
    fn a_long_line_keeps_its_match_and_says_what_it_cut() {
        let mut line = vec![b'a'; 1000];
        line.extend_from_slice(b"NEEDLE");
        line.extend(vec![b'b'; 2000]);
        let out = clip(&line, Some((1000, 1006)));
        assert!(out.contains("NEEDLE"), "{out}");
        assert!(out.starts_with("…[925 bytes cut]"), "{out}");
        assert!(out.ends_with("[1,781 bytes cut]…"), "{out}");
    }

    #[test]
    fn names_group_by_directory_and_count_what_is_left_out() {
        let mut names = Names::new(3);
        names.add_path(".", "/");
        names.add_path("./a", "");
        names.add_path("./sub", "/");
        names.add_path("./sub/x", "");
        names.add_path("./sub/y", "");
        names.add_path("./b", "");
        assert_eq!(
            names.render("names"),
            format!("./\n./: a sub/ +1 more\n./sub/: 2 more\n… 3 more names {FULL}\n")
        );
    }
}
