//! `cut` and `tr`: select parts of lines, and translate or delete bytes.
//! Both work on bytes, as GNU's do (`cut -c` is `cut -b` there too).

use std::io::Read;

use super::util::{Arg, Spec, lines, open, parse, value};
use super::{Ctx, Stop};

/// A list of 1-based positions: `1,3-5,7-`, `-2`.
struct Ranges(Vec<(usize, usize)>);

impl Ranges {
    fn parse(text: &str) -> Result<Self, String> {
        let mut out = Vec::new();
        for part in text.split([',', ' ']) {
            if part == "-" {
                return Err("invalid range with no endpoint: -".into());
            }
            let (lo, hi) = if let Some((lo, hi)) = part.split_once('-') {
                let lo = if lo.is_empty() { 1 } else { position(lo)? };
                let hi = if hi.is_empty() {
                    usize::MAX
                } else {
                    position(hi)?
                };
                (lo, hi)
            } else {
                let n = position(part)?;
                (n, n)
            };
            if lo == 0 {
                return Err("fields and positions are numbered from 1".into());
            }
            if lo > hi {
                return Err("invalid decreasing range".into());
            }
            out.push((lo, hi));
        }
        out.sort_unstable();
        // Merge overlapping and touching ranges, so output delimiters go
        // between runs that are really apart.
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (lo, hi) in out {
            match merged.last_mut() {
                Some(last) if lo <= last.1.saturating_add(1) => last.1 = last.1.max(hi),
                _ => merged.push((lo, hi)),
            }
        }
        Ok(Self(merged))
    }

    fn contains(&self, n: usize) -> bool {
        self.0.iter().any(|&(lo, hi)| lo <= n && n <= hi)
    }

    /// Which run `n` falls in.
    fn run_of(&self, n: usize) -> Option<usize> {
        self.0.iter().position(|&(lo, hi)| lo <= n && n <= hi)
    }
}

