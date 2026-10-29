//! The tier-1 commands that read and write text: `cat`, `head`, `tail`,
//! `wc`, `nl`, `rev`, `tee`, `seq`, `yes`, `sleep`, `env` and `printenv`
//! here, and `grep`, `diff`, `sort`/`uniq` and `cut`/`tr` in their own
//! modules.

use std::io::{self, Read, Write};

use super::util::{Arg, Spec, chomp, lines, open, parse, parse_size, split_lines, value};
use super::{Command, Ctx, Stop};
use crate::host::WriteMode;

pub const NAMES: &[&str] = &[
    "cat", "cut", "diff", "env", "grep", "head", "nl", "printenv", "rev", "seq", "sleep", "sort",
    "tail", "tee", "tr", "uniq", "wc", "yes",
];

pub fn lookup(name: &str) -> Option<Command> {
    Some(match name {
        "cat" => cat,
        "head" => head,
        "tail" => tail,
        "wc" => wc,
        "nl" => nl,
        "rev" => rev,
        "tee" => tee,
        "seq" => seq,
        "yes" => yes,
        "sleep" => sleep,
        "env" => env,
        "printenv" => printenv,
        "grep" => super::grep::grep,
        "diff" => super::diff::diff,
        "sort" => super::sort::sort,
        "uniq" => super::sort::uniq,
        "cut" => super::cut::cut,
        "tr" => super::cut::tr,
        _ => return None,
    })
}

/// The operands, or standard input when there are none.
fn or_stdin(operands: Vec<String>) -> Vec<String> {
    if operands.is_empty() {
        vec!["-".into()]
    } else {
        operands
    }
}

/// The result of a command body: its status, or the stop that ended it.
fn finish(result: Result<i32, Stop>) -> i32 {
    match result {
        Ok(status) | Err(Stop(status)) => status,
    }
}

#[derive(Default)]
#[expect(clippy::struct_excessive_bools, reason = "one switch per GNU flag")]
struct CatOpts {
    number: bool,
    nonblank: bool,
    squeeze: bool,
    ends: bool,
    tabs: bool,
    nonprinting: bool,
}

fn cat(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let spec = Spec::new(
        "",
        &[
            ("number", false),
            ("number-nonblank", false),
            ("squeeze-blank", false),
            ("show-ends", false),
            ("show-tabs", false),
            ("show-nonprinting", false),
            ("show-all", false),
        ],
    );
    let parsed = match parse(ctx, args, &spec) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let mut o = CatOpts::default();
    let mut operands = Vec::new();
    for arg in parsed {
        match arg {
            Arg::Operand(op) => operands.push(op),
            Arg::Flag(f, _) => match f.as_str() {
                "-n" | "--number" => o.number = true,
                "-b" | "--number-nonblank" => o.nonblank = true,
                "-s" | "--squeeze-blank" => o.squeeze = true,
                "-E" | "--show-ends" => o.ends = true,
                "-T" | "--show-tabs" => o.tabs = true,
                "-v" | "--show-nonprinting" => o.nonprinting = true,
                "-A" | "--show-all" => {
                    o.nonprinting = true;
                    o.ends = true;
                    o.tabs = true;
                }
                "-e" => {
                    o.nonprinting = true;
                    o.ends = true;
                }
                "-t" => {
                    o.nonprinting = true;
                    o.tabs = true;
                }
                // GNU accepts and ignores it: output is never buffered here.
                "-u" => {}
                other => return ctx.unsupported(other),
            },
        }
    }
    let plain = !(o.number || o.nonblank || o.squeeze || o.ends || o.tabs || o.nonprinting);
    let mut status = 0;
    let mut line_no = 0_u64;
    let mut prev_blank = false;
    for operand in or_stdin(operands) {
        let mut reader = match open(ctx, &operand) {
            Ok(r) => r,
            Err(e) => {
                ctx.io_error(&operand, &e);
                status = 1;
                continue;
            }
        };
        let result = if plain {
            copy(ctx, &mut reader)
        } else {
            lines(&mut reader, b'\n', |line| {
                let blank = line == b"\n";
                if o.squeeze && blank && prev_blank {
                    return Ok(());
                }
                prev_blank = blank;
                let mut out = Vec::with_capacity(line.len().saturating_add(8));
                if o.nonblank && !blank || o.number && !o.nonblank {
                    line_no = line_no.saturating_add(1);
                    out.extend_from_slice(format!("{line_no:>6}\t").as_bytes());
                }
                let (body, newline) = match line.strip_suffix(b"\n") {
                    Some(body) => (body, true),
                    None => (line, false),
                };
                for &b in body {
                    show_byte(&mut out, b, &o);
                }
                if newline {
                    if o.ends {
                        out.push(b'$');
                    }
                    out.push(b'\n');
                }
                ctx.out(&out).map_err(CatError::Stop)
            })
        };
        match result {
            Ok(()) => {}
            Err(CatError::Stop(Stop(code))) => return code,
            Err(CatError::Io(e)) => {
                ctx.io_error(&operand, &e);
                status = 1;
            }
        }
    }
    status
}

