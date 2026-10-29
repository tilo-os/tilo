//! `ls`: list directories as GNU `ls` does, reading through the host.
//!
//! One name per line when the output is not the host's own terminal, and
//! GNU's column layout (tab stops every eight) when it is. `-l` prints the
//! long format with names for users and groups when the host knows them.
//!
//! For an agent, each directory is one line, `src/: lib.rs main.rs bin/`,
//! marked as `-F` marks names; under `-l`, a line per entry under its
//! directory's, `  -rw-r--r-- 1234 2026-10-29 12:00 lib.rs`, with the exact
//! size and the time as a date. Past [`NAMES`](super::agent::NAMES) names
//! it counts, and says how many it left out.

use std::cmp::Ordering;
use std::path::PathBuf;

use super::agent::{self, Lines, Names};
use super::util::{Arg, Spec, human, local, month_name, parse};
use super::{Ctx, Render, Stop, describe};
use crate::host::{Kind, Metadata};
use crate::io::Std;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Name,
    Time,
    Size,
    None,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    OnePerLine,
    Columns,
    Long,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Indicator {
    None,
    Slash,
    Classify,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hidden {
    Skip,
    AlmostAll,
    All,
}

#[expect(clippy::struct_excessive_bools, reason = "one switch per GNU flag")]
struct Opts {
    hidden: Hidden,
    format: Format,
    human: bool,
    recursive: bool,
    directory: bool,
    sort: Sort,
    reverse: bool,
    indicator: Indicator,
    numeric: bool,
    width: usize,
}

/// One name to list.
struct Entry {
    /// The name as printed: the operand as given, or the name in its
    /// directory.
    name: String,
    path: PathBuf,
    meta: Metadata,
    /// A symlink's target, for `-l`.
    target: Option<String>,
    /// What a symlink points at, for `-F` in the long format.
    target_meta: Option<Metadata>,
}

/// Where a listing goes: GNU's text, or one of the agent's forms.
enum Out<'c, 'a> {
    Text,
    Names(Names),
    Long(Lines<'c, 'a>),
}

const LONGS: &[(&str, bool)] = &[
    ("all", false),
    ("almost-all", false),
    ("human-readable", false),
    ("recursive", false),
    ("directory", false),
    ("reverse", false),
    ("classify", false),
    ("color", false),
    ("colour", false),
    ("numeric-uid-gid", false),
];

pub fn ls(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let parsed = match parse(ctx, args, &Spec::new("", LONGS)) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let terminal = ctx.io.stdout.host_stream() == Some(Std::Out);
    let mut o = Opts {
        hidden: Hidden::Skip,
        format: if terminal {
            Format::Columns
        } else {
            Format::OnePerLine
        },
        human: false,
        recursive: false,
        directory: false,
        sort: Sort::Name,
        reverse: false,
        indicator: Indicator::None,
        numeric: false,
        width: ctx
            .env
            .iter()
            .find(|(k, _)| k == "COLUMNS")
            .and_then(|(_, v)| v.parse().ok())
            .filter(|&w: &usize| w > 0)
            .unwrap_or(80),
    };
    let mut operands = Vec::new();
    for arg in parsed {
        let (flag, v) = match arg {
            Arg::Operand(op) => {
                operands.push(op);
                continue;
            }
            Arg::Flag(f, v) => (f, v),
        };
        match flag.as_str() {
            "-a" | "--all" => o.hidden = Hidden::All,
            "-A" | "--almost-all" => o.hidden = Hidden::AlmostAll,
            "-1" => o.format = Format::OnePerLine,
            "-C" => o.format = Format::Columns,
            "-l" => o.format = Format::Long,
            "-n" | "--numeric-uid-gid" => {
                o.format = Format::Long;
                o.numeric = true;
            }
            "-h" | "--human-readable" => o.human = true,
            "-R" | "--recursive" => o.recursive = true,
            "-d" | "--directory" => o.directory = true,
            "-t" => o.sort = Sort::Time,
            "-S" => o.sort = Sort::Size,
            "-U" => o.sort = Sort::None,
            "-r" | "--reverse" => o.reverse = true,
            "-F" | "--classify" => o.indicator = Indicator::Classify,
            "-p" => o.indicator = Indicator::Slash,
            "--color" | "--colour" => {
                let plain = matches!(
                    v.as_deref(),
                    Some("never" | "no" | "none" | "auto" | "tty" | "if-tty")
                );
                if !plain {
                    return ctx.unsupported(&flag);
                }
            }
            other => return ctx.unsupported(other),
        }
    }
    let explicit = !operands.is_empty();
    if operands.is_empty() {
        operands.push(".".into());
    }
    let mut out = match (ctx.render, o.format) {
        (Render::Text, _) => Out::Text,
        (Render::Agent, Format::Long) => Out::Long(Lines::new(ctx, agent::NAMES)),
        (Render::Agent, _) => Out::Names(Names::new(agent::NAMES)),
    };
    let result = run(ctx, &o, &operands, explicit, &mut out).and_then(|status| {
        match &out {
            Out::Text => {}
            Out::Names(names) => ctx.out_str(&names.render("names"))?,
            Out::Long(lines) => lines.finish("entries", ("directory", "directories"))?,
        }
        Ok(status)
    });
    match result {
        Ok(status) => status,
        Err(Stop(code)) => code,
    }
}

fn run<'c, 'a>(
    ctx: &'c Ctx<'a>,
    o: &Opts,
    operands: &[String],
    explicit: bool,
    out: &mut Out<'c, 'a>,
) -> Result<i32, Stop> {
    let mut status = 0;
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    // GNU follows a symlink named on the command line unless the listing is
    // about the names themselves (`-l`, `-d`, `-F`).
    let follow = !(o.format == Format::Long || o.directory || o.indicator == Indicator::Classify);
    for op in operands {
        let path = ctx.resolve(op);
        let meta = if follow {
            ctx.host
                .metadata(&path)
                .or_else(|_| ctx.host.symlink_metadata(&path))
        } else {
            ctx.host.symlink_metadata(&path)
        };
        let meta = match meta {
            Ok(m) => m,
            Err(e) => {
                ctx.error(format!("cannot access '{op}': {}", describe(&e)));
                status = 2;
                continue;
            }
        };
        let to_dir = !o.directory
            && (meta.is_dir()
                || (meta.kind == Kind::Symlink
                    && o.format != Format::Long
                    && ctx.host.metadata(&path).is_ok_and(|m| m.is_dir())));
        let entry = entry(ctx, op.clone(), path, meta);
        if to_dir {
            dirs.push(entry);
        } else {
            files.push(entry);
        }
    }
    sort(&mut files, o);
    sort(&mut dirs, o);
    let mut printed = false;
    if !files.is_empty() {
        show(ctx, o, out, None, &files)?;
        printed = true;
    }
    let headers = explicit && (operands.len() > 1 || !files.is_empty()) || o.recursive;
    for dir in &dirs {
        if list_dir(ctx, o, out, &dir.name, &dir.path, headers, &mut printed)? {
            status = status.max(2);
        }
    }
    Ok(status)
}

fn entry(ctx: &Ctx<'_>, name: String, path: PathBuf, meta: Metadata) -> Entry {
    let (target, target_meta) = if meta.kind == Kind::Symlink {
        (
            ctx.host
                .read_link(&path)
                .ok()
                .map(|t| t.to_string_lossy().into_owned()),
            ctx.host.metadata(&path).ok(),
        )
    } else {
        (None, None)
    };
    Entry {
        name,
        path,
        meta,
        target,
        target_meta,
    }
}

/// List one directory, and under `-R` every directory below it. Answers
/// whether the directory itself could not be read.
fn list_dir<'c, 'a>(
    ctx: &'c Ctx<'a>,
    o: &Opts,
    out: &mut Out<'c, 'a>,
    shown: &str,
    path: &std::path::Path,
    header: bool,
    printed: &mut bool,
) -> Result<bool, Stop> {
    let listing = ctx.host.read_dir(path);
    let text = matches!(out, Out::Text);
    if text && *printed && header {
        ctx.out_str("\n")?;
    }
    if text && header {
        ctx.out_str(&format!("{shown}:\n"))?;
    }
    *printed = true;
    let listing = match listing {
        Ok(l) => l,
        Err(e) => {
            ctx.error(format!("cannot open directory '{shown}': {}", describe(&e)));
            return Ok(true);
        }
    };
    let mut entries = Vec::new();
    if o.hidden == Hidden::All {
        for dot in [".", ".."] {
            let p = crate::paths::normalize(&path.join(dot));
            if let Ok(meta) = ctx.host.metadata(&p) {
                entries.push(entry(ctx, dot.into(), p, meta));
            }
        }
    }
    for item in listing {
        if item.name.starts_with('.') && o.hidden == Hidden::Skip {
            continue;
        }
        let p = path.join(&item.name);
        let Ok(meta) = ctx.host.symlink_metadata(&p) else {
            continue;
        };
        entries.push(entry(ctx, item.name, p, meta));
    }
    sort(&mut entries, o);
    if !text {
        show(ctx, o, out, Some(shown), &entries)?;
    } else if o.format == Format::Long {
        let blocks: u64 = entries
            .iter()
            .map(|e| blocks(&e.meta))
            .fold(0, u64::saturating_add)
            / 2;
        let total = if o.human {
            human(blocks.saturating_mul(1024))
        } else {
            blocks.to_string()
        };
        ctx.out_str(&format!("total {total}\n"))?;
    }
    if text {
        print_group(ctx, o, &entries)?;
    }
    if o.recursive {
        for e in &entries {
            if e.meta.is_dir() && e.name != "." && e.name != ".." {
                let sub = if shown.ends_with('/') {
                    format!("{shown}{}", e.name)
                } else {
                    format!("{shown}/{}", e.name)
                };
                list_dir(ctx, o, out, &sub, &e.path, true, printed)?;
            }
        }
    }
    Ok(false)
}

fn sort(entries: &mut [Entry], o: &Opts) {
    if o.sort == Sort::None {
        return;
    }
    entries.sort_by(|a, b| {
        let by_name = a.name.as_bytes().cmp(b.name.as_bytes());
        let primary = match o.sort {
            Sort::Time => b.meta.modified.cmp(&a.meta.modified),
            Sort::Size => b.meta.len.cmp(&a.meta.len),
            Sort::Name | Sort::None => Ordering::Equal,
        };
        let order = primary.then(by_name);
        if o.reverse { order.reverse() } else { order }
    });
}

/// 512-byte blocks a file takes, as a file system with 4 KiB blocks would
/// give them: hosts report sizes, not allocations.
pub fn blocks(meta: &Metadata) -> u64 {
    if meta.kind == Kind::Symlink {
        return 0;
    }
    meta.len.div_ceil(4096).saturating_mul(8)
}

fn indicator(o: &Opts, meta: &Metadata) -> &'static str {
    match (o.indicator, meta.kind) {
        (Indicator::None, _) => "",
        (_, Kind::Dir) => "/",
        (Indicator::Classify, Kind::Symlink) => "@",
        (Indicator::Classify, Kind::File) if meta.mode & 0o111 != 0 => "*",
        (Indicator::Slash | Indicator::Classify, _) => "",
    }
}