fn position(text: &str) -> Result<usize, String> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("invalid byte, character or field list: '{text}'"));
    }
    text.parse()
        .map_err(|_| format!("byte offset '{text}' is too large"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Bytes,
    Fields,
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per GNU flag, read in one place"
)]
pub fn cut(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let spec = Spec::new(
        "bcfd",
        &[
            ("bytes", true),
            ("characters", true),
            ("fields", true),
            ("delimiter", true),
            ("only-delimited", false),
            ("complement", false),
            ("output-delimiter", true),
            ("zero-terminated", false),
        ],
    );
    let parsed = match parse(ctx, args, &spec) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let mut mode: Option<(Mode, String)> = None;
    let mut delimiter: Option<u8> = None;
    let (mut only_delimited, mut complement) = (false, false);
    let mut output_delimiter: Option<Vec<u8>> = None;
    let mut terminator = b'\n';
    let mut operands = Vec::new();
    for arg in parsed {
        let (flag, v) = match arg {
            Arg::Operand(op) => {
                operands.push(op);
                continue;
            }
            Arg::Flag(f, v) => (f, v),
        };
        let val = value(v.as_ref()).to_string();
        let chosen = match flag.as_str() {
            "-b" | "--bytes" | "-c" | "--characters" => Some(Mode::Bytes),
            "-f" | "--fields" => Some(Mode::Fields),
            _ => None,
        };
        if let Some(m) = chosen {
            if mode.is_some() {
                ctx.error("only one list may be specified");
                return 2;
            }
            mode = Some((m, val));
            continue;
        }
        match flag.as_str() {
            "-d" | "--delimiter" => {
                if val.len() > 1 {
                    ctx.error("the delimiter must be a single character");
                    return 2;
                }
                delimiter = Some(val.bytes().next().unwrap_or(0));
            }
            "-s" | "--only-delimited" => only_delimited = true,
            "--complement" => complement = true,
            "--output-delimiter" => output_delimiter = Some(val.into_bytes()),
            "-z" | "--zero-terminated" => terminator = 0,
            // GNU accepts and ignores it.
            "-n" => {}
            other => return ctx.unsupported(other),
        }
    }
    let Some((mode, list)) = mode else {
        ctx.error("you must specify a list of bytes, characters, or fields");
        return 2;
    };
    if mode == Mode::Bytes && (delimiter.is_some() || only_delimited) {
        ctx.error("an input delimiter may be specified only when operating on fields");
        return 2;
    }
    let ranges = match Ranges::parse(&list) {
        Ok(r) => r,
        Err(message) => {
            ctx.error(message);
            return 2;
        }
    };
    let delimiter = delimiter.unwrap_or(b'\t');
    let selected = |n: usize| ranges.contains(n) != complement;
    let operands = if operands.is_empty() {
        vec!["-".to_string()]
    } else {
        operands
    };
    let mut status = 0;
    for op in &operands {
        let reader = match open(ctx, op) {
            Ok(r) => r,
            Err(e) => {
                ctx.io_error(op, &e);
                status = 1;
                continue;
            }
        };
        let result = lines(reader, terminator, |line| -> Result<(), LineError> {
            let body = super::util::chomp(line, terminator);
            let mut out = Vec::with_capacity(body.len().saturating_add(1));
            match mode {
                Mode::Bytes => {
                    let mut last_run: Option<usize> = None;
                    for (i, &b) in body.iter().enumerate() {
                        let n = i.saturating_add(1);
                        if !selected(n) {
                            continue;
                        }
                        if let Some(sep) = &output_delimiter {
                            let run = if complement { None } else { ranges.run_of(n) };
                            if last_run.is_some() && run != last_run {
                                out.extend_from_slice(sep);
                            }
                            last_run = run.or(Some(0));
                        }
                        out.push(b);
                    }
                }
                Mode::Fields => {
                    if body.contains(&delimiter) {
                        let sep = output_delimiter.clone().unwrap_or_else(|| vec![delimiter]);
                        let mut first = true;
                        for (i, field) in body.split(|&b| b == delimiter).enumerate() {
                            if selected(i.saturating_add(1)) {
                                if !first {
                                    out.extend_from_slice(&sep);
                                }
                                out.extend_from_slice(field);
                                first = false;
                            }
                        }
                    } else {
                        if only_delimited {
                            return Ok(());
                        }
                        out.extend_from_slice(body);
                    }
                }
            }
            out.push(terminator);
            ctx.out(&out).map_err(LineError::Stop)
        });
        match result {
            Ok(()) => {}
            Err(LineError::Stop(Stop(code))) => return code,
            Err(LineError::Io(e)) => {
                ctx.io_error(op, &e);
                status = 1;
            }
        }
    }
    status
}

enum LineError {
    Stop(Stop),
    Io(std::io::Error),
}

impl From<std::io::Error> for LineError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// One element of a `tr` set, before ranges and repeats are expanded.
enum Item {
    Byte(u8),
    Class(Vec<u8>),
    /// `[c*n]`; `n` of none fills SET2 to SET1's length.
    Repeat(u8, Option<usize>),
}

fn class_bytes(name: &str) -> Option<Vec<u8>> {
    let all = 0_u8..=255;
    let pick: fn(&u8) -> bool = match name {
        "alpha" => u8::is_ascii_alphabetic,
        "digit" => u8::is_ascii_digit,
        "alnum" => u8::is_ascii_alphanumeric,
        "upper" => u8::is_ascii_uppercase,
        "lower" => u8::is_ascii_lowercase,
        "space" => |b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c'),
        "blank" => |b| *b == b' ' || *b == b'\t',
        "punct" => u8::is_ascii_punctuation,
        "print" => |b| (0x20..0x7f).contains(b),
        "graph" => u8::is_ascii_graphic,
        "cntrl" => u8::is_ascii_control,
        "xdigit" => u8::is_ascii_hexdigit,
        _ => return None,
    };
    Some(all.filter(pick).collect())
}

/// The bytes of a set's text after backslash escapes, with each byte marked
/// whether it was escaped (so `\-` is never a range).
fn unescape(text: &str) -> Vec<(u8, bool)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0_usize;
    while let Some(&b) = bytes.get(i) {
        i = i.saturating_add(1);
        if b != b'\\' {
            out.push((b, false));
            continue;
        }
        let Some(&e) = bytes.get(i) else {
            out.push((b'\\', true));
            break;
        };
        i = i.saturating_add(1);
        let byte = match e {
            b'a' => 7,
            b'b' => 8,
            b'f' => 12,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'v' => 11,
            b'0'..=b'7' => {
                let mut code = u32::from(e.wrapping_sub(b'0'));
                for _ in 0..2 {
                    match bytes.get(i) {
                        Some(&d @ b'0'..=b'7') if code.saturating_mul(8) < 256 => {
                            code = code
                                .saturating_mul(8)
                                .saturating_add(u32::from(d.wrapping_sub(b'0')));
                            i = i.saturating_add(1);
                        }
                        _ => break,
                    }
                }
                u8::try_from(code).unwrap_or(u8::MAX)
            }
            other => other,
        };
        out.push((byte, true));
    }
    out
}