/// A copy stopped by its reader or by its writer.
enum CatError {
    Stop(Stop),
    Io(io::Error),
}

impl From<io::Error> for CatError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

fn copy(ctx: &Ctx<'_>, reader: &mut dyn Read) -> Result<(), CatError> {
    let mut buf = vec![0_u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        ctx.out(buf.get(..n).unwrap_or_default())
            .map_err(CatError::Stop)?;
    }
}

/// One byte as `cat -v`/`-T` show it.
fn show_byte(out: &mut Vec<u8>, b: u8, o: &CatOpts) {
    if b == b'\t' {
        if o.tabs {
            out.extend_from_slice(b"^I");
        } else {
            out.push(b);
        }
        return;
    }
    if !o.nonprinting {
        out.push(b);
        return;
    }
    let low = if b >= 128 {
        out.extend_from_slice(b"M-");
        b.wrapping_sub(128)
    } else {
        b
    };
    match low {
        0..=31 => {
            out.push(b'^');
            out.push(low.wrapping_add(64));
        }
        127 => out.extend_from_slice(b"^?"),
        _ => out.push(low),
    }
}

/// `-NUM` (and for `tail`, `+NUM`) as the first argument: the old way to
/// say `-n NUM`.
fn obsolete_count(args: &[String], plus: bool) -> Vec<String> {
    let mut out = args.to_vec();
    if let Some(first) = args.first() {
        let digits = first
            .strip_prefix('-')
            .or_else(|| if plus { first.strip_prefix('+') } else { None });
        if let Some(d) = digits
            && !d.is_empty()
            && d.bytes().all(|b| b.is_ascii_digit())
        {
            let n = if first.starts_with('+') {
                format!("+{d}")
            } else {
                d.to_string()
            };
            out.splice(0..1, ["-n".to_string(), n]);
        }
    }
    out
}

/// A `head`/`tail` count: how many, and whether it counts from the other
/// end (`head -n -3`, `tail -n +3`).
struct Count {
    n: u64,
    flipped: bool,
    bytes: bool,
}

fn count_arg(ctx: &Ctx<'_>, text: &str, flip: char, bytes: bool) -> Result<Count, i32> {
    let (flipped, digits) = match text.strip_prefix(flip) {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix(['-', '+']).unwrap_or(text)),
    };
    if let Some(n) = parse_size(digits) {
        Ok(Count { n, flipped, bytes })
    } else {
        let what = if bytes { "bytes" } else { "lines" };
        ctx.error(format!("invalid number of {what}: '{text}'"));
        Err(2)
    }
}

struct HeadTail {
    count: Count,
    headers: Option<bool>,
    delimiter: u8,
    operands: Vec<String>,
}

fn head_tail_args(ctx: &Ctx<'_>, args: &[String], tail: bool) -> Result<HeadTail, i32> {
    let longs: &[(&str, bool)] = &[
        ("lines", true),
        ("bytes", true),
        ("quiet", false),
        ("silent", false),
        ("verbose", false),
        ("zero-terminated", false),
        ("follow", false),
        ("retry", false),
        ("pid", true),
        ("sleep-interval", true),
    ];
    let parsed = parse(ctx, &obsolete_count(args, tail), &Spec::new("ncs", longs))?;
    let flip = if tail { '+' } else { '-' };
    let mut out = HeadTail {
        count: Count {
            n: 10,
            flipped: false,
            bytes: false,
        },
        headers: None,
        delimiter: b'\n',
        operands: Vec::new(),
    };
    for arg in parsed {
        match arg {
            Arg::Operand(op) => out.operands.push(op),
            Arg::Flag(f, v) => match f.as_str() {
                "-n" | "--lines" => out.count = count_arg(ctx, value(v.as_ref()), flip, false)?,
                "-c" | "--bytes" => out.count = count_arg(ctx, value(v.as_ref()), flip, true)?,
                "-q" | "--quiet" | "--silent" => out.headers = Some(false),
                "-v" | "--verbose" => out.headers = Some(true),
                "-z" | "--zero-terminated" => out.delimiter = 0,
                other => return Err(ctx.unsupported(other)),
            },
        }
    }
    Ok(out)
}

