//! `sort` and `uniq`, as GNU's in the C locale: lines compare byte by byte
//! unless a key or flag says otherwise.

use std::cmp::Ordering;
use std::io::{Read, Write};

use super::util::{Arg, Spec, open, parse, split_lines, value};
use super::{Ctx, Stop};
use crate::host::WriteMode;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one switch per GNU ordering flag"
)]
struct Order {
    numeric: bool,
    general: bool,
    human: bool,
    fold: bool,
    dictionary: bool,
    printable: bool,
    reverse: bool,
    blanks: bool,
}

impl Order {
    fn any(self) -> bool {
        self != Self::default()
    }
}

#[derive(Clone, Copy)]
struct Key {
    start_field: usize,
    start_char: usize,
    /// The last field, or none for the end of the line.
    end_field: Option<usize>,
    /// The last character in the end field; 0 for all of it.
    end_char: usize,
    order: Order,
    blank_start: bool,
    blank_end: bool,
}

struct Sorter {
    keys: Vec<Key>,
    global: Order,
    separator: Option<u8>,
    stable: bool,
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per GNU flag, read in one place"
)]
pub fn sort(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let spec = Spec::new(
        "ktoST",
        &[
            ("numeric-sort", false),
            ("general-numeric-sort", false),
            ("human-numeric-sort", false),
            ("ignore-case", false),
            ("dictionary-order", false),
            ("ignore-nonprinting", false),
            ("ignore-leading-blanks", false),
            ("reverse", false),
            ("unique", false),
            ("stable", false),
            ("key", true),
            ("field-separator", true),
            ("output", true),
            ("zero-terminated", false),
            ("check", false),
            ("merge", false),
            ("buffer-size", true),
            ("temporary-directory", true),
            ("parallel", true),
            ("month-sort", false),
            ("version-sort", false),
            ("random-sort", false),
            ("sort", true),
        ],
    );
    let parsed = match parse(ctx, args, &spec) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let mut s = Sorter {
        keys: Vec::new(),
        global: Order::default(),
        separator: None,
        stable: false,
    };
    let (mut unique, mut check, mut quiet_check, mut delimiter) = (false, false, false, b'\n');
    let mut output: Option<String> = None;
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
        match flag.as_str() {
            "-n" | "--numeric-sort" => s.global.numeric = true,
            "-g" | "--general-numeric-sort" => s.global.general = true,
            "-h" | "--human-numeric-sort" => s.global.human = true,
            "-f" | "--ignore-case" => s.global.fold = true,
            "-d" | "--dictionary-order" => s.global.dictionary = true,
            "-i" | "--ignore-nonprinting" => s.global.printable = true,
            "-b" | "--ignore-leading-blanks" => s.global.blanks = true,
            "-r" | "--reverse" => s.global.reverse = true,
            "-u" | "--unique" => unique = true,
            "-s" | "--stable" => s.stable = true,
            "-z" | "--zero-terminated" => delimiter = 0,
            "-c" | "--check" => check = true,
            "-C" => {
                check = true;
                quiet_check = true;
            }
            "-o" | "--output" => output = Some(val.to_string()),
            // Merging sorted inputs gives what sorting them all gives, and
            // the memory and temporary-file knobs have nothing to tune:
            // everything is sorted in memory.
            "-m"
            | "--merge"
            | "-S"
            | "--buffer-size"
            | "-T"
            | "--temporary-directory"
            | "--parallel" => {}
            "-t" | "--field-separator" => {
                let sep = if val == "\\0" {
                    Some(0)
                } else {
                    val.bytes().next()
                };
                match (sep, val.len()) {
                    (Some(b), 1) | (Some(b @ 0), 2) => s.separator = Some(b),
                    _ => {
                        ctx.error(format!("multi-character tab '{val}'"));
                        return 2;
                    }
                }
            }
            "-k" | "--key" => {
                let Some(k) = parse_key(val) else {
                    ctx.error(format!("invalid field specification '{val}'"));
                    return 2;
                };
                s.keys.push(k);
            }
            other => return ctx.unsupported(other),
        }
    }
    let operands = if operands.is_empty() {
        vec!["-".to_string()]
    } else {
        operands
    };
    let mut input: Vec<(String, Vec<u8>)> = Vec::new();
    for op in &operands {
        let mut bytes = Vec::new();
        match open(ctx, op).and_then(|mut r| r.read_to_end(&mut bytes)) {
            Ok(_) => input.push((op.clone(), bytes)),
            Err(e) => {
                ctx.error(format!("cannot read: {op}: {}", super::describe(&e)));
                return 2;
            }
        }
    }
    let mut lines: Vec<&[u8]> = Vec::new();
    for (_, bytes) in &input {
        lines.extend(
            split_lines(bytes, delimiter)
                .into_iter()
                .map(|l| super::util::chomp(l, delimiter)),
        );
    }
    if check {
        for (i, pair) in lines.windows(2).enumerate() {
            let [a, b] = pair else { continue };
            let order = s.compare(a, b);
            if order == Ordering::Greater || unique && order == Ordering::Equal {
                if !quiet_check {
                    let name = operands.first().map_or("-", String::as_str);
                    ctx.error(format!(
                        "{name}:{}: disorder: {}",
                        i.saturating_add(2),
                        String::from_utf8_lossy(b)
                    ));
                }
                return 1;
            }
        }
        return 0;
    }
    lines.sort_by(|a, b| s.compare(a, b));
    if unique {
        lines.dedup_by(|b, a| s.compare_keys(a, b) == Ordering::Equal);
    }
    let mut out = Vec::new();
    for line in &lines {
        out.extend_from_slice(line);
        out.push(delimiter);
    }
    if let Some(file) = output {
        let written = ctx
            .host
            .open_write(&ctx.resolve(&file), WriteMode::Truncate, ctx.run)
            .and_then(|mut w| w.write_all(&out).and_then(|()| w.flush()));
        return match written {
            Ok(()) => 0,
            Err(e) => {
                ctx.error(format!("open failed: {file}: {}", super::describe(&e)));
                2
            }
        };
    }
    match ctx.out(&out) {
        Ok(()) => 0,
        Err(Stop(code)) => code,
    }
}