fn parse_set(text: &str) -> Result<Vec<Item>, String> {
    let chars = unescape(text);
    let mut items = Vec::new();
    let mut i = 0_usize;
    while let Some(&(b, escaped)) = chars.get(i) {
        let next = |k: usize| chars.get(i.saturating_add(k)).copied();
        if b == b'[' && !escaped {
            // `[:class:]`, `[=c=]` or `[c*n]`.
            if let Some((b':' | b'=', false)) = next(1) {
                let kind = next(1).map_or(b':', |(k, _)| k);
                let rest: Vec<u8> = chars
                    .iter()
                    .skip(i.saturating_add(2))
                    .map(|(c, _)| *c)
                    .collect();
                if let Some(end) = rest.windows(2).position(|w| w == [kind, b']']) {
                    let name =
                        String::from_utf8_lossy(rest.get(..end).unwrap_or_default()).into_owned();
                    if kind == b':' {
                        let bytes = class_bytes(&name)
                            .ok_or_else(|| format!("invalid character class '{name}'"))?;
                        items.push(Item::Class(bytes));
                    } else {
                        items.extend(name.bytes().map(Item::Byte));
                    }
                    i = i.saturating_add(end).saturating_add(4);
                    continue;
                }
            }
            if let (Some((c, _)), Some((b'*', false))) = (next(1), next(2)) {
                let rest: Vec<u8> = chars
                    .iter()
                    .skip(i.saturating_add(3))
                    .map(|(c, _)| *c)
                    .collect();
                if let Some(end) = rest.iter().position(|&c| c == b']') {
                    let count =
                        String::from_utf8_lossy(rest.get(..end).unwrap_or_default()).into_owned();
                    let n = if count.is_empty() {
                        None
                    } else {
                        let radix = if count.starts_with('0') { 8 } else { 10 };
                        let n = usize::from_str_radix(&count, radix).map_err(|_| {
                            format!("invalid repeat count '{count}' in [c*n] construct")
                        })?;
                        (n > 0).then_some(n)
                    };
                    items.push(Item::Repeat(c, n));
                    i = i.saturating_add(end).saturating_add(4);
                    continue;
                }
            }
        }
        if let (Some((b'-', false)), Some((hi, _))) = (next(1), next(2)) {
            if hi < b {
                return Err(format!(
                    "range-endpoints of '{}-{}' are in reverse collating sequence order",
                    char::from(b),
                    char::from(hi)
                ));
            }
            items.extend((b..=hi).map(Item::Byte));
            i = i.saturating_add(3);
            continue;
        }
        items.push(Item::Byte(b));
        i = i.saturating_add(1);
    }
    Ok(items)
}

