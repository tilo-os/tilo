//! `grep`: print the lines that match a pattern, as GNU grep does.
//!
//! Patterns are POSIX basic (the default), extended (`-E`) or fixed
//! strings (`-F`), translated to the `regex` crate's syntax. Back-references
//! have no equivalent there, so a pattern using one is refused. Directories
//! are searched through the host under `-r`/`-R`, in name order.
//!
//! What a search finds (lines, counts, names) goes to one of two
//! renderings. For people, GNU's text. For an agent, matches are grouped by
//! file, a line naming the file and then each line as `  12: text` (`-` for
//! a context line, as GNU marks it), line numbers always; `-c` is
//! `file: count`; `-l`/`-L` names are grouped by directory. Past
//! [`LINES`](super::agent::LINES) lines it counts, and says how many it left
//! out.

use std::path::Path;

use regex::bytes::{Regex, RegexBuilder};

use super::agent::{self, Lines, Names};
use super::util::{Arg, Spec, glob, parse, split_lines, value};
use super::{Ctx, Render, Stop, describe};
use crate::host::Kind;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Syntax {
    Basic,
    Extended,
    Fixed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Listing {
    Lines,
    Count,
    WithMatch,
    WithoutMatch,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Binary {
    Report,
    Text,
    Skip,
}

#[expect(clippy::struct_excessive_bools, reason = "one switch per GNU flag")]
struct Opts {
    invert: bool,
    line_number: bool,
    only: bool,
    quiet: bool,
    silent: bool,
    word: bool,
    listing: Listing,
    with_name: Option<bool>,
    /// `-r` (`Some(false)`) or `-R`, which follows every symlink.
    recursive: Option<bool>,
    before: usize,
    after: usize,
    context: bool,
    max_count: Option<u64>,
    binary: Binary,
    include: Vec<String>,
    exclude: Vec<String>,
    exclude_dir: Vec<String>,
    label: String,
}

const LONGS: &[(&str, bool)] = &[
    ("extended-regexp", false),
    ("fixed-strings", false),
    ("basic-regexp", false),
    ("perl-regexp", false),
    ("regexp", true),
    ("file", true),
    ("ignore-case", false),
    ("no-ignore-case", false),
    ("word-regexp", false),
    ("line-regexp", false),
    ("invert-match", false),
    ("count", false),
    ("color", false),
    ("colour", false),
    ("files-without-match", false),
    ("files-with-matches", false),
    ("max-count", true),
    ("only-matching", false),
    ("quiet", false),
    ("silent", false),
    ("no-messages", false),
    ("byte-offset", false),
    ("with-filename", false),
    ("no-filename", false),
    ("label", true),
    ("line-number", false),
    ("initial-tab", false),
    ("null", false),
    ("null-data", false),
    ("after-context", true),
    ("before-context", true),
    ("context", true),
    ("text", false),
    ("binary-files", true),
    ("recursive", false),
    ("dereference-recursive", false),
    ("include", true),
    ("exclude", true),
    ("exclude-dir", true),
    ("line-buffered", false),
];

#[expect(
    clippy::too_many_lines,
    reason = "one arm per GNU flag, read in one place"
)]
pub fn grep(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    // `-NUM` is `-C NUM`.
    let mut expanded = Vec::with_capacity(args.len());
    let mut done = false;
    for arg in args {
        if !done
            && arg.len() > 1
            && arg.starts_with('-')
            && arg.bytes().skip(1).all(|b| b.is_ascii_digit())
        {
            expanded.push("-C".to_string());
            expanded.push(arg.get(1..).unwrap_or("0").to_string());
        } else {
            done |= arg == "--";
            expanded.push(arg.clone());
        }
    }
    let parsed = match parse(ctx, &expanded, &Spec::new("efmABCd", LONGS)) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let mut o = Opts {
        invert: false,
        line_number: false,
        only: false,
        quiet: false,
        silent: false,
        word: false,
        listing: Listing::Lines,
        with_name: None,
        recursive: None,
        before: 0,
        after: 0,
        context: false,
        max_count: None,
        binary: Binary::Report,
        include: Vec::new(),
        exclude: Vec::new(),
        exclude_dir: Vec::new(),
        label: "(standard input)".into(),
    };
    let (mut syntax, mut fold, mut whole_line) = (Syntax::Basic, false, false);
    let mut patterns: Option<Vec<String>> = None;
    let mut operands = Vec::new();
    for arg in parsed {
        let (flag, v) = match arg {
            Arg::Operand(op) => {
                operands.push(op);
                continue;
            }
            Arg::Flag(f, v) => (f, v),
        };
        let val = value(v.as_ref());
        let number = |what: &str| -> Result<usize, i32> {
            val.parse().map_err(|_| {
                ctx.error(format!("{val}: invalid {what} argument"));
                2
            })
        };
        match flag.as_str() {
            "-E" | "--extended-regexp" => syntax = Syntax::Extended,
            "-F" | "--fixed-strings" => syntax = Syntax::Fixed,
            "-G" | "--basic-regexp" => syntax = Syntax::Basic,
            "-e" | "--regexp" => patterns
                .get_or_insert_with(Vec::new)
                .extend(val.split('\n').map(String::from)),
            "-f" | "--file" => match ctx.read_operand(val) {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let list = patterns.get_or_insert_with(Vec::new);
                    list.extend(text.lines().map(String::from));
                }
                Err(e) => {
                    ctx.io_error(val, &e);
                    return 2;
                }
            },
            "-i" | "-y" | "--ignore-case" => fold = true,
            "--no-ignore-case" => fold = false,
            "-w" | "--word-regexp" => o.word = true,
            "-x" | "--line-regexp" => whole_line = true,
            "-v" | "--invert-match" => o.invert = true,
            "-c" | "--count" => o.listing = Listing::Count,
            "-l" | "--files-with-matches" => o.listing = Listing::WithMatch,
            "-L" | "--files-without-match" => o.listing = Listing::WithoutMatch,
            "-m" | "--max-count" => match number("max count") {
                Ok(n) => o.max_count = Some(n as u64),
                Err(code) => return code,
            },
            "-o" | "--only-matching" => o.only = true,
            "-q" | "--quiet" | "--silent" => o.quiet = true,
            "-s" | "--no-messages" => o.silent = true,
            "-H" | "--with-filename" => o.with_name = Some(true),
            "-h" | "--no-filename" => o.with_name = Some(false),
            "--label" => val.clone_into(&mut o.label),
            "-n" | "--line-number" => o.line_number = true,
            "-A" | "--after-context" => match number("context length") {
                Ok(n) => {
                    o.after = n;
                    o.context = true;
                }
                Err(code) => return code,
            },
            "-B" | "--before-context" => match number("context length") {
                Ok(n) => {
                    o.before = n;
                    o.context = true;
                }
                Err(code) => return code,
            },
            "-C" | "--context" => match number("context length") {
                Ok(n) => {
                    o.before = n;
                    o.after = n;
                    o.context = true;
                }
                Err(code) => return code,
            },
            "-a" | "--text" => o.binary = Binary::Text,
            "-I" => o.binary = Binary::Skip,
            "--binary-files" => match val {
                "binary" => o.binary = Binary::Report,
                "text" => o.binary = Binary::Text,
                "without-match" => o.binary = Binary::Skip,
                _ => {
                    ctx.error(format!("invalid argument '{val}' for '--binary-files'"));
                    return 2;
                }
            },
            "-r" | "--recursive" => o.recursive = Some(false),
            "-R" | "--dereference-recursive" => o.recursive = Some(true),
            "--include" => o.include.push(val.to_string()),
            "--exclude" => o.exclude.push(val.to_string()),
            "--exclude-dir" => o.exclude_dir.push(val.to_string()),
            "--color" | "--colour" if matches!(v.as_deref(), Some("never" | "auto")) => {}
            "-U" | "--line-buffered" => {}
            other => return ctx.unsupported(other),
        }
    }
    let patterns = if let Some(p) = patterns {
        p
    } else {
        if operands.is_empty() {
            ctx.error("no pattern given");
            return 2;
        }
        let first = operands.remove(0);
        first.split('\n').map(String::from).collect()
    };
    let regex = match build(&patterns, syntax, fold, whole_line) {
        Ok(r) => r,
        Err(message) => {
            ctx.error(message);
            return 2;
        }
    };
    let implicit = operands.is_empty();
    if implicit {
        operands.push(if o.recursive.is_some() { "." } else { "-" }.into());
    }
    let with_name = o.with_name.unwrap_or_else(|| {
        operands.len() > 1
            || o.recursive.is_some()
                && operands.first().is_some_and(|op| {
                    op != "-"
                        && ctx
                            .host
                            .metadata(&ctx.resolve(op))
                            .is_ok_and(|m| m.is_dir())
                })
    });
    // An agent is told which file a match is in unless the only input is
    // standard input.
    let headers = !(operands.len() == 1 && operands.first().is_some_and(|op| op == "-"));
    let mut run = Run {
        ctx,
        o: &o,
        regex,
        with_name,
        matched: false,
        error: false,
        grouped: false,
        agent: (ctx.render == Render::Agent).then(|| Agent {
            lines: Lines::new(ctx, agent::LINES),
            names: Names::new(agent::NAMES),
            current: None,
            headers,
        }),
    };
    let result = (|| -> Result<(), Stop> {
        for op in &operands {
            run.operand(op, implicit)?;
        }
        Ok(())
    })();
    let result = result.and_then(|()| match &run.agent {
        Some(a) => {
            if !a.names.is_empty() {
                ctx.out_str(&a.names.render("names"))?;
            }
            a.lines.finish("lines", ("file", "files"))
        }
        None => Ok(()),
    });
    match result {
        Ok(()) => {}
        Err(Stop(0)) => return 0,
        Err(Stop(code)) => return code,
    }
    if run.error && !(o.quiet && run.matched) {
        2
    } else {
        i32::from(!run.matched)
    }
}