fn head(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let a = match head_tail_args(ctx, args, false) {
        Ok(a) => a,
        Err(code) => return code,
    };
    let operands = or_stdin(a.operands);
    let headers = a.headers.unwrap_or(operands.len() > 1);
    let mut status = 0;
    for (i, operand) in operands.iter().enumerate() {
        let reader: Box<dyn Read + Send> = match open(ctx, operand) {
            // Standard input is read unbuffered, so what `head` leaves is
            // still there for the next reader; a file can be read ahead.
            Ok(r) if operand == "-" => r,
            Ok(r) => Box::new(io::BufReader::new(r)),
            Err(e) => {
                ctx.error(format!(
                    "cannot open '{operand}' for reading: {}",
                    super::describe(&e)
                ));
                status = 1;
                continue;
            }
        };
        if headers {
            let gap = if i == 0 { "" } else { "\n" };
            let name = if operand == "-" {
                "standard input"
            } else {
                operand
            };
            if let Err(stop) = ctx.out_str(&format!("{gap}==> {name} <==\n")) {
                return stop.0;
            }
        }
        match head_one(ctx, reader, &a.count, a.delimiter) {
            Ok(()) => {}
            Err(CatError::Stop(stop)) => return stop.0,
            Err(CatError::Io(e)) => {
                ctx.io_error(operand, &e);
                status = 1;
            }
        }
    }
    status
}

fn head_one(
    ctx: &Ctx<'_>,
    mut reader: Box<dyn Read + Send>,
    count: &Count,
    delimiter: u8,
) -> Result<(), CatError> {
    if count.flipped {
        // All but the last N: only knowable once the whole input is read.
        let mut all = Vec::new();
        reader.read_to_end(&mut all)?;
        let n = usize::try_from(count.n).unwrap_or(usize::MAX);
        let keep: Vec<u8> = if count.bytes {
            all.get(..all.len().saturating_sub(n))
                .unwrap_or_default()
                .to_vec()
        } else {
            let lines = split_lines(&all, delimiter);
            lines
                .get(..lines.len().saturating_sub(n))
                .unwrap_or_default()
                .concat()
        };
        return ctx.out(&keep).map_err(CatError::Stop);
    }
    // Read only what is printed, a byte at a time for lines, so a command
    // after this one on the same input reads on from where `head` stopped.
    let mut left = count.n;
    let mut buf = vec![0_u8; 8192];
    while left > 0 {
        let want = if count.bytes {
            usize::try_from(left).unwrap_or(usize::MAX).min(buf.len())
        } else {
            1
        };
        let n = reader.read(buf.get_mut(..want).unwrap_or_default())?;
        if n == 0 {
            break;
        }
        let chunk = buf.get(..n).unwrap_or_default();
        if count.bytes {
            left = left.saturating_sub(n as u64);
        } else if chunk.first() == Some(&delimiter) {
            left = left.saturating_sub(1);
        }
        ctx.out(chunk).map_err(CatError::Stop)?;
    }
    Ok(())
}

fn tail(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let a = match head_tail_args(ctx, args, true) {
        Ok(a) => a,
        Err(code) => return code,
    };
    let operands = or_stdin(a.operands);
    let headers = a.headers.unwrap_or(operands.len() > 1);
    let mut status = 0;
    let mut first = true;
    for operand in &operands {
        let mut all = Vec::new();
        let read = open(ctx, operand).and_then(|mut r| r.read_to_end(&mut all));
        if let Err(e) = read {
            ctx.error(format!(
                "cannot open '{operand}' for reading: {}",
                super::describe(&e)
            ));
            status = 1;
            continue;
        }
        if headers {
            let gap = if first { "" } else { "\n" };
            let name = if operand == "-" {
                "standard input"
            } else {
                operand
            };
            if let Err(stop) = ctx.out_str(&format!("{gap}==> {name} <==\n")) {
                return stop.0;
            }
        }
        first = false;
        let n = usize::try_from(a.count.n).unwrap_or(usize::MAX);
        let out: Vec<u8> = match (a.count.bytes, a.count.flipped) {
            (true, false) => all
                .get(all.len().saturating_sub(n)..)
                .unwrap_or_default()
                .to_vec(),
            (true, true) => all
                .get(n.saturating_sub(1).min(all.len())..)
                .unwrap_or_default()
                .to_vec(),
            (false, flipped) => {
                let lines = split_lines(&all, a.delimiter);
                let from = if flipped {
                    n.saturating_sub(1).min(lines.len())
                } else {
                    lines.len().saturating_sub(n)
                };
                lines.get(from..).unwrap_or_default().concat()
            }
        };
        if let Err(stop) = ctx.out(&out) {
            return stop.0;
        }
    }
    status
}