fn parse_key(spec: &str) -> Option<Key> {
    let (start, end) = match spec.split_once(',') {
        Some((s, e)) => (s, Some(e)),
        None => (spec, None),
    };
    let mut key = Key {
        start_field: 1,
        start_char: 1,
        end_field: None,
        end_char: 0,
        order: Order::default(),
        blank_start: false,
        blank_end: false,
    };
    let (field, ch, blanks) = key_part(start, &mut key.order)?;
    if field == 0 || ch == Some(0) {
        return None;
    }
    key.start_field = field;
    key.start_char = ch.unwrap_or(1);
    key.blank_start = blanks;
    if let Some(end) = end {
        let (field, ch, blanks) = key_part(end, &mut key.order)?;
        if field == 0 {
            return None;
        }
        key.end_field = Some(field);
        key.end_char = ch.unwrap_or(0);
        key.blank_end = blanks;
    }
    Some(key)
}

/// `F[.C][OPTS]`: the field, the character, and whether `b` was among the
/// options, which also land in `order`.
fn key_part(text: &str, order: &mut Order) -> Option<(usize, Option<usize>, bool)> {
    let opts_at = text
        .char_indices()
        .find(|(_, c)| c.is_ascii_alphabetic())
        .map_or(text.len(), |(i, _)| i);
    let (pos, opts) = text.split_at(opts_at);
    let (field, ch) = match pos.split_once('.') {
        Some((f, c)) => (f.parse().ok()?, Some(c.parse().ok()?)),
        None => (pos.parse().ok()?, None),
    };
    let mut blanks = false;
    for c in opts.chars() {
        match c {
            'b' => blanks = true,
            'n' => order.numeric = true,
            'g' => order.general = true,
            'h' => order.human = true,
            'f' => order.fold = true,
            'd' => order.dictionary = true,
            'i' => order.printable = true,
            'r' => order.reverse = true,
            _ => return None,
        }
    }
    Some((field, ch, blanks))
}