/// The compiled pattern, and whether matches must be whole words.
struct Matcher {
    regex: Option<Regex>,
}

fn build(
    patterns: &[String],
    syntax: Syntax,
    fold: bool,
    whole_line: bool,
) -> Result<Matcher, String> {
    if patterns.is_empty() {
        return Ok(Matcher { regex: None });
    }
    let mut parts = Vec::with_capacity(patterns.len());
    for p in patterns {
        parts.push(match syntax {
            Syntax::Fixed => regex::escape(p),
            Syntax::Basic => translate(p, false)?,
            Syntax::Extended => translate(p, true)?,
        });
    }
    let joined = parts
        .iter()
        .map(|p| format!("(?:{p})"))
        .collect::<Vec<_>>()
        .join("|");
    let source = if whole_line {
        format!("^(?:{joined})$")
    } else {
        joined
    };
    RegexBuilder::new(&source)
        .case_insensitive(fold)
        .build()
        .map(|regex| Matcher { regex: Some(regex) })
        .map_err(|e| format!("invalid pattern: {e}"))
}

/// A POSIX basic or extended regular expression in the `regex` crate's
/// syntax.
pub fn translate(pattern: &str, extended: bool) -> Result<String, String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut i = 0_usize;
    // Where `*` is a literal and `^` an anchor: the start of the pattern,
    // of a group, or of an alternative.
    let mut at_start = true;
    while let Some(&c) = chars.get(i) {
        i = i.saturating_add(1);
        let was_start = at_start;
        at_start = false;
        match c {
            '\\' => {
                let Some(&e) = chars.get(i) else {
                    return Err("trailing backslash (\\)".into());
                };
                i = i.saturating_add(1);
                match e {
                    '(' if !extended => {
                        out.push('(');
                        at_start = true;
                    }
                    ')' if !extended => out.push(')'),
                    '|' if !extended => {
                        out.push('|');
                        at_start = true;
                    }
                    '{' if !extended => {
                        let close = find_interval(&chars, i, true).ok_or("Unmatched \\{")?;
                        let body: String = chars.get(i..close).unwrap_or_default().iter().collect();
                        out.push_str(&interval(&body));
                        i = close.saturating_add(2);
                    }
                    '+' | '?' if !extended => {
                        if was_start {
                            out.push('\\');
                        }
                        out.push(e);
                    }
                    '<' => out.push_str("\\b{start}"),
                    '>' => out.push_str("\\b{end}"),
                    'b' | 'B' | 'w' | 'W' | 's' | 'S' => {
                        out.push('\\');
                        out.push(e);
                    }
                    '`' => out.push_str("\\A"),
                    '\'' => out.push_str("\\z"),
                    '1'..='9' => {
                        return Err(format!("back-references (\\{e}) are not supported"));
                    }
                    other => out.push_str(&regex::escape(&other.to_string())),
                }
            }
            '*' if was_start => out.push_str("\\*"),
            '+' | '?' if extended && was_start => {
                out.push('\\');
                out.push(c);
            }
            '*' => out.push('*'),
            '+' | '?' | '|' | '(' | ')' if extended => {
                out.push(c);
                at_start = matches!(c, '|' | '(');
            }
            '{' if extended && !was_start => match find_interval(&chars, i, false) {
                Some(close) => {
                    let body: String = chars.get(i..close).unwrap_or_default().iter().collect();
                    out.push_str(&interval(&body));
                    i = close.saturating_add(1);
                }
                None => out.push_str("\\{"),
            },
            '^' if was_start || extended => {
                out.push('^');
                at_start = true;
            }
            '$' if extended || anchor_end(&chars, i) => out.push('$'),
            '.' => out.push('.'),
            '[' => i = bracket(&chars, i, &mut out)?,
            other => out.push_str(&regex::escape(&other.to_string())),
        }
    }
    Ok(out)
}