#[derive(Default, Clone, Copy)]
struct Counts {
    lines: u64,
    words: u64,
    chars: u64,
    bytes: u64,
    longest: u64,
}

impl Counts {
    fn add(&mut self, other: &Self) {
        self.lines = self.lines.saturating_add(other.lines);
        self.words = self.words.saturating_add(other.words);
        self.chars = self.chars.saturating_add(other.chars);
        self.bytes = self.bytes.saturating_add(other.bytes);
        self.longest = self.longest.max(other.longest);
    }
}

fn count(reader: &mut dyn Read) -> io::Result<Counts> {
    let mut c = Counts::default();
    let mut buf = vec![0_u8; 64 * 1024];
    let mut in_word = false;
    let mut column = 0_u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        for &b in buf.get(..n).unwrap_or_default() {
            c.bytes = c.bytes.saturating_add(1);
            // A UTF-8 continuation byte is part of the character before.
            if b & 0xc0 != 0x80 {
                c.chars = c.chars.saturating_add(1);
            }
            match b {
                b'\n' => {
                    c.lines = c.lines.saturating_add(1);
                    c.longest = c.longest.max(column);
                    column = 0;
                }
                b'\t' => column = (column / 8).saturating_add(1).saturating_mul(8),
                b'\r' | b'\x0c' => column = 0,
                _ if b & 0xc0 != 0x80 && !b.is_ascii_control() => {
                    column = column.saturating_add(1);
                }
                _ => {}
            }
            let space = matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c');
            if space {
                in_word = false;
            } else if !in_word && (b >= 0x80 || b.is_ascii_graphic()) {
                in_word = true;
                c.words = c.words.saturating_add(1);
            }
        }
    }
    c.longest = c.longest.max(column);
    Ok(c)
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "one switch per count wc can print"
)]
#[derive(Clone, Copy)]
struct WcWhich {
    lines: bool,
    words: bool,
    chars: bool,
    bytes: bool,
    longest: bool,
}

fn wc(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let spec = Spec::new(
        "",
        &[
            ("lines", false),
            ("words", false),
            ("chars", false),
            ("bytes", false),
            ("max-line-length", false),
            ("total", true),
            ("files0-from", true),
        ],
    );
    let parsed = match parse(ctx, args, &spec) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let mut w = WcWhich {
        lines: false,
        words: false,
        chars: false,
        bytes: false,
        longest: false,
    };
    let mut total_when = String::from("auto");
    let mut operands = Vec::new();
    for arg in parsed {
        match arg {
            Arg::Operand(op) => operands.push(op),
            Arg::Flag(f, v) => match f.as_str() {
                "-l" | "--lines" => w.lines = true,
                "-w" | "--words" => w.words = true,
                "-m" | "--chars" => w.chars = true,
                "-c" | "--bytes" => w.bytes = true,
                "-L" | "--max-line-length" => w.longest = true,
                "--total" => total_when = value(v.as_ref()).to_string(),
                other => return ctx.unsupported(other),
            },
        }
    }
    if !(w.lines || w.words || w.chars || w.bytes || w.longest) {
        w.lines = true;
        w.words = true;
        w.bytes = true;
    }
    if !matches!(total_when.as_str(), "auto" | "always" | "only" | "never") {
        ctx.error(format!("invalid argument '{total_when}' for '--total'"));
        return 2;
    }
    let implicit = operands.is_empty();
    let operands = or_stdin(operands);
    let selected = [w.lines, w.words, w.chars, w.bytes, w.longest]
        .iter()
        .filter(|b| **b)
        .count();
    let width = number_width(ctx, &operands, selected);
    let mut status = 0;
    let mut total = Counts::default();
    let mut rows = Vec::new();
    for operand in &operands {
        match open(ctx, operand).and_then(|mut r| count(&mut r)) {
            Ok(c) => {
                total.add(&c);
                rows.push((c, (!implicit).then_some(operand.as_str())));
            }
            Err(e) => {
                ctx.io_error(operand, &e);
                status = 1;
            }
        }
    }
    let show_total = match total_when.as_str() {
        "always" | "only" => true,
        "never" => false,
        _ => operands.len() > 1,
    };
    let mut out = String::new();
    if total_when != "only" {
        for (c, name) in &rows {
            out.push_str(&wc_line(c, w, width, *name));
        }
    }
    if show_total {
        let name = (total_when != "only").then_some("total");
        out.push_str(&wc_line(&total, w, width, name));
    }
    finish(ctx.out_str(&out).map(|()| status))
}