/// The marker an agent sees after a name: `ls -F`'s, always.
fn agent_marker(meta: &Metadata) -> &'static str {
    match meta.kind {
        Kind::Dir => "/",
        Kind::Symlink => "@",
        Kind::File if meta.mode & 0o111 != 0 => "*",
        _ => "",
    }
}

/// One group of entries: the operands that are files (`dir` none), or a
/// directory's entries.
fn show(
    ctx: &Ctx<'_>,
    o: &Opts,
    out: &mut Out<'_, '_>,
    dir: Option<&str>,
    entries: &[Entry],
) -> Result<(), Stop> {
    match out {
        Out::Text => print_group(ctx, o, entries),
        Out::Names(names) => {
            if dir.is_some() {
                names.open(dir);
            }
            for e in entries {
                names.add(dir, &e.name, agent_marker(&e.meta));
            }
            Ok(())
        }
        Out::Long(lines) => {
            lines.group(dir.map(|d| format!("{}:", agent::header(d))));
            let indent = if dir.is_some() { "  " } else { "" };
            for e in entries {
                let t = local(ctx, e.meta.modified);
                let mut name = format!("{}{}", agent::quote(&e.name), agent_marker(&e.meta));
                if let Some(target) = &e.target {
                    name.push_str(" -> ");
                    name.push_str(&agent::quote(target));
                }
                lines.line(&format!(
                    "{indent}{} {} {}-{:02}-{:02} {:02}:{:02} {name}",
                    mode_string(e.meta.kind, e.meta.mode),
                    e.meta.len,
                    t.year,
                    t.month,
                    t.day,
                    t.hour,
                    t.minute
                ))?;
            }
            Ok(())
        }
    }
}