fn is_blank(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

/// Where each field of `line` starts and ends.
fn fields(line: &[u8], separator: Option<u8>) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    if let Some(sep) = separator {
        let mut start = 0;
        for (i, &b) in line.iter().enumerate() {
            if b == sep {
                out.push((start, i));
                start = i.saturating_add(1);
            }
        }
        out.push((start, line.len()));
    } else {
        // A field is its leading blanks and the non-blanks after them.
        let mut pos = 0;
        while pos < line.len() {
            let start = pos;
            while line.get(pos).copied().is_some_and(is_blank) {
                pos = pos.saturating_add(1);
            }
            while line.get(pos).is_some_and(|&b| !is_blank(b)) {
                pos = pos.saturating_add(1);
            }
            out.push((start, pos));
        }
    }
    out
}

fn skip_blanks(line: &[u8], mut at: usize, end: usize) -> usize {
    while at < end && line.get(at).copied().is_some_and(is_blank) {
        at = at.saturating_add(1);
    }
    at
}

impl Sorter {
    fn key_slice<'l>(&self, line: &'l [u8], key: &Key, order: Order) -> &'l [u8] {
        let f = fields(line, self.separator);
        let len = line.len();
        let start = match f.get(key.start_field.saturating_sub(1)) {
            Some(&(s, e)) => {
                let s = if key.blank_start || order.blanks && !key.order.any() {
                    skip_blanks(line, s, e)
                } else {
                    s
                };
                s.saturating_add(key.start_char.saturating_sub(1)).min(e)
            }
            None => len,
        };
        let end = match key.end_field {
            None => len,
            Some(ef) => match f.get(ef.saturating_sub(1)) {
                Some(&(_, e)) if key.end_char == 0 => e,
                Some(&(s, e)) => {
                    let s = if key.blank_end {
                        skip_blanks(line, s, e)
                    } else {
                        s
                    };
                    s.saturating_add(key.end_char).min(e)
                }
                None => len,
            },
        };
        line.get(start..end.max(start)).unwrap_or_default()
    }

    /// Compare by the keys alone, as `-u` decides what is a duplicate.
    fn compare_keys(&self, a: &[u8], b: &[u8]) -> Ordering {
        if self.keys.is_empty() {
            return compare_with(a, b, self.global);
        }
        for key in &self.keys {
            let order = if key.order.any() {
                Order {
                    blanks: false,
                    ..key.order
                }
            } else {
                self.global
            };
            let ka = self.key_slice(a, key, self.global);
            let kb = self.key_slice(b, key, self.global);
            let o = compare_with(ka, kb, order);
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    }

    fn compare(&self, a: &[u8], b: &[u8]) -> Ordering {
        let o = self.compare_keys(a, b);
        if o != Ordering::Equal || self.stable {
            return o;
        }
        // The last resort: whole lines, byte by byte, reversed under `-r`.
        let whole = a.cmp(b);
        if self.global.reverse {
            whole.reverse()
        } else {
            whole
        }
    }
}

fn compare_with(a: &[u8], b: &[u8], order: Order) -> Ordering {
    let (a, b) = if order.blanks {
        (trim_blanks(a), trim_blanks(b))
    } else {
        (a, b)
    };
    let o = if order.numeric {
        numeric(a, b)
    } else if order.human {
        human(a, b)
    } else if order.general {
        general(a).total_cmp(&general(b))
    } else if order.fold || order.dictionary || order.printable {
        let keep = |c: &&u8| {
            (!order.dictionary || c.is_ascii_alphanumeric() || is_blank(**c))
                && (!order.printable || (0x20..0x7f).contains(*c))
        };
        let fold = |c: &u8| {
            if order.fold {
                c.to_ascii_uppercase()
            } else {
                *c
            }
        };
        a.iter()
            .filter(keep)
            .map(fold)
            .cmp(b.iter().filter(keep).map(fold))
    } else {
        a.cmp(b)
    };
    if order.reverse { o.reverse() } else { o }
}