/// GNU's column width: wide enough for the sum of the regular files' sizes,
/// at least seven when any input is not a regular file, and one when a
/// single count of a single input is printed.
fn number_width(ctx: &Ctx<'_>, operands: &[String], selected: usize) -> usize {
    if operands.len() == 1 && selected == 1 {
        return 1;
    }
    let mut minimum = 1;
    let mut regular_total = 0_u64;
    for (i, op) in operands.iter().enumerate() {
        if op == "-" {
            minimum = 7;
            continue;
        }
        match ctx.host.metadata(&ctx.resolve(op)) {
            Ok(m) if m.is_file() => regular_total = regular_total.saturating_add(m.len),
            Ok(_) => minimum = 7,
            Err(_) if i == 0 => return 1,
            Err(_) => {}
        }
    }
    regular_total.to_string().len().max(minimum)
}

fn wc_line(c: &Counts, w: WcWhich, width: usize, name: Option<&str>) -> String {
    let fields: Vec<String> = [
        (w.lines, c.lines),
        (w.words, c.words),
        (w.chars, c.chars),
        (w.bytes, c.bytes),
        (w.longest, c.longest),
    ]
    .iter()
    .filter(|(on, _)| *on)
    .map(|(_, n)| format!("{n:>width$}"))
    .collect();
    let mut line = fields.join(" ");
    if let Some(name) = name {
        line.push(' ');
        line.push_str(name);
    }
    line.push('\n');
    line
}

fn nl(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let spec = Spec::new(
        "bnwsvi",
        &[
            ("body-numbering", true),
            ("number-format", true),
            ("number-width", true),
            ("number-separator", true),
            ("starting-line-number", true),
            ("line-increment", true),
        ],
    );
    let parsed = match parse(ctx, args, &spec) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let (mut style, mut format, mut width, mut sep) =
        ('t', "rn".to_string(), 6_usize, "\t".to_string());
    let (mut number, mut incr) = (1_i64, 1_i64);
    let mut operands = Vec::new();
    for arg in parsed {
        match arg {
            Arg::Operand(op) => operands.push(op),
            Arg::Flag(f, v) => {
                let v = value(v.as_ref());
                let bad = || {
                    ctx.error(format!("invalid argument '{v}' for '{f}'"));
                    2
                };
                match f.as_str() {
                    "-b" | "--body-numbering" => match v {
                        "a" | "t" | "n" => style = v.chars().next().unwrap_or('t'),
                        _ => return ctx.unsupported(&format!("{f} {v}")),
                    },
                    "-n" | "--number-format" => match v {
                        "ln" | "rn" | "rz" => format = v.to_string(),
                        _ => return bad(),
                    },
                    "-w" | "--number-width" => match v.parse() {
                        Ok(n) if n > 0 => width = n,
                        _ => return bad(),
                    },
                    "-s" | "--number-separator" => sep = v.to_string(),
                    "-v" | "--starting-line-number" => match v.parse() {
                        Ok(n) => number = n,
                        Err(_) => return bad(),
                    },
                    "-i" | "--line-increment" => match v.parse() {
                        Ok(n) => incr = n,
                        Err(_) => return bad(),
                    },
                    other => return ctx.unsupported(other),
                }
            }
        }
    }
    let blank = " ".repeat(width.saturating_add(sep.chars().count()));
    let mut status = 0;
    for operand in or_stdin(operands) {
        let reader = match open(ctx, &operand) {
            Ok(r) => r,
            Err(e) => {
                ctx.io_error(&operand, &e);
                status = 1;
                continue;
            }
        };
        let result = lines(reader, b'\n', |line| {
            let empty = chomp(line, b'\n').is_empty();
            let numbered = match style {
                'a' => true,
                't' => !empty,
                _ => false,
            };
            let mut out = Vec::new();
            if numbered {
                let label = match format.as_str() {
                    "ln" => format!("{number:<width$}"),
                    "rz" => format!("{number:0>width$}"),
                    _ => format!("{number:>width$}"),
                };
                out.extend_from_slice(label.as_bytes());
                out.extend_from_slice(sep.as_bytes());
                number = number.saturating_add(incr);
            } else {
                out.extend_from_slice(blank.as_bytes());
            }
            out.extend_from_slice(line);
            ctx.out(&out).map_err(CatError::Stop)
        });
        match result {
            Ok(()) => {}
            Err(CatError::Stop(stop)) => return stop.0,
            Err(CatError::Io(e)) => {
                ctx.io_error(&operand, &e);
                status = 1;
            }
        }
    }
    status
}