/// Whether a `$` just before `at` ends a basic expression or a group in it.
fn anchor_end(chars: &[char], at: usize) -> bool {
    match chars.get(at) {
        None => true,
        Some('\\') => matches!(chars.get(at.saturating_add(1)), Some(')' | '|')),
        _ => false,
    }
}

/// The index of the `}` (or `\}`) closing an interval whose body starts at
/// `from`, if the body is a valid `m`, `m,`, `,n` or `m,n`.
fn find_interval(chars: &[char], from: usize, escaped: bool) -> Option<usize> {
    let mut i = from;
    let mut comma = false;
    while let Some(&c) = chars.get(i) {
        match c {
            '0'..='9' => {}
            ',' if !comma => comma = true,
            '\\' if escaped && chars.get(i.saturating_add(1)) == Some(&'}') => return Some(i),
            '}' if !escaped => return Some(i),
            _ => return None,
        }
        i = i.saturating_add(1);
    }
    None
}

fn interval(body: &str) -> String {
    match body.strip_prefix(',') {
        Some(max) => format!("{{0,{max}}}"),
        None => format!("{{{body}}}"),
    }
}

/// Copy the bracket expression starting after the `[` at `from`, escaping
/// what the `regex` crate reads specially inside a class. Answers the index
/// after its `]`.
fn bracket(chars: &[char], from: usize, out: &mut String) -> Result<usize, String> {
    let mut i = from;
    let mut body = String::from("[");
    if chars.get(i) == Some(&'^') {
        body.push('^');
        i = i.saturating_add(1);
    }
    let first = i;
    loop {
        let Some(&c) = chars.get(i) else {
            return Err("Unmatched [, [^, [:, [., or [=".into());
        };
        if c == ']' && i != first {
            body.push(']');
            out.push_str(&body);
            return Ok(i.saturating_add(1));
        }
        if c == '['
            && let Some(&kind @ (':' | '=' | '.')) = chars.get(i.saturating_add(1))
        {
            let start = i.saturating_add(2);
            let mut j = start;
            while let Some(&d) = chars.get(j) {
                if d == kind && chars.get(j.saturating_add(1)) == Some(&']') {
                    break;
                }
                j = j.saturating_add(1);
            }
            if chars.get(j).is_none() {
                return Err("Unmatched [, [^, [:, [., or [=".into());
            }
            let name: String = chars.get(start..j).unwrap_or_default().iter().collect();
            if kind == ':' {
                body.push_str("[:");
                body.push_str(&name);
                body.push_str(":]");
            } else {
                for n in name.chars() {
                    body.push_str(&class_char(n));
                }
            }
            i = j.saturating_add(2);
            continue;
        }
        let last = chars.get(i.saturating_add(1)) == Some(&']');
        if c == '-' && (i == first || last) {
            body.push_str("\\-");
        } else if c == '-' {
            body.push('-');
        } else {
            body.push_str(&class_char(c));
        }
        i = i.saturating_add(1);
    }
}

