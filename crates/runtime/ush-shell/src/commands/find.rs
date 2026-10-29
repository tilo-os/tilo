//! `find`: walk directories through the host and print what an expression
//! selects.
//!
//! The tests and operators people use are here (`-name`, `-path`, `-type`,
//! `-size`, `-newer`, `-mtime`, `!`, `-a`, `-o`, parentheses, `-prune`).
//! Entries of a directory are visited in name order, so the output is the
//! same on every host.
//!
//! `-exec`, `-execdir`, `-ok` and `-okdir` run their command back through
//! the shell ([`Ctx::dispatch`]), never as a process; `-exec … {} +` runs
//! one command per batch of names, as GNU does. `-delete` asks for the
//! environment's `rm` (or `rmdir`) action, so it is undoable like any.
//!
//! For an agent, the names `-print` prints are grouped by directory
//! ([`super::agent::Names`]).

use std::path::{Path, PathBuf};

use super::agent::{NAMES, Names};
use super::files::base_name;
use super::util::glob;
use super::{Ctx, Render, Stop, describe};
use crate::host::{Kind, Metadata};
use crate::io::{Input, Stdio};

/// The longest command line `-exec … {} +` builds, in bytes: GNU xargs'
/// default, so batches split where GNU's would.
const BATCH_BYTES: usize = super::xargs::DEFAULT_SIZE;