/// A set's bytes in order; `fill` is how long a `[c*]` repeat makes it.
fn expand(items: &[Item], fill: usize) -> Vec<u8> {
    let fixed: usize = items
        .iter()
        .map(|it| match it {
            Item::Byte(_) => 1,
            Item::Class(c) => c.len(),
            Item::Repeat(_, n) => n.unwrap_or(0),
        })
        .fold(0, usize::saturating_add);
    let mut out = Vec::new();
    for it in items {
        match it {
            Item::Byte(b) => out.push(*b),
            Item::Class(c) => out.extend_from_slice(c),
            Item::Repeat(b, Some(n)) => out.extend(std::iter::repeat_n(*b, *n)),
            Item::Repeat(b, None) => {
                out.extend(std::iter::repeat_n(*b, fill.saturating_sub(fixed)));
            }
        }
    }
    out
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per GNU flag, read in one place"
)]
pub fn tr(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let spec = Spec::new(
        "",
        &[
            ("complement", false),
            ("delete", false),
            ("squeeze-repeats", false),
            ("truncate-set1", false),
        ],
    );
    let parsed = match parse(ctx, args, &spec) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let (mut complement, mut delete, mut squeeze, mut truncate) = (false, false, false, false);
    let mut sets = Vec::new();
    for arg in parsed {
        match arg {
            Arg::Operand(op) => sets.push(op),
            Arg::Flag(f, _) => match f.as_str() {
                "-c" | "-C" | "--complement" => complement = true,
                "-d" | "--delete" => delete = true,
                "-s" | "--squeeze-repeats" => squeeze = true,
                "-t" | "--truncate-set1" => truncate = true,
                other => return ctx.unsupported(other),
            },
        }
    }
    let translating = !delete && sets.len() == 2;
    let wanted = if delete && squeeze || translating {
        2
    } else {
        1
    };
    if sets.is_empty() {
        ctx.error("missing operand");
        return 2;
    }
    if sets.len() < wanted || (!squeeze && !delete && sets.len() < 2) {
        ctx.error(format!(
            "missing operand after '{}'",
            sets.last().map_or("", String::as_str)
        ));
        return 2;
    }
    if sets.len() > wanted {
        ctx.error(format!(
            "extra operand '{}'",
            sets.get(wanted).map_or("", String::as_str)
        ));
        return 2;
    }
    let parsed_sets: Result<Vec<Vec<Item>>, String> = sets.iter().map(|s| parse_set(s)).collect();
    let parsed_sets = match parsed_sets {
        Ok(p) => p,
        Err(message) => {
            ctx.error(message);
            return 2;
        }
    };
    let mut set1 = parsed_sets
        .first()
        .map(|s| expand(s, 0))
        .unwrap_or_default();
    if complement {
        let mut member = [false; 256];
        for &b in &set1 {
            if let Some(m) = member.get_mut(usize::from(b)) {
                *m = true;
            }
        }
        set1 = (0_u8..=255)
            .filter(|b| !member.get(usize::from(*b)).copied().unwrap_or(false))
            .collect();
    }
    let set2 = parsed_sets
        .get(1)
        .map(|s| expand(s, set1.len()))
        .unwrap_or_default();
    let mut in_set1 = [false; 256];
    for &b in &set1 {
        if let Some(m) = in_set1.get_mut(usize::from(b)) {
            *m = true;
        }
    }
    let mut map: [u8; 256] = std::array::from_fn(|i| u8::try_from(i).unwrap_or(0));
    if translating {
        if set2.is_empty() && !set1.is_empty() {
            ctx.error("when not truncating set1, string2 must be non-empty");
            return 2;
        }
        let last = set2.last().copied().unwrap_or(0);
        let count = if truncate {
            set1.len().min(set2.len())
        } else {
            set1.len()
        };
        for (i, &from) in set1.iter().enumerate().take(count) {
            if let Some(slot) = map.get_mut(usize::from(from)) {
                *slot = set2.get(i).copied().unwrap_or(last);
            }
        }
    }
    let squeeze_set: &[u8] = if delete || translating { &set2 } else { &set1 };
    let mut in_squeeze = [false; 256];
    if squeeze {
        for &b in squeeze_set {
            if let Some(m) = in_squeeze.get_mut(usize::from(b)) {
                *m = true;
            }
        }
    }
    let member = |table: &[bool; 256], b: u8| table.get(usize::from(b)).copied().unwrap_or(false);
    let mut last: Option<u8> = None;
    let mut reader = super::util::Stdin(ctx.io.stdin.clone());
    let mut buf = vec![0_u8; 64 * 1024];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => return 0,
            Ok(n) => n,
            Err(e) => {
                ctx.io_error("standard input", &e);
                return 1;
            }
        };
        let mut out = Vec::with_capacity(n);
        for &b in buf.get(..n).unwrap_or_default() {
            if delete && member(&in_set1, b) {
                continue;
            }
            let c = map.get(usize::from(b)).copied().unwrap_or(b);
            if squeeze && last == Some(c) && member(&in_squeeze, c) {
                continue;
            }
            last = Some(c);
            out.push(c);
        }
        if let Err(Stop(code)) = ctx.out(&out) {
            return code;
        }
    }
}