fn print_group(ctx: &Ctx<'_>, o: &Opts, entries: &[Entry]) -> Result<(), Stop> {
    match o.format {
        Format::Long => print_long(ctx, o, entries),
        Format::OnePerLine => {
            for e in entries {
                ctx.out_str(&format!("{}{}\n", e.name, indicator(o, &e.meta)))?;
            }
            Ok(())
        }
        Format::Columns => {
            let names: Vec<String> = entries
                .iter()
                .map(|e| format!("{}{}", e.name, indicator(o, &e.meta)))
                .collect();
            print_columns(ctx, &names, o.width)
        }
    }
}

/// GNU's vertical column layout: as many columns as fit in fewer than
/// `width` characters, each as wide as its widest name plus two, padded
/// with tabs at every eighth column the way GNU `ls` pads.
fn print_columns(ctx: &Ctx<'_>, names: &[String], width: usize) -> Result<(), Stop> {
    let n = names.len();
    if n == 0 {
        return Ok(());
    }
    let lens: Vec<usize> = names.iter().map(|s| s.chars().count()).collect();
    let max_cols = n.min((width / 3).max(1));
    let mut best: (usize, Vec<usize>) = (1, vec![lens.iter().copied().max().unwrap_or(0)]);
    for cols in 2..=max_cols {
        let rows = n.div_ceil(cols);
        let mut widths = vec![0_usize; cols];
        let mut line = 0_usize;
        for (k, &len) in lens.iter().enumerate() {
            let col = k.checked_div(rows).unwrap_or(0);
            let real = len.saturating_add(if col == cols.saturating_sub(1) { 0 } else { 2 });
            if let Some(w) = widths.get_mut(col)
                && *w < real
            {
                line = line.saturating_add(real.saturating_sub(*w));
                *w = real;
            }
        }
        if line < width {
            best = (cols, widths);
        }
    }
    let rows = n.div_ceil(best.0);
    for row in 0..rows {
        let mut out = String::new();
        let mut pos = 0_usize;
        let mut col = 0_usize;
        let mut k = row;
        while let (Some(name), Some(&len)) = (names.get(k), lens.get(k)) {
            out.push_str(name);
            k = k.saturating_add(rows);
            if k >= n {
                break;
            }
            let w = best.1.get(col).copied().unwrap_or(0);
            indent(&mut out, pos.saturating_add(len), pos.saturating_add(w));
            pos = pos.saturating_add(w);
            col = col.saturating_add(1);
        }
        out.push('\n');
        ctx.out_str(&out)?;
    }
    Ok(())
}