/// A literal character inside a `regex` crate class.
fn class_char(c: char) -> String {
    match c {
        '\\' | '[' | ']' | '^' | '&' | '~' | '-' => format!("\\{c}"),
        other => other.to_string(),
    }
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

impl Matcher {
    /// The first match in `line` at or after `from`, as `(start, end)`. With
    /// `word`, only a match with no word character on either side counts, as
    /// `grep -w` asks.
    fn find(&self, line: &[u8], from: usize, word: bool) -> Option<(usize, usize)> {
        let regex = self.regex.as_ref()?;
        let mut pos = from;
        while pos <= line.len() {
            let m = regex.find_at(line, pos)?;
            let (s, e) = (m.start(), m.end());
            if !word {
                return Some((s, e));
            }
            let before = s.checked_sub(1).and_then(|p| line.get(p)).copied();
            let after = line.get(e).copied();
            if !before.is_some_and(is_word_byte) && !after.is_some_and(is_word_byte) {
                return Some((s, e));
            }
            // A shorter match at the same start may still end at a word
            // boundary (`-w ab*` on `abb_ a`).
            if !before.is_some_and(is_word_byte) {
                for end in (s.saturating_add(1)..e).rev() {
                    if line.get(end).copied().is_some_and(is_word_byte) {
                        continue;
                    }
                    let shorter = regex.find_at(line.get(..end).unwrap_or_default(), s);
                    if shorter.is_some_and(|m| m.start() == s && m.end() == end) {
                        return Some((s, end));
                    }
                }
            }
            pos = s.saturating_add(1);
        }
        None
    }
}

#[expect(clippy::struct_excessive_bools, reason = "the state of one search")]
struct Run<'c, 'a> {
    ctx: &'c Ctx<'a>,
    o: &'c Opts,
    regex: Matcher,
    with_name: bool,
    matched: bool,
    error: bool,
    /// Whether a group of lines with context has been printed, so the next
    /// one is set apart with `--`.
    grouped: bool,
    /// The agent rendering, when the reader is one.
    agent: Option<Agent<'c, 'a>>,
}

/// What an agent is shown, as it is found.
struct Agent<'c, 'a> {
    lines: Lines<'c, 'a>,
    names: Names,
    /// The file whose lines are being shown.
    current: Option<String>,
    headers: bool,
}

impl Agent<'_, '_> {
    /// Start `name`'s group if its lines are not the ones being shown.
    fn file(&mut self, name: &str) {
        if self.current.as_deref() != Some(name) {
            self.current = Some(name.to_string());
            let head = self.headers.then(|| name.to_string());
            self.lines.group(head);
        }
    }
}

impl Run<'_, '_> {
    fn complain(&mut self, name: &str, why: &str) {
        self.error = true;
        if !self.o.silent {
            self.ctx.error(format!("{name}: {why}"));
        }
    }

    fn operand(&mut self, op: &str, implicit: bool) -> Result<(), Stop> {
        if op == "-" {
            return match self.ctx.io.stdin.read_all() {
                Ok(bytes) => {
                    let label = self.o.label.clone();
                    self.search(&label, &bytes)
                }
                Err(e) => {
                    self.complain(&self.o.label.clone(), &describe(&e));
                    Ok(())
                }
            };
        }
        let path = self.ctx.resolve(op);
        let meta = match self.ctx.host.metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                self.complain(op, &describe(&e));
                return Ok(());
            }
        };
        if meta.is_dir() {
            if self.o.recursive.is_none() {
                self.complain(op, "Is a directory");
                return Ok(());
            }
            let shown = if implicit { "" } else { op };
            return self.walk(shown, &path);
        }
        if !self.wanted(&super::files::base_name(op)) {
            return Ok(());
        }
        self.file(op, &path)
    }

    fn wanted(&self, name: &str) -> bool {
        let included =
            self.o.include.is_empty() || self.o.include.iter().any(|g| glob(g, name, false));
        included && !self.o.exclude.iter().any(|g| glob(g, name, false))
    }

    fn walk(&mut self, shown: &str, dir: &Path) -> Result<(), Stop> {
        let mut entries = match self.ctx.host.read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                let name = if shown.is_empty() { "." } else { shown };
                self.complain(name, &describe(&e));
                return Ok(());
            }
        };
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        for entry in entries {
            let child = dir.join(&entry.name);
            let child_shown = if shown.is_empty() {
                entry.name.clone()
            } else if shown.ends_with('/') {
                format!("{shown}{}", entry.name)
            } else {
                format!("{shown}/{}", entry.name)
            };
            let follow = self.o.recursive == Some(true);
            let kind = match entry.kind {
                Kind::Symlink if !follow => continue,
                Kind::Symlink => match self.ctx.host.metadata(&child) {
                    Ok(m) => m.kind,
                    Err(_) => continue,
                },
                other => other,
            };
            match kind {
                Kind::Dir => {
                    if !self
                        .o
                        .exclude_dir
                        .iter()
                        .any(|g| glob(g, &entry.name, false))
                    {
                        self.walk(&child_shown, &child)?;
                    }
                }
                Kind::File if self.wanted(&entry.name) => self.file(&child_shown, &child)?,
                _ => {}
            }
        }
        Ok(())
    }

    fn file(&mut self, shown: &str, path: &Path) -> Result<(), Stop> {
        let mut bytes = Vec::new();
        let read = self
            .ctx
            .host
            .open_read(path)
            .and_then(|mut r| std::io::Read::read_to_end(&mut r, &mut bytes));
        match read {
            Ok(_) => self.search(shown, &bytes),
            Err(e) => {
                self.complain(shown, &describe(&e));
                Ok(())
            }
        }
    }

    fn prefix(&self, name: &str, number: usize, sep: char) -> String {
        let mut p = String::new();
        if self.with_name {
            p.push_str(name);
            p.push(sep);
        }
        if self.o.line_number {
            p.push_str(&number.saturating_add(1).to_string());
            p.push(sep);
        }
        p
    }

    /// One line found: a match (`:`, with the span to keep in view) or a
    /// line of context (`-`).
    fn print_line(
        &mut self,
        name: &str,
        number: usize,
        sep: char,
        line: &[u8],
        span: Option<(usize, usize)>,
    ) -> Result<(), Stop> {
        if let Some(a) = &mut self.agent {
            a.file(name);
            let indent = if a.headers { "  " } else { "" };
            let text = agent::clip(line, span);
            return a
                .lines
                .line(&format!("{indent}{}{sep} {text}", number.saturating_add(1)));
        }
        let mut out = self.prefix(name, number, sep).into_bytes();
        out.extend_from_slice(line);
        out.push(b'\n');
        self.ctx.out(&out)
    }

    /// A file `-l`, `-L` or a skipped binary file names.
    fn print_name(&mut self, name: &str) -> Result<(), Stop> {
        if let Some(a) = &mut self.agent {
            a.names.add_path(name, "");
            return Ok(());
        }
        self.ctx.out_str(&format!("{name}\n"))
    }

    fn print_count(&mut self, name: &str, count: u64) -> Result<(), Stop> {
        if let Some(a) = &mut self.agent {
            a.current = None;
            a.lines.group(None);
            return if a.headers {
                a.lines.line(&format!("{name}: {count}"))
            } else {
                a.lines.line(&count.to_string())
            };
        }
        let text = if self.with_name {
            format!("{name}:{count}\n")
        } else {
            format!("{count}\n")
        };
        self.ctx.out_str(&text)
    }

    /// `--` between groups of lines with context; an agent sees the gap in
    /// the line numbers.
    fn print_separator(&self) -> Result<(), Stop> {
        if self.agent.is_some() {
            return Ok(());
        }
        self.ctx.out_str("--\n")
    }

    fn search(&mut self, name: &str, bytes: &[u8]) -> Result<(), Stop> {
        let o = self.o;
        let binary = o.binary != Binary::Text && bytes.contains(&0);
        if binary && o.binary == Binary::Skip {
            if o.listing == Listing::WithoutMatch {
                self.print_name(name)?;
            }
            return Ok(());
        }
        let lines: Vec<&[u8]> = split_lines(bytes, b'\n')
            .into_iter()
            .map(|l| super::util::chomp(l, b'\n'))
            .collect();
        let printing = o.listing == Listing::Lines && !o.quiet && !binary;
        let context = printing && o.context && !o.only;
        let mut count = 0_u64;
        let mut last_printed: Option<usize> = None;
        let mut after_left = 0_usize;
        let mut binary_hit = false;
        for (i, line) in lines.iter().enumerate() {
            let limit_reached = o.max_count.is_some_and(|m| count >= m);
            if limit_reached {
                if context && after_left > 0 {
                    self.print_line(name, i, '-', line, None)?;
                    after_left = after_left.saturating_sub(1);
                    continue;
                }
                break;
            }
            let first = self.regex.find(line, 0, o.word);
            let selected = first.is_some() != o.invert;
            if !selected {
                if context && after_left > 0 {
                    self.print_line(name, i, '-', line, None)?;
                    last_printed = Some(i);
                    after_left = after_left.saturating_sub(1);
                }
                continue;
            }
            count = count.saturating_add(1);
            self.matched = true;
            if o.quiet {
                return Err(Stop(0));
            }
            if binary && o.listing == Listing::Lines {
                binary_hit = true;
                break;
            }
            if o.listing == Listing::WithMatch || o.listing == Listing::WithoutMatch {
                break;
            }
            if !printing {
                continue;
            }
            if context {
                let start = i.saturating_sub(o.before);
                let from = last_printed.map_or(start, |l| start.max(l.saturating_add(1)));
                let gap = last_printed.is_none_or(|l| from > l.saturating_add(1));
                if self.grouped && gap {
                    self.print_separator()?;
                }
                for (j, before) in lines.iter().enumerate().take(i).skip(from) {
                    self.print_line(name, j, '-', before, None)?;
                }
                self.grouped = true;
                after_left = o.after;
            }
            if o.only {
                if !o.invert {
                    let mut pos = 0;
                    while let Some((s, e)) = self.regex.find(line, pos, o.word) {
                        if e > s {
                            let part = line.get(s..e).unwrap_or_default();
                            self.print_line(name, i, ':', part, Some((0, part.len())))?;
                            pos = e;
                        } else {
                            pos = e.saturating_add(1);
                        }
                    }
                }
            } else {
                self.print_line(name, i, ':', line, first)?;
            }
            last_printed = Some(i);
        }
        match o.listing {
            Listing::Count => self.print_count(name, count)?,
            Listing::WithMatch if count > 0 => self.print_name(name)?,
            Listing::WithoutMatch if count == 0 => self.print_name(name)?,
            _ => {}
        }
        if binary_hit {
            // GNU grep says this on standard error, so a pipeline reading
            // the matches never takes it for one.
            self.ctx.error(format!("{name}: binary file matches"));
        }
        Ok(())
    }
}