fn rev(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let parsed = match parse(ctx, args, &Spec::new("", &[])) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let mut operands = Vec::new();
    for arg in parsed {
        match arg {
            Arg::Operand(op) => operands.push(op),
            Arg::Flag(f, _) => return ctx.unsupported(&f),
        }
    }
    let mut status = 0;
    for operand in or_stdin(operands) {
        let reader = match open(ctx, &operand) {
            Ok(r) => r,
            Err(e) => {
                ctx.error(format!("cannot open {operand}: {}", super::describe(&e)));
                status = 1;
                continue;
            }
        };
        let result = lines(reader, b'\n', |line| {
            let body = chomp(line, b'\n');
            let mut out: Vec<u8> = match std::str::from_utf8(body) {
                Ok(text) => text.chars().rev().collect::<String>().into_bytes(),
                Err(_) => body.iter().rev().copied().collect(),
            };
            if body.len() < line.len() {
                out.push(b'\n');
            }
            ctx.out(&out).map_err(CatError::Stop)
        });
        match result {
            Ok(()) => {}
            Err(CatError::Stop(stop)) => return stop.0,
            Err(CatError::Io(e)) => {
                ctx.io_error(&operand, &e);
                status = 1;
            }
        }
    }
    status
}

fn tee(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let spec = Spec::new(
        "",
        &[
            ("append", false),
            ("ignore-interrupts", false),
            ("output-error", false),
        ],
    );
    let parsed = match parse(ctx, args, &spec) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let mut mode = WriteMode::Truncate;
    let mut operands = Vec::new();
    for arg in parsed {
        match arg {
            Arg::Operand(op) => operands.push(op),
            Arg::Flag(f, _) => match f.as_str() {
                "-a" | "--append" => mode = WriteMode::Append,
                // Interrupts reach this command only through the host, which
                // the shell asks between steps; there is no signal to ignore.
                "-i" | "--ignore-interrupts" => {}
                other => return ctx.unsupported(other),
            },
        }
    }
    let mut status = 0;
    let mut files = Vec::new();
    for operand in &operands {
        match ctx.host.open_write(&ctx.resolve(operand), mode) {
            Ok(w) => files.push((operand.clone(), Some(w))),
            Err(e) => {
                ctx.io_error(operand, &e);
                status = 1;
            }
        }
    }
    let mut buf = vec![0_u8; 64 * 1024];
    loop {
        let n = match ctx.io.stdin.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                ctx.io_error("standard input", &e);
                return 1;
            }
        };
        let chunk = buf.get(..n).unwrap_or_default();
        if let Err(stop) = ctx.out(chunk) {
            return stop.0;
        }
        for (name, file) in &mut files {
            if let Some(w) = file
                && let Err(e) = w.write_all(chunk)
            {
                ctx.io_error(name, &e);
                status = 1;
                *file = None;
            }
        }
    }
    for (name, file) in &mut files {
        if let Some(w) = file
            && let Err(e) = w.flush()
        {
            ctx.io_error(name, &e);
            status = 1;
        }
    }
    status
}

/// A `seq` operand: its value in units of `10^-digits`.
#[derive(Clone, Copy)]
struct Decimal {
    units: i128,
    digits: u32,
}

fn decimal(text: &str) -> Option<Decimal> {
    let body = text.strip_prefix('+').unwrap_or(text);
    let (negative, body) = match body.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, body),
    };
    let (whole, frac) = body.split_once('.').unwrap_or((body, ""));
    if whole.is_empty() && frac.is_empty()
        || !whole
            .bytes()
            .chain(frac.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let digits = u32::try_from(frac.len()).ok()?;
    let joined = format!("{whole}{frac}");
    let units: i128 = if joined.is_empty() {
        0
    } else {
        joined.parse().ok()?
    };
    Some(Decimal {
        units: if negative {
            units.checked_neg()?
        } else {
            units
        },
        digits,
    })
}

fn rescale(d: Decimal, digits: u32) -> Option<i128> {
    d.units
        .checked_mul(10_i128.checked_pow(digits.checked_sub(d.digits)?)?)
}