fn trim_blanks(s: &[u8]) -> &[u8] {
    let at = s.iter().position(|&b| !is_blank(b)).unwrap_or(s.len());
    s.get(at..).unwrap_or_default()
}

/// The leading number of `s` as `sort -n` reads it: sign, integer digits
/// without leading zeros, and fraction digits without trailing zeros.
fn number_parts(s: &[u8]) -> (bool, &[u8], &[u8], usize) {
    let s = trim_blanks(s);
    let (negative, s) = match s.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, s),
    };
    let int_len = s.iter().take_while(|b| b.is_ascii_digit()).count();
    let int = s.get(..int_len).unwrap_or_default();
    let int = int
        .get(int.iter().take_while(|&&b| b == b'0').count()..)
        .unwrap_or_default();
    let rest = s.get(int_len..).unwrap_or_default();
    let frac = match rest.split_first() {
        Some((b'.', f)) => {
            let n = f.iter().take_while(|b| b.is_ascii_digit()).count();
            f.get(..n).unwrap_or_default()
        }
        _ => &[][..],
    };
    let frac_trim = frac
        .len()
        .saturating_sub(frac.iter().rev().take_while(|&&b| b == b'0').count());
    let frac = frac.get(..frac_trim).unwrap_or_default();
    let consumed = int_len.saturating_add(frac.len());
    (
        negative && (!int.is_empty() || !frac.is_empty()),
        int,
        frac,
        consumed,
    )
}

fn numeric(a: &[u8], b: &[u8]) -> Ordering {
    let (na, ia, fa, _) = number_parts(a);
    let (nb, ib, fb, _) = number_parts(b);
    let magnitude = ia
        .len()
        .cmp(&ib.len())
        .then_with(|| ia.cmp(ib))
        .then_with(|| fa.cmp(fb));
    match (na, nb) {
        (false, false) => magnitude,
        (true, true) => magnitude.reverse(),
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
    }
}

fn human(a: &[u8], b: &[u8]) -> Ordering {
    let rank = |s: &[u8]| -> (bool, usize) {
        let (negative, int, frac, _) = number_parts(s);
        let t = trim_blanks(s);
        let digits = t
            .iter()
            .position(|&c| !(c.is_ascii_digit() || c == b'.' || c == b'-'))
            .unwrap_or(t.len());
        let unit = t.get(digits).map_or(0, |&c| {
            b"KMGTPEZYRQ"
                .iter()
                .position(|&u| u == c.to_ascii_uppercase())
                .map_or(0, |p| p.saturating_add(1))
        });
        let zero = int.is_empty() && frac.is_empty();
        (negative, if zero { 0 } else { unit })
    };
    let (na, ua) = rank(a);
    let (nb, ub) = rank(b);
    let by_unit = if na && nb { ub.cmp(&ua) } else { ua.cmp(&ub) };
    match (na, nb) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => by_unit.then_with(|| numeric(a, b)),
    }
}