/// Pad from column `from` to `to` with tabs where a tab stop fits, then
/// spaces, as GNU `ls` does with its default tab size of eight.
fn indent(out: &mut String, mut from: usize, to: usize) {
    while from < to {
        if to / 8 > from.saturating_add(1) / 8 {
            out.push('\t');
            from = from.saturating_add(8_usize.saturating_sub(from % 8));
        } else {
            out.push(' ');
            from = from.saturating_add(1);
        }
    }
}

/// `-rw-r--r--`: a mode as `ls -l` and `stat` print it.
pub fn mode_string(kind: Kind, mode: u32) -> String {
    let mut s = String::with_capacity(10);
    s.push(match kind {
        Kind::Dir => 'd',
        Kind::Symlink => 'l',
        Kind::File => '-',
        Kind::Other => '?',
    });
    let bit = |m: u32| mode & m != 0;
    for (shift, special, lower, upper) in [
        (6_u32, 0o4000, 's', 'S'),
        (3, 0o2000, 's', 'S'),
        (0, 0o1000, 't', 'T'),
    ] {
        s.push(if bit(0o4 << shift) { 'r' } else { '-' });
        s.push(if bit(0o2 << shift) { 'w' } else { '-' });
        let exec = bit(0o1 << shift);
        s.push(match (bit(special), exec) {
            (true, true) => lower,
            (true, false) => upper,
            (false, true) => 'x',
            (false, false) => '-',
        });
    }
    s
}