fn show_decimal(units: i128, digits: u32) -> String {
    if digits == 0 {
        return units.to_string();
    }
    let scale = 10_i128.saturating_pow(digits);
    let sign = if units < 0 { "-" } else { "" };
    let abs = units.unsigned_abs();
    let scale = scale.unsigned_abs();
    format!(
        "{sign}{}.{:0>width$}",
        abs.checked_div(scale).unwrap_or(0),
        abs.checked_rem(scale).unwrap_or(0),
        width = digits as usize
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per GNU flag, read in one place"
)]
fn seq(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let mut spec = Spec::new(
        "sf",
        &[
            ("separator", true),
            ("equal-width", false),
            ("format", true),
        ],
    );
    spec.numbers_are_operands = true;
    let parsed = match parse(ctx, args, &spec) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let (mut separator, mut equal) = ("\n".to_string(), false);
    let mut operands = Vec::new();
    for arg in parsed {
        match arg {
            Arg::Operand(op) => operands.push(op),
            Arg::Flag(f, v) => match f.as_str() {
                "-s" | "--separator" => separator = value(v.as_ref()).to_string(),
                "-w" | "--equal-width" => equal = true,
                other => return ctx.unsupported(other),
            },
        }
    }
    let mut numbers = Vec::new();
    for op in &operands {
        let Some(d) = decimal(op) else {
            ctx.error(format!("invalid floating point argument: '{op}'"));
            return 2;
        };
        numbers.push(d);
    }
    let one = Decimal {
        units: 1,
        digits: 0,
    };
    let (first, step, last) = match numbers.as_slice() {
        [last] => (one, one, *last),
        [first, last] => (*first, one, *last),
        [first, step, last] => (*first, *step, *last),
        [] => {
            ctx.error("missing operand");
            return 2;
        }
        _ => {
            ctx.error(format!(
                "extra operand '{}'",
                operands.get(3).map_or("", String::as_str)
            ));
            return 2;
        }
    };
    if step.units == 0 {
        ctx.error(format!(
            "invalid Zero increment value: '{}'",
            operands.get(1).map_or("0", String::as_str)
        ));
        return 2;
    }
    let shown = first.digits.max(step.digits);
    let scale = shown.max(last.digits);
    let (Some(start), Some(inc), Some(end)) = (
        rescale(first, scale),
        rescale(step, scale),
        rescale(last, scale),
    ) else {
        ctx.error("number out of range");
        return 2;
    };
    let drop = 10_i128.saturating_pow(scale.saturating_sub(shown));
    let print = |units: i128| show_decimal(units.checked_div(drop).unwrap_or(units), shown);
    let width = if equal {
        print(start).len().max(print(end).len())
    } else {
        0
    };
    let pad = |text: String| -> String {
        if text.len() >= width {
            return text;
        }
        match text.strip_prefix('-') {
            Some(digits) => format!("-{digits:0>w$}", w = width.saturating_sub(1)),
            None => format!("{text:0>width$}"),
        }
    };
    let mut out = String::new();
    let mut value = start;
    let mut any = false;
    while if inc > 0 { value <= end } else { value >= end } {
        if any {
            out.push_str(&separator);
        }
        out.push_str(&pad(print(value)));
        any = true;
        if out.len() >= 8192 {
            if let Err(stop) = ctx.out_str(&out) {
                return stop.0;
            }
            out.clear();
        }
        match value.checked_add(inc) {
            Some(next) => value = next,
            None => break,
        }
    }
    if any {
        out.push('\n');
    }
    finish(ctx.out_str(&out).map(|()| 0))
}

fn yes(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let words = match args.split_first() {
        Some((first, rest)) if first == "--" => rest,
        _ => args,
    };
    let line = if words.is_empty() {
        "y\n".to_string()
    } else {
        format!("{}\n", words.join(" "))
    };
    // Whole lines, many at once: one write per eight kilobytes or so.
    let copies = 8192_usize.checked_div(line.len()).unwrap_or(1).max(1);
    let chunk = line.repeat(copies);
    loop {
        if let Err(stop) = ctx.out_str(&chunk) {
            return stop.0;
        }
    }
}

/// How long to wait between asking the host whether to stop.
const SLEEP_STEP_MS: u64 = 50;

fn sleep(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let mut operands: Vec<&String> = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        if arg == "--" {
            operands.extend(args.iter().skip(i.saturating_add(1)));
            break;
        }
        if arg.starts_with('-') && arg.len() > 1 && decimal(arg.get(1..).unwrap_or("")).is_none() {
            return ctx.unsupported(arg);
        }
        operands.push(arg);
    }
    if operands.is_empty() {
        ctx.error("missing operand");
        return 2;
    }
    let mut millis = 0_f64;
    for op in operands {
        let (number, unit) = match op.char_indices().last() {
            Some((at, c)) if "smhd".contains(c) && at > 0 => (op.get(..at).unwrap_or(""), c),
            _ => (op.as_str(), 's'),
        };
        let seconds: f64 = match number.parse::<f64>() {
            Ok(s) if s >= 0.0 && !s.is_nan() => s,
            _ => {
                ctx.error(format!("invalid time interval '{op}'"));
                return 2;
            }
        };
        let factor = match unit {
            'm' => 60.0,
            'h' => 3600.0,
            'd' => 86_400.0,
            _ => 1.0,
        };
        millis += seconds * factor * 1000.0;
    }
    // Saturating float-to-int: `infinity` becomes the longest wait there is.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "non-negative, and `as` saturates"
    )]
    let mut left = millis.ceil() as u64;
    while left > 0 {
        if ctx.host.interrupted() {
            return 130;
        }
        let step = left.min(SLEEP_STEP_MS);
        ctx.host.sleep(step);
        left = left.saturating_sub(step);
    }
    if ctx.host.interrupted() { 130 } else { 0 }
}