/// `sort -g`: the leading floating-point number, and anything that is not
/// one before every number.
fn general(s: &[u8]) -> f64 {
    let t = trim_blanks(s);
    let text = String::from_utf8_lossy(t);
    let mut best = f64::NEG_INFINITY;
    for end in (1..=text.len()).rev() {
        if let Some(prefix) = text.get(..end)
            && let Ok(v) = prefix.parse::<f64>()
        {
            best = v;
            break;
        }
    }
    best
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per GNU flag, read in one place"
)]
pub fn uniq(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let spec = Spec::new(
        "fsw",
        &[
            ("count", false),
            ("repeated", false),
            ("all-repeated", false),
            ("unique", false),
            ("ignore-case", false),
            ("skip-fields", true),
            ("skip-chars", true),
            ("check-chars", true),
            ("zero-terminated", false),
        ],
    );
    let parsed = match parse(ctx, args, &spec) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let (mut count, mut repeated, mut all_repeated, mut unique_only, mut fold) =
        (false, false, false, false, false);
    let (mut skip_fields, mut skip_chars, mut check_chars) = (0_usize, 0_usize, None::<usize>);
    let mut delimiter = b'\n';
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
        let number = || -> Result<usize, i32> {
            val.parse().map_err(|_| {
                ctx.error(format!("{val}: invalid number"));
                2
            })
        };
        match flag.as_str() {
            "-c" | "--count" => count = true,
            "-d" | "--repeated" => repeated = true,
            "-D" | "--all-repeated" => all_repeated = true,
            "-u" | "--unique" => unique_only = true,
            "-i" | "--ignore-case" => fold = true,
            "-z" | "--zero-terminated" => delimiter = 0,
            "-f" | "--skip-fields" => match number() {
                Ok(n) => skip_fields = n,
                Err(code) => return code,
            },
            "-s" | "--skip-chars" => match number() {
                Ok(n) => skip_chars = n,
                Err(code) => return code,
            },
            "-w" | "--check-chars" => match number() {
                Ok(n) => check_chars = Some(n),
                Err(code) => return code,
            },
            other => return ctx.unsupported(other),
        }
    }
    let input = operands.first().cloned().unwrap_or_else(|| "-".into());
    let mut bytes = Vec::new();
    if let Err(e) = open(ctx, &input).and_then(|mut r| r.read_to_end(&mut bytes)) {
        ctx.io_error(&input, &e);
        return 1;
    }
    let lines: Vec<&[u8]> = split_lines(&bytes, delimiter)
        .into_iter()
        .map(|l| super::util::chomp(l, delimiter))
        .collect();
    let compared = |line: &[u8]| -> Vec<u8> {
        let mut at = 0;
        for _ in 0..skip_fields {
            while line.get(at).copied().is_some_and(is_blank) {
                at = at.saturating_add(1);
            }
            while line.get(at).is_some_and(|&b| !is_blank(b)) {
                at = at.saturating_add(1);
            }
        }
        at = at.saturating_add(skip_chars).min(line.len());
        let rest = line.get(at..).unwrap_or_default();
        let rest = match check_chars {
            Some(n) => rest.get(..n.min(rest.len())).unwrap_or_default(),
            None => rest,
        };
        if fold {
            rest.to_ascii_lowercase()
        } else {
            rest.to_vec()
        }
    };
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(first) = lines.get(i) {
        let key = compared(first);
        let mut j = i.saturating_add(1);
        while lines.get(j).is_some_and(|l| compared(l) == key) {
            j = j.saturating_add(1);
        }
        let n = j.saturating_sub(i);
        let group = lines.get(i..j).unwrap_or_default();
        if all_repeated {
            if n > 1 {
                for line in group {
                    out.extend_from_slice(line);
                    out.push(delimiter);
                }
            }
        } else if !(repeated && n == 1 || unique_only && n > 1) {
            if count {
                out.extend_from_slice(format!("{n:>7} ").as_bytes());
            }
            out.extend_from_slice(first);
            out.push(delimiter);
        }
        i = j;
    }
    if let Some(file) = operands.get(1) {
        let written = ctx
            .host
            .open_write(&ctx.resolve(file), WriteMode::Truncate, ctx.run)
            .and_then(|mut w| w.write_all(&out).and_then(|()| w.flush()));
        return match written {
            Ok(()) => 0,
            Err(e) => {
                ctx.io_error(file, &e);
                1
            }
        };
    }
    match ctx.out(&out) {
        Ok(()) => 0,
        Err(Stop(code)) => code,
    }
}