pub fn user(ctx: &Ctx<'_>, uid: u32) -> String {
    ctx.host.user_name(uid).unwrap_or_else(|| uid.to_string())
}

pub fn group(ctx: &Ctx<'_>, gid: u32) -> String {
    ctx.host.group_name(gid).unwrap_or_else(|| gid.to_string())
}

/// The date column of `ls -l`: time of day within six months of now, the
/// year otherwise.
fn date(ctx: &Ctx<'_>, at: i64) -> String {
    const SIX_MONTHS: i64 = 31_556_952 / 2;
    let now = ctx.host.now();
    let t = local(ctx, at);
    let recent = now.saturating_sub(SIX_MONTHS) < at && at <= now;
    if recent {
        format!(
            "{} {:>2} {:02}:{:02}",
            month_name(t.month),
            t.day,
            t.hour,
            t.minute
        )
    } else {
        format!("{} {:>2}  {}", month_name(t.month), t.day, t.year)
    }
}

fn print_long(ctx: &Ctx<'_>, o: &Opts, entries: &[Entry]) -> Result<(), Stop> {
    let rows: Vec<[String; 5]> = entries
        .iter()
        .map(|e| {
            let (owner, grp) = if o.numeric {
                (e.meta.uid.to_string(), e.meta.gid.to_string())
            } else {
                (user(ctx, e.meta.uid), group(ctx, e.meta.gid))
            };
            let size = if o.human {
                human(e.meta.len)
            } else {
                e.meta.len.to_string()
            };
            [
                mode_string(e.meta.kind, e.meta.mode),
                e.meta.nlink.to_string(),
                owner,
                grp,
                size,
            ]
        })
        .collect();
    let width = |i: usize| {
        rows.iter()
            .map(|r| r.get(i).map_or(0, |s| s.chars().count()))
            .max()
            .unwrap_or(0)
    };
    let (wn, wu, wg, ws) = (width(1), width(2), width(3), width(4));
    for (e, [mode, nlink, owner, grp, size]) in entries.iter().zip(&rows) {
        let mut name = e.name.clone();
        match (&e.target, e.meta.kind) {
            (Some(target), Kind::Symlink) => {
                name.push_str(" -> ");
                name.push_str(target);
                if let Some(m) = &e.target_meta {
                    name.push_str(indicator(o, m));
                }
            }
            _ => name.push_str(indicator(o, &e.meta)),
        }
        ctx.out_str(&format!(
            "{mode} {nlink:>wn$} {owner:<wu$} {grp:<wg$} {size:>ws$} {} {name}\n",
            date(ctx, e.meta.modified)
        ))?;
    }
    Ok(())
}