/// A command `-exec`, `-execdir`, `-ok` or `-okdir` runs.
#[derive(Debug)]
struct Exec {
    /// The command line; `{}` in it is the name. Under `+`, without the
    /// final `{}`, which the batch's names take the place of.
    argv: Vec<String>,
    /// `+`: names are collected and run in batches.
    batch: bool,
    /// `-execdir`/`-okdir`: run in the name's directory, as `./name`.
    in_dir: bool,
    /// `-ok`/`-okdir`: ask first.
    ask: bool,
    pending: Vec<String>,
    pending_dir: Option<PathBuf>,
    pending_len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cmp {
    Less,
    Equal,
    More,
}

#[derive(Debug)]
enum Expr {
    And(Box<Self>, Box<Self>),
    Or(Box<Self>, Box<Self>),
    Comma(Box<Self>, Box<Self>),
    Not(Box<Self>),
    Const(bool),
    Name(String, bool),
    Path(String, bool),
    Type(Vec<char>),
    Empty,
    Size(Cmp, u64, u64),
    Newer(i64),
    /// `-mtime`/`-mmin`: age in units of this many seconds.
    Age(Cmp, i64, i64),
    Print,
    Print0,
    Prune,
    Quit,
    /// The command of this index in the walk's list.
    Exec(usize),
    Delete,
}

impl Expr {
    fn has_action(&self) -> bool {
        match self {
            Self::And(a, b) | Self::Or(a, b) | Self::Comma(a, b) => {
                a.has_action() || b.has_action()
            }
            Self::Not(a) => a.has_action(),
            Self::Print | Self::Print0 | Self::Quit | Self::Exec(_) | Self::Delete => true,
            _ => false,
        }
    }
}

/// Why parsing the expression failed: already reported, with this status.
struct Bad(i32);

struct Parser<'c, 'a> {
    ctx: &'c Ctx<'a>,
    tokens: Vec<String>,
    at: usize,
    maxdepth: Option<usize>,
    mindepth: usize,
    depth_first: bool,
    execs: Vec<Exec>,
}

impl Parser<'_, '_> {
    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.at).map(String::as_str)
    }

    fn next(&mut self) -> Option<String> {
        let t = self.tokens.get(self.at).cloned();
        if t.is_some() {
            self.at = self.at.saturating_add(1);
        }
        t
    }

    fn fail(&self, message: impl std::fmt::Display) -> Bad {
        self.ctx.error(message);
        Bad(2)
    }

    fn comma(&mut self) -> Result<Expr, Bad> {
        let mut left = self.or()?;
        while self.peek() == Some(",") {
            self.next();
            let right = self.or()?;
            left = Expr::Comma(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn or(&mut self) -> Result<Expr, Bad> {
        let mut left = self.and()?;
        while matches!(self.peek(), Some("-o" | "-or")) {
            self.next();
            let right = self.and()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Expr, Bad> {
        let mut left = self.not()?;
        loop {
            match self.peek() {
                Some("-a" | "-and") => {
                    self.next();
                }
                Some("-o" | "-or" | "," | ")") | None => return Ok(left),
                Some(_) => {}
            }
            let right = self.not()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
    }

    fn not(&mut self) -> Result<Expr, Bad> {
        if matches!(self.peek(), Some("!" | "-not")) {
            self.next();
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.primary()
    }

    fn arg(&mut self, name: &str) -> Result<String, Bad> {
        self.next()
            .ok_or_else(|| self.fail(format!("missing argument to '{name}'")))
    }

    fn number(&mut self, name: &str) -> Result<(Cmp, String), Bad> {
        let text = self.arg(name)?;
        let (cmp, rest) = if let Some(r) = text.strip_prefix('+') {
            (Cmp::More, r)
        } else if let Some(r) = text.strip_prefix('-') {
            (Cmp::Less, r)
        } else {
            (Cmp::Equal, text.as_str())
        };
        Ok((cmp, rest.to_string()))
    }

    fn depth(&mut self, name: &str) -> Result<usize, Bad> {
        let text = self.arg(name)?;
        text.parse().map_err(|_| {
            self.fail(format!(
                "Expected a positive decimal integer argument to {name}, but got '{text}'"
            ))
        })
    }

    fn primary(&mut self) -> Result<Expr, Bad> {
        let Some(token) = self.next() else {
            return Err(self.fail("expected an expression"));
        };
        Ok(match token.as_str() {
            "(" => {
                let inner = self.comma()?;
                if self.next().as_deref() != Some(")") {
                    return Err(self.fail("invalid expression; I was expecting to find a ')' somewhere but did not see one."));
                }
                inner
            }
            "-name" => Expr::Name(self.arg(&token)?, false),
            "-iname" => Expr::Name(self.arg(&token)?, true),
            "-path" | "-wholename" => Expr::Path(self.arg(&token)?, false),
            "-ipath" | "-iwholename" => Expr::Path(self.arg(&token)?, true),
            "-type" => {
                let kinds = self.arg(&token)?;
                let list: Vec<char> = kinds.split(',').filter_map(|k| k.chars().next()).collect();
                if let Some(bad) = list.iter().find(|c| !"fdlbcps".contains(**c)) {
                    return Err(self.fail(format!("Unknown argument to -type: {bad}")));
                }
                Expr::Type(list)
            }
            "-empty" => Expr::Empty,
            "-size" => {
                let (cmp, text) = self.number(&token)?;
                let (digits, unit) = match text.char_indices().last() {
                    Some((at, c)) if c.is_ascii_alphabetic() => {
                        (text.get(..at).unwrap_or(""), Some(c))
                    }
                    _ => (text.as_str(), None),
                };
                let unit = match unit {
                    None | Some('b') => 512,
                    Some('c') => 1,
                    Some('w') => 2,
                    Some('k') => 1024,
                    Some('M') => 1 << 20,
                    Some('G') => 1 << 30,
                    Some(_) => return Err(self.fail(format!("invalid -size type in '{text}'"))),
                };
                let n = digits
                    .parse()
                    .map_err(|_| self.fail(format!("invalid argument '{text}' to '-size'")))?;
                Expr::Size(cmp, n, unit)
            }
            "-mtime" | "-mmin" => {
                let (cmp, text) = self.number(&token)?;
                let n = text
                    .parse()
                    .map_err(|_| self.fail(format!("invalid argument '{text}' to '{token}'")))?;
                Expr::Age(cmp, n, if token == "-mtime" { 86_400 } else { 60 })
            }
            "-newer" => {
                let reference = self.arg(&token)?;
                match self.ctx.host.metadata(&self.ctx.resolve(&reference)) {
                    Ok(m) => Expr::Newer(m.modified),
                    Err(e) => {
                        self.ctx.error(format!("'{reference}': {}", describe(&e)));
                        return Err(Bad(1));
                    }
                }
            }
            "-maxdepth" => {
                self.maxdepth = Some(self.depth(&token)?);
                Expr::Const(true)
            }
            "-mindepth" => {
                self.mindepth = self.depth(&token)?;
                Expr::Const(true)
            }
            "-depth" | "-d" => {
                self.depth_first = true;
                Expr::Const(true)
            }
            "-true" | "-noleaf" | "-xdev" | "-mount" | "-ignore_readdir_race" => Expr::Const(true),
            "-false" => Expr::Const(false),
            "-print" => Expr::Print,
            "-print0" => Expr::Print0,
            "-prune" => Expr::Prune,
            "-quit" => Expr::Quit,
            "-delete" => {
                // Deleting implies `-depth`: a directory's entries go first.
                self.depth_first = true;
                Expr::Delete
            }
            "-exec" | "-execdir" | "-ok" | "-okdir" => self.exec(&token)?,
            other if other.starts_with('-') => {
                return Err(Bad(self.ctx.unsupported(other)));
            }
            ")" => return Err(self.fail("invalid expression; you have too many ')'")),
            other => {
                return Err(self.fail(format!("paths must precede expression: '{other}'")));
            }
        })
    }
}

impl Parser<'_, '_> {
    /// The command of `-exec` and its kind, up to its `;` (or its `{} +`).
    fn exec(&mut self, token: &str) -> Result<Expr, Bad> {
        let ask = token.starts_with("-ok");
        let mut argv = Vec::new();
        let batch = loop {
            let Some(t) = self.next() else {
                return Err(self.fail(format!("missing argument to `{token}'")));
            };
            if t == ";" {
                break false;
            }
            if t == "+" && !ask && argv.last().is_some_and(|l| l == "{}") {
                argv.pop();
                break true;
            }
            argv.push(t);
        };
        if argv.is_empty() {
            return Err(self.fail(format!("missing argument to `{token}'")));
        }
        if batch && argv.iter().any(|a| a.contains("{}")) {
            return Err(self.fail(format!(
                "only one instance of {{}} is supported with {token} ... +"
            )));
        }
        self.execs.push(Exec {
            argv,
            batch,
            in_dir: token.ends_with("dir"),
            ask,
            pending: Vec::new(),
            pending_dir: None,
            pending_len: 0,
        });
        Ok(Expr::Exec(self.execs.len().saturating_sub(1)))
    }
}

struct Walk<'c, 'a> {
    ctx: &'c Ctx<'a>,
    expr: Expr,
    maxdepth: Option<usize>,
    mindepth: usize,
    depth_first: bool,
    status: i32,
    quit: bool,
    execs: Vec<Exec>,
    /// What `-print` printed, for an agent: rendered at the end.
    names: Option<Names>,
}

/// What the expression is evaluated against.
struct Node<'p> {
    shown: &'p str,
    path: &'p Path,
    meta: &'p Metadata,
}

pub fn find(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let mut rest = args;
    // `-P` (never follow symlinks) is how this walks anyway.
    while let Some((first, tail)) = rest.split_first() {
        match first.as_str() {
            "-P" => rest = tail,
            "-H" | "-L" => return ctx.unsupported(first),
            "--" => {
                rest = tail;
                break;
            }
            _ => break,
        }
    }
    let split = rest
        .iter()
        .position(|a| a.starts_with('-') && a.len() > 1 || a == "(" || a == "!")
        .unwrap_or(rest.len());
    let (starts, tokens) = rest.split_at(split);
    let mut parser = Parser {
        ctx,
        tokens: tokens.to_vec(),
        at: 0,
        maxdepth: None,
        mindepth: 0,
        depth_first: false,
        execs: Vec::new(),
    };
    let expr = if tokens.is_empty() {
        Expr::Const(true)
    } else {
        match parser.comma() {
            Ok(e) => e,
            Err(Bad(code)) => return code,
        }
    };
    if let Some(extra) = parser.peek() {
        ctx.error(format!("unexpected '{extra}'"));
        return 2;
    }
    let expr = if expr.has_action() {
        expr
    } else {
        Expr::And(Box::new(expr), Box::new(Expr::Print))
    };
    let mut walk = Walk {
        ctx,
        expr,
        maxdepth: parser.maxdepth,
        mindepth: parser.mindepth,
        depth_first: parser.depth_first,
        status: 0,
        quit: false,
        execs: std::mem::take(&mut parser.execs),
        names: (ctx.render == Render::Agent).then(|| Names::new(NAMES)),
    };
    let defaults = [String::from(".")];
    let starts = if starts.is_empty() {
        &defaults[..]
    } else {
        starts
    };
    for start in starts {
        let path = ctx.resolve(start);
        match ctx.host.symlink_metadata(&path) {
            Ok(meta) => {
                if let Err(Stop(code)) = walk.visit(start, &path, &meta, 0) {
                    return code;
                }
            }
            Err(e) => {
                ctx.error(format!("'{start}': {}", describe(&e)));
                walk.status = 1;
            }
        }
        if walk.quit {
            break;
        }
    }
    // What `+` has collected runs at the end, `-quit` or not.
    for i in 0..walk.execs.len() {
        if let Err(Stop(code)) = walk.flush(i) {
            return code;
        }
    }
    if let Some(names) = &walk.names
        && let Err(Stop(code)) = ctx.out_str(&names.render("names"))
    {
        return code;
    }
    walk.status
}

impl Walk<'_, '_> {
    fn visit(
        &mut self,
        shown: &str,
        path: &Path,
        meta: &Metadata,
        depth: usize,
    ) -> Result<(), Stop> {
        let node = Node { shown, path, meta };
        let selected = depth >= self.mindepth;
        let mut prune = false;
        if selected && !self.depth_first {
            let expr = std::mem::replace(&mut self.expr, Expr::Const(true));
            let result = self.eval(&expr, &node, &mut prune);
            self.expr = expr;
            result?;
        }
        let descend =
            meta.is_dir() && !prune && !self.quit && self.maxdepth.is_none_or(|m| depth < m);
        if descend {
            match self.ctx.host.read_dir(path) {
                Ok(mut entries) => {
                    entries.sort_by(|a, b| a.name.cmp(&b.name));
                    for entry in entries {
                        let child: PathBuf = path.join(&entry.name);
                        let child_shown = if shown.ends_with('/') {
                            format!("{shown}{}", entry.name)
                        } else {
                            format!("{shown}/{}", entry.name)
                        };
                        let Ok(child_meta) = self.ctx.host.symlink_metadata(&child) else {
                            continue;
                        };
                        self.visit(&child_shown, &child, &child_meta, depth.saturating_add(1))?;
                        if self.quit {
                            return Ok(());
                        }
                    }
                }
                Err(e) => {
                    self.ctx.error(format!("'{shown}': {}", describe(&e)));
                    self.status = 1;
                }
            }
        }
        if selected && self.depth_first && !self.quit {
            let expr = std::mem::replace(&mut self.expr, Expr::Const(true));
            let result = self.eval(&expr, &node, &mut prune);
            self.expr = expr;
            result?;
        }
        Ok(())
    }

    fn eval(&mut self, expr: &Expr, n: &Node<'_>, prune: &mut bool) -> Result<bool, Stop> {
        Ok(match expr {
            Expr::And(a, b) => self.eval(a, n, prune)? && self.eval(b, n, prune)?,
            Expr::Or(a, b) => self.eval(a, n, prune)? || self.eval(b, n, prune)?,
            Expr::Comma(a, b) => {
                self.eval(a, n, prune)?;
                self.eval(b, n, prune)?
            }
            Expr::Not(a) => !self.eval(a, n, prune)?,
            Expr::Const(v) => *v,
            Expr::Name(pattern, fold) => glob(pattern, &base_name(n.shown), *fold),
            Expr::Path(pattern, fold) => glob(pattern, n.shown, *fold),
            Expr::Type(kinds) => kinds.iter().any(|k| {
                matches!(
                    (k, n.meta.kind),
                    ('f', Kind::File) | ('d', Kind::Dir) | ('l', Kind::Symlink)
                )
            }),
            Expr::Empty => match n.meta.kind {
                Kind::File => n.meta.len == 0,
                Kind::Dir => self.ctx.host.read_dir(n.path).is_ok_and(|e| e.is_empty()),
                _ => false,
            },
            Expr::Size(cmp, size, unit) => {
                let units = n.meta.len.div_ceil(*unit);
                compare(*cmp, units, *size)
            }
            Expr::Newer(at) => n.meta.modified > *at,
            Expr::Age(cmp, count, unit) => {
                let age = self.ctx.host.now().saturating_sub(n.meta.modified);
                let units = age.div_euclid(*unit);
                match cmp {
                    Cmp::Less => units < *count,
                    Cmp::Equal => units == *count,
                    Cmp::More => units > *count,
                }
            }
            Expr::Print => {
                if let Some(names) = &mut self.names {
                    let marker = match n.meta.kind {
                        Kind::Dir if !n.shown.ends_with('/') => "/",
                        Kind::Symlink => "@",
                        _ => "",
                    };
                    names.add_path(n.shown, marker);
                } else {
                    self.ctx.out_str(&format!("{}\n", n.shown))?;
                }
                true
            }
            Expr::Print0 => {
                self.ctx.out_str(&format!("{}\0", n.shown))?;
                true
            }
            Expr::Prune => {
                *prune = true;
                true
            }
            Expr::Quit => {
                self.quit = true;
                true
            }
            Expr::Exec(i) => self.exec(*i, n)?,
            Expr::Delete => self.delete(n),
        })
    }

    /// Where `-execdir` runs for `shown`, and what it calls it: the
    /// directory the name is in, and `./name`.
    fn in_dir(&self, shown: &str) -> (PathBuf, String) {
        let (dir, name) = match shown.trim_end_matches('/').rsplit_once('/') {
            Some(("", name)) => ("/", name),
            Some((dir, name)) => (dir, name),
            None => (".", shown),
        };
        (self.ctx.resolve(dir), format!("./{name}"))
    }

    /// Run a command's status as `find` takes it: a stop ends the walk, a
    /// command that could not run makes the status 1.
    fn ran(&mut self, status: i32) -> Result<i32, Stop> {
        match status {
            130 | 141 => Err(Stop(status)),
            126 | 127 => {
                self.status = 1;
                Ok(status)
            }
            other => Ok(other),
        }
    }

    fn exec(&mut self, i: usize, n: &Node<'_>) -> Result<bool, Stop> {
        let ctx = self.ctx;
        let (cwd, name) = match self.execs.get(i) {
            Some(spec) if spec.in_dir => self.in_dir(n.shown),
            Some(_) => (ctx.cwd.to_path_buf(), n.shown.to_string()),
            None => return Ok(false),
        };
        let Some(spec) = self.execs.get_mut(i) else {
            return Ok(false);
        };
        if spec.batch {
            let other_dir = spec.in_dir && spec.pending_dir.as_ref().is_some_and(|d| *d != cwd);
            let full = spec
                .pending_len
                .saturating_add(name.len())
                .saturating_add(1)
                > BATCH_BYTES;
            if !spec.pending.is_empty() && (other_dir || full) {
                self.flush(i)?;
            }
            if let Some(spec) = self.execs.get_mut(i) {
                spec.pending_len = spec
                    .pending_len
                    .saturating_add(name.len())
                    .saturating_add(1);
                spec.pending.push(name);
                spec.pending_dir = Some(cwd);
            }
            return Ok(true);
        }
        let argv: Vec<String> = spec.argv.iter().map(|a| a.replace("{}", &name)).collect();
        let status = if spec.ask {
            let command = argv.first().map_or("", String::as_str);
            if !ctx
                .dispatch
                .confirm(&format!("< {command} ... {name} > ? "))
            {
                return Ok(false);
            }
            // The answer came from the terminal; the command reads nothing.
            let io = Stdio {
                stdin: Input::null(),
                stdout: ctx.io.stdout.clone(),
                stderr: ctx.io.stderr.clone(),
            };
            ctx.dispatch.run(&argv, &cwd, &io)
        } else {
            ctx.dispatch.run(&argv, &cwd, ctx.io)
        };
        Ok(self.ran(status)? == 0)
    }

    /// Run what `-exec … {} +` has collected for command `i`.
    fn flush(&mut self, i: usize) -> Result<(), Stop> {
        let ctx = self.ctx;
        let Some(spec) = self.execs.get_mut(i) else {
            return Ok(());
        };
        if spec.pending.is_empty() {
            return Ok(());
        }
        let mut argv = spec.argv.clone();
        argv.append(&mut spec.pending);
        spec.pending_len = 0;
        let cwd = spec
            .pending_dir
            .take()
            .unwrap_or_else(|| ctx.cwd.to_path_buf());
        let status = ctx.dispatch.run(&argv, &cwd, ctx.io);
        // GNU: any batch that fails makes find's status non-zero.
        if self.ran(status)? != 0 {
            self.status = 1;
        }
        Ok(())
    }

    /// `-delete`: the environment's `rm` for a file, `rmdir` for a
    /// directory (its entries went first, as `-delete` implies `-depth`).
    fn delete(&mut self, n: &Node<'_>) -> bool {
        // GNU never deletes the starting point `.`.
        if n.shown == "." {
            return true;
        }
        let action = if n.meta.is_dir() { "rmdir" } else { "rm" };
        if self.ctx.file_action(action, &[n.shown.to_string()]) == 0 {
            true
        } else {
            self.status = 1;
            false
        }
    }
}

fn compare(cmp: Cmp, value: u64, against: u64) -> bool {
    match cmp {
        Cmp::Less => value < against,
        Cmp::Equal => value == against,
        Cmp::More => value > against,
    }
}