fn printenv(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let parsed = match parse(ctx, args, &Spec::new("", &[("null", false)])) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let mut end = '\n';
    let mut names = Vec::new();
    for arg in parsed {
        match arg {
            Arg::Operand(op) => names.push(op),
            Arg::Flag(f, _) if f == "-0" || f == "--null" => end = '\0',
            Arg::Flag(f, _) => return ctx.unsupported(&f),
        }
    }
    let mut out = String::new();
    let mut status = 0;
    if names.is_empty() {
        for (k, v) in ctx.env {
            out.push_str(k);
            out.push('=');
            out.push_str(v);
            out.push(end);
        }
    } else {
        for name in &names {
            match ctx.env.iter().rev().find(|(k, _)| k == name) {
                Some((_, v)) if !name.contains('=') => {
                    out.push_str(v);
                    out.push(end);
                }
                _ => status = 1,
            }
        }
    }
    finish(ctx.out_str(&out).map(|()| status))
}

/// `env [-i] [-0] [-C DIR] [-u NAME]... [NAME=VALUE]... [COMMAND [ARG]...]`:
/// print the environment, or run `COMMAND` in the changed one, resolved as
/// the shell resolves any utility.
fn env(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let mut vars: Vec<(String, String)> = ctx.env.to_vec();
    let mut change = super::EnvChange::default();
    let mut end = '\n';
    let mut dir: Option<String> = None;
    let mut rest = args.iter();
    let mut flags_done = false;
    let unset = |vars: &mut Vec<(String, String)>, change: &mut super::EnvChange, name: &str| {
        vars.retain(|(k, _)| k != name);
        change.set.retain(|(k, _)| k != name);
        change.unset.push(name.to_string());
    };
    while let Some(arg) = rest.next() {
        if !flags_done && arg == "--" {
            flags_done = true;
            continue;
        }
        if !flags_done && arg.starts_with('-') && arg.len() > 1 {
            match arg.as_str() {
                "-i" | "--ignore-environment" => {
                    vars.clear();
                    change = super::EnvChange {
                        clear: true,
                        ..super::EnvChange::default()
                    };
                }
                "-0" | "--null" => end = '\0',
                "-u" | "--unset" | "-C" | "--chdir" => {
                    let Some(operand) = rest.next() else {
                        let letter = if arg.contains('u') { 'u' } else { 'C' };
                        ctx.error(format!("option requires an argument -- '{letter}'"));
                        return 125;
                    };
                    if arg.contains('u') {
                        unset(&mut vars, &mut change, operand);
                    } else {
                        dir = Some(operand.clone());
                    }
                }
                other => {
                    if let Some(name) = other.strip_prefix("--unset=") {
                        unset(&mut vars, &mut change, name);
                    } else if let Some(d) = other.strip_prefix("--chdir=") {
                        dir = Some(d.to_string());
                    } else {
                        return ctx.unsupported(other);
                    }
                }
            }
            continue;
        }
        if arg == "-" && !flags_done {
            vars.clear();
            change = super::EnvChange {
                clear: true,
                ..super::EnvChange::default()
            };
            continue;
        }
        flags_done = true;
        let Some((name, val)) = arg.split_once('=').filter(|(n, _)| !n.is_empty()) else {
            // The first operand that is no assignment is the command.
            let command: Vec<String> = std::iter::once(arg.clone()).chain(rest.cloned()).collect();
            if end == '\0' {
                ctx.error("cannot specify --null (-0) with command");
                return 125;
            }
            let cwd = dir
                .as_deref()
                .map_or_else(|| ctx.cwd.to_path_buf(), |d| ctx.resolve(d));
            return ctx.dispatch.run_with_env(&command, &cwd, ctx.io, &change);
        };
        vars.retain(|(k, _)| k != name);
        vars.push((name.to_string(), val.to_string()));
        change.set.retain(|(k, _)| k != name);
        change.set.push((name.to_string(), val.to_string()));
    }
    if let Some(d) = dir {
        ctx.error(format!("must specify command with --chdir (-C) ('{d}')"));
        return 125;
    }
    let mut out = String::new();
    for (k, v) in &vars {
        out.push_str(k);
        out.push('=');
        out.push_str(v);
        out.push(end);
    }
    finish(ctx.out_str(&out).map(|()| 0))
}
