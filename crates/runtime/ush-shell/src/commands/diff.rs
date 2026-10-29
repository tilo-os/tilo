//! `diff`: compare two files line by line, in GNU's normal or unified
//! format.
//!
//! The comparison is Myers' O(ND) algorithm, the one GNU diff is built on,
//! over lines interned to numbers after the common head and tail are set
//! aside. Comparing two directories needs `-r`, which this slice does not
//! do, so it is refused.
//!
//! The changes are worked out as hunks first, then rendered: for people in
//! the format asked for; for an agent as the hunks alone, one line naming the
//! two sides (`--- a +++ b`), then each hunk's `@@ -3,2 +3,2 @@` and its
//! `-`/`+` lines, with no lines of context unless `-u`/`-U` asked for them.
//! Past [`LINES`](super::agent::LINES) lines it counts, and says how many it
//! left out.

use std::collections::HashMap;
use std::io::Read;

use super::agent::{self, Lines};
use super::files::base_name;
use super::util::{Arg, Spec, full_time, parse, split_lines, value};
use super::{Ctx, Render, Stop};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Normal,
    Unified(usize),
    Brief,
}

#[expect(clippy::struct_excessive_bools, reason = "one switch per GNU flag")]
struct Opts {
    format: Format,
    fold_case: bool,
    space_change: bool,
    all_space: bool,
    text: bool,
    report_same: bool,
    labels: Vec<String>,
}

/// One input: its name as given, its label, its time and its lines.
struct Side {
    name: String,
    modified: i64,
    bytes: Vec<u8>,
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per GNU flag, read in one place"
)]
pub fn diff(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let spec = Spec::new(
        "U",
        &[
            ("unified", false),
            ("brief", false),
            ("ignore-case", false),
            ("ignore-space-change", false),
            ("ignore-all-space", false),
            ("text", false),
            ("report-identical-files", false),
            ("label", true),
            ("normal", false),
            ("color", false),
            ("recursive", false),
            ("new-file", false),
            ("context", false),
            ("side-by-side", false),
        ],
    );
    let parsed = match parse(ctx, args, &spec) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let mut o = Opts {
        format: Format::Normal,
        fold_case: false,
        space_change: false,
        all_space: false,
        text: false,
        report_same: false,
        labels: Vec::new(),
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
            "-u" => o.format = Format::Unified(3),
            "--unified" => match v.as_deref().map(str::parse::<usize>) {
                None => o.format = Format::Unified(3),
                Some(Ok(n)) => o.format = Format::Unified(n),
                Some(Err(_)) => {
                    ctx.error(format!("invalid context length '{}'", value(v.as_ref())));
                    return 2;
                }
            },
            "-U" => {
                let Ok(n) = value(v.as_ref()).parse() else {
                    ctx.error(format!("invalid context length '{}'", value(v.as_ref())));
                    return 2;
                };
                o.format = Format::Unified(n);
            }
            "-q" | "--brief" => o.format = Format::Brief,
            "--normal" => o.format = Format::Normal,
            "-i" | "--ignore-case" => o.fold_case = true,
            "-b" | "--ignore-space-change" => o.space_change = true,
            "-w" | "--ignore-all-space" => o.all_space = true,
            "-a" | "--text" => o.text = true,
            "-s" | "--report-identical-files" => o.report_same = true,
            "--label" => o.labels.push(value(v.as_ref()).to_string()),
            "--color" if matches!(v.as_deref(), Some("never" | "auto")) => {}
            other => return ctx.unsupported(other),
        }
    }
    let [left, right] = operands.as_slice() else {
        if operands.len() < 2 {
            ctx.error(format!(
                "missing operand after '{}'",
                operands.last().map_or("diff", String::as_str)
            ));
        } else {
            ctx.error(format!(
                "extra operand '{}'",
                operands.get(2).map_or("", String::as_str)
            ));
        }
        return 2;
    };
    let left_dir = is_dir(ctx, left);
    let right_dir = is_dir(ctx, right);
    let (left, right) = match (left_dir, right_dir) {
        (true, true) => {
            ctx.error("comparing two directories needs -r, which is not supported");
            return 2;
        }
        (true, false) => (join_name(left, right), right.clone()),
        (false, true) => (left.clone(), join_name(right, left)),
        (false, false) => (left.clone(), right.clone()),
    };
    let (a, b) = match (read_side(ctx, &left), read_side(ctx, &right)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) => {
            ctx.io_error(&left, &e);
            return 2;
        }
        (_, Err(e)) => {
            ctx.io_error(&right, &e);
            return 2;
        }
    };
    match compare(ctx, &o, &a, &b) {
        Ok(status) | Err(Stop(status)) => status,
    }
}

fn is_dir(ctx: &Ctx<'_>, operand: &str) -> bool {
    operand != "-"
        && ctx
            .host
            .metadata(&ctx.resolve(operand))
            .is_ok_and(|m| m.is_dir())
}

/// `dir/name-of-file`: a directory compared with a file means the file of
/// the same name in it.
fn join_name(dir: &str, file: &str) -> String {
    let base = base_name(file);
    if dir.ends_with('/') {
        format!("{dir}{base}")
    } else {
        format!("{dir}/{base}")
    }
}

fn read_side(ctx: &Ctx<'_>, name: &str) -> std::io::Result<Side> {
    if name == "-" {
        return Ok(Side {
            name: name.into(),
            modified: ctx.host.now(),
            bytes: ctx.io.stdin.read_all()?,
        });
    }
    let path = ctx.resolve(name);
    let meta = ctx.host.metadata(&path)?;
    let mut bytes = Vec::new();
    ctx.host.open_read(&path)?.read_to_end(&mut bytes)?;
    Ok(Side {
        name: name.into(),
        modified: meta.modified,
        bytes,
    })
}

/// What a line is compared as, after `-i`, `-b` and `-w`.
fn key(o: &Opts, line: &[u8]) -> Vec<u8> {
    let (body, newline) = match line.strip_suffix(b"\n") {
        Some(b) => (b, true),
        None => (line, false),
    };
    let mut out = Vec::with_capacity(body.len().saturating_add(1));
    let mut in_space = false;
    for &c in body {
        let c = if o.fold_case {
            c.to_ascii_lowercase()
        } else {
            c
        };
        if c.is_ascii_whitespace() {
            if o.all_space {
                continue;
            }
            if o.space_change {
                in_space = true;
                continue;
            }
        } else if in_space {
            if !out.is_empty() {
                out.push(b' ');
            }
            in_space = false;
        }
        out.push(c);
    }
    if newline {
        out.push(b'\n');
    }
    out
}

/// A run of changed lines: `a[a0..a1]` became `b[b0..b1]`.
#[derive(Debug, Clone, Copy)]
struct Change {
    a0: usize,
    a1: usize,
    b0: usize,
    b1: usize,
}

fn compare(ctx: &Ctx<'_>, o: &Opts, a: &Side, b: &Side) -> Result<i32, Stop> {
    let label_a = o.labels.first().unwrap_or(&a.name).clone();
    let label_b = o.labels.get(1).unwrap_or(&b.name).clone();
    if a.bytes == b.bytes {
        if o.report_same {
            ctx.out_str(&format!("Files {label_a} and {label_b} are identical\n"))?;
        }
        return Ok(0);
    }
    let binary = !o.text && (a.bytes.contains(&0) || b.bytes.contains(&0));
    if binary {
        ctx.out_str(&format!("Binary files {label_a} and {label_b} differ\n"))?;
        return Ok(1);
    }
    let la = split_lines(&a.bytes, b'\n');
    let lb = split_lines(&b.bytes, b'\n');
    let mut ids: HashMap<Vec<u8>, u32> = HashMap::new();
    let mut intern = |line: &[u8]| -> u32 {
        let next = u32::try_from(ids.len()).unwrap_or(u32::MAX);
        *ids.entry(key(o, line)).or_insert(next)
    };
    let ka: Vec<u32> = la.iter().map(|l| intern(l)).collect();
    let kb: Vec<u32> = lb.iter().map(|l| intern(l)).collect();
    let changes = changes(&ka, &kb);
    if changes.is_empty() {
        if o.report_same {
            ctx.out_str(&format!("Files {label_a} and {label_b} are identical\n"))?;
        }
        return Ok(0);
    }
    match o.format {
        Format::Normal | Format::Unified(_) if ctx.render == Render::Agent => {
            let context = match o.format {
                Format::Unified(n) => n,
                _ => 0,
            };
            ctx.out_str(&format!("--- {label_a} +++ {label_b}\n"))?;
            let mut lines = Lines::new(ctx, agent::LINES);
            for hunk in hunks(&changes, &la, &lb, context) {
                lines.group(Some(hunk.header()));
                for (marker, line) in &hunk.lines {
                    let (body, newline) = match line.strip_suffix(b"\n") {
                        Some(body) => (body, true),
                        None => (*line, false),
                    };
                    lines.line(&format!(
                        "{}{}",
                        char::from(*marker),
                        agent::clip(body, None)
                    ))?;
                    if !newline {
                        lines.line("\\ No newline at end of file")?;
                    }
                }
            }
            lines.finish("lines", ("hunk", "hunks"))?;
        }
        Format::Brief => {
            ctx.out_str(&format!("Files {label_a} and {label_b} differ\n"))?;
        }
        Format::Normal => normal(ctx, &changes, &la, &lb)?,
        Format::Unified(context) => {
            let time_a = full_time(ctx, a.modified);
            let time_b = full_time(ctx, b.modified);
            ctx.out_str(&format!(
                "--- {label_a}\t{time_a}\n+++ {label_b}\t{time_b}\n"
            ))?;
            unified(ctx, &changes, &la, &lb, context)?;
        }
    }
    Ok(1)
}

/// `line` printed after `marker`, with GNU's note when it has no newline.
fn line_out(out: &mut Vec<u8>, marker: &[u8], line: &[u8]) {
    out.extend_from_slice(marker);
    if let Some(body) = line.strip_suffix(b"\n") {
        out.extend_from_slice(body);
        out.push(b'\n');
    } else {
        out.extend_from_slice(line);
        out.extend_from_slice(b"\n\\ No newline at end of file\n");
    }
}

/// A 1-based line range in the normal format: `3` or `3,5`.
fn range(from: usize, to: usize) -> String {
    if to.saturating_sub(from) == 1 {
        to.to_string()
    } else {
        format!("{},{to}", from.saturating_add(1))
    }
}

fn normal(ctx: &Ctx<'_>, changes: &[Change], la: &[&[u8]], lb: &[&[u8]]) -> Result<(), Stop> {
    for c in changes {
        let mut out = Vec::new();
        let head = match (c.a1 > c.a0, c.b1 > c.b0) {
            (true, true) => format!("{}c{}\n", range(c.a0, c.a1), range(c.b0, c.b1)),
            (true, false) => format!("{}d{}\n", range(c.a0, c.a1), c.b0),
            _ => format!("{}a{}\n", c.a0, range(c.b0, c.b1)),
        };
        out.extend_from_slice(head.as_bytes());
        for line in la.get(c.a0..c.a1).unwrap_or_default() {
            line_out(&mut out, b"< ", line);
        }
        if c.a1 > c.a0 && c.b1 > c.b0 {
            out.extend_from_slice(b"---\n");
        }
        for line in lb.get(c.b0..c.b1).unwrap_or_default() {
            line_out(&mut out, b"> ", line);
        }
        ctx.out(&out)?;
    }
    Ok(())
}

/// A unified range: `start,count`, `start` alone for one line, and for none
/// the line before.
fn unified_range(from: usize, count: usize) -> String {
    match count {
        0 => format!("{from},0"),
        1 => from.saturating_add(1).to_string(),
        _ => format!("{},{count}", from.saturating_add(1)),
    }
}

/// One hunk of a unified diff: where it is on each side, and its lines,
/// each with its marker (` `, `-` or `+`).
struct Hunk<'l> {
    a_from: usize,
    a_len: usize,
    b_from: usize,
    b_len: usize,
    lines: Vec<(u8, &'l [u8])>,
}

impl Hunk<'_> {
    /// `@@ -3,2 +3,2 @@`
    fn header(&self) -> String {
        format!(
            "@@ -{} +{} @@",
            unified_range(self.a_from, self.a_len),
            unified_range(self.b_from, self.b_len)
        )
    }
}

/// The hunks of a unified diff with `context` lines around each change.
fn hunks<'l>(
    changes: &[Change],
    la: &[&'l [u8]],
    lb: &[&'l [u8]],
    context: usize,
) -> Vec<Hunk<'l>> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(first) = changes.get(i) {
        // Changes closer than twice the context share one hunk.
        let mut last_index = i;
        while let (Some(cur), Some(next)) = (
            changes.get(last_index),
            changes.get(last_index.saturating_add(1)),
        ) {
            if next.a0.saturating_sub(cur.a1) > context.saturating_mul(2) {
                break;
            }
            last_index = last_index.saturating_add(1);
        }
        let last = changes.get(last_index).copied().unwrap_or(*first);
        let lead = context.min(first.a0);
        let a_from = first.a0.saturating_sub(lead);
        let b_from = first.b0.saturating_sub(lead);
        let a_to = last.a1.saturating_add(context).min(la.len());
        let trail = a_to.saturating_sub(last.a1);
        let b_to = last.b1.saturating_add(trail).min(lb.len());
        let mut lines = Vec::new();
        let mut at = a_from;
        for c in changes.get(i..=last_index).unwrap_or_default() {
            for line in la.get(at..c.a0).unwrap_or_default() {
                lines.push((b' ', *line));
            }
            for line in la.get(c.a0..c.a1).unwrap_or_default() {
                lines.push((b'-', *line));
            }
            for line in lb.get(c.b0..c.b1).unwrap_or_default() {
                lines.push((b'+', *line));
            }
            at = c.a1;
        }
        for line in la.get(at..a_to).unwrap_or_default() {
            lines.push((b' ', *line));
        }
        out.push(Hunk {
            a_from,
            a_len: a_to.saturating_sub(a_from),
            b_from,
            b_len: b_to.saturating_sub(b_from),
            lines,
        });
        i = last_index.saturating_add(1);
    }
    out
}

fn unified(
    ctx: &Ctx<'_>,
    changes: &[Change],
    la: &[&[u8]],
    lb: &[&[u8]],
    context: usize,
) -> Result<(), Stop> {
    for hunk in hunks(changes, la, lb, context) {
        let mut out = format!("{}\n", hunk.header()).into_bytes();
        for (marker, line) in &hunk.lines {
            line_out(&mut out, &[*marker], line);
        }
        ctx.out(&out)?;
    }
    Ok(())
}

/// The runs of lines that differ between `a` and `b`, in order.
fn changes(a: &[u32], b: &[u32]) -> Vec<Change> {
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let a_rest = a.get(prefix..).unwrap_or_default();
    let b_rest = b.get(prefix..).unwrap_or_default();
    let suffix = a_rest
        .iter()
        .rev()
        .zip(b_rest.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let a_mid = a_rest
        .get(..a_rest.len().saturating_sub(suffix))
        .unwrap_or_default();
    let b_mid = b_rest
        .get(..b_rest.len().saturating_sub(suffix))
        .unwrap_or_default();
    // Which lines of each side are kept, from the edit script.
    let mut a_kept = vec![false; a_mid.len()];
    let mut b_kept = vec![false; b_mid.len()];
    for (x, y) in myers(a_mid, b_mid) {
        if let Some(k) = a_kept.get_mut(x) {
            *k = true;
        }
        if let Some(k) = b_kept.get_mut(y) {
            *k = true;
        }
    }
    let mut out = Vec::new();
    let (mut x, mut y) = (0_usize, 0_usize);
    loop {
        while a_kept.get(x) == Some(&true) && b_kept.get(y) == Some(&true) {
            x = x.saturating_add(1);
            y = y.saturating_add(1);
        }
        let (x0, y0) = (x, y);
        while a_kept.get(x) == Some(&false) {
            x = x.saturating_add(1);
        }
        while b_kept.get(y) == Some(&false) {
            y = y.saturating_add(1);
        }
        if x == x0 && y == y0 {
            break;
        }
        out.push(Change {
            a0: x0.saturating_add(prefix),
            a1: x.saturating_add(prefix),
            b0: y0.saturating_add(prefix),
            b1: y.saturating_add(prefix),
        });
    }
    out
}

/// The pairs of equal lines (`a` index, `b` index) of a shortest edit
/// script, by Myers' greedy algorithm with the furthest-reaching paths of
/// each step kept for the walk back.
#[expect(
    clippy::many_single_char_names,
    reason = "the names of Myers' paper: n, m, d, k, x, y, v"
)]
fn myers(a: &[u32], b: &[u32]) -> Vec<(usize, usize)> {
    let n = isize::try_from(a.len()).unwrap_or(isize::MAX);
    let m = isize::try_from(b.len()).unwrap_or(isize::MAX);
    let max = n.saturating_add(m);
    let offset = max.saturating_add(1);
    let size = usize::try_from(offset.saturating_mul(2).saturating_add(1)).unwrap_or(0);
    let mut v = vec![0_isize; size];
    let at = |v: &[isize], k: isize| -> isize {
        usize::try_from(k.saturating_add(offset))
            .ok()
            .and_then(|i| v.get(i).copied())
            .unwrap_or(0)
    };
    let set = |v: &mut [isize], k: isize, x: isize| {
        if let Some(slot) = usize::try_from(k.saturating_add(offset))
            .ok()
            .and_then(|i| v.get_mut(i))
        {
            *slot = x;
        }
    };
    let same = |x: isize, y: isize| -> bool {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return false;
        };
        matches!((a.get(x), b.get(y)), (Some(p), Some(q)) if p == q)
    };
    // `trace[d]` is the part of `v` step `d` started from: diagonals
    // `-(d+1)..=d+1`, all the walk back reads.
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut found = false;
    let mut d = 0_isize;
    while d <= max && !found {
        let lo = d.saturating_add(1).saturating_neg();
        trace.push((lo..=d.saturating_add(1)).map(|k| at(&v, k)).collect());
        let mut k = d.saturating_neg();
        while k <= d {
            let down = k == d.saturating_neg()
                || (k != d && at(&v, k.wrapping_sub(1)) < at(&v, k.wrapping_add(1)));
            let mut x = if down {
                at(&v, k.wrapping_add(1))
            } else {
                at(&v, k.wrapping_sub(1)).saturating_add(1)
            };
            let mut y = x.saturating_sub(k);
            while x < n && y < m && same(x, y) {
                x = x.saturating_add(1);
                y = y.saturating_add(1);
            }
            set(&mut v, k, x);
            if x >= n && y >= m {
                found = true;
                break;
            }
            k = k.saturating_add(2);
        }
        d = d.saturating_add(1);
    }
    let mut pairs = Vec::new();
    let (mut x, mut y) = (n, m);
    for (step, saved) in trace.iter().enumerate().rev() {
        let d = isize::try_from(step).unwrap_or(isize::MAX);
        let lo = d.saturating_add(1).saturating_neg();
        let get = |k: isize| -> isize {
            usize::try_from(k.saturating_sub(lo))
                .ok()
                .and_then(|i| saved.get(i).copied())
                .unwrap_or(0)
        };
        let k = x.saturating_sub(y);
        let prev_k = if k == d.saturating_neg()
            || (k != d && get(k.wrapping_sub(1)) < get(k.wrapping_add(1)))
        {
            k.wrapping_add(1)
        } else {
            k.wrapping_sub(1)
        };
        let prev_x = get(prev_k);
        let prev_y = prev_x.saturating_sub(prev_k);
        while x > prev_x && y > prev_y {
            x = x.saturating_sub(1);
            y = y.saturating_sub(1);
            if let (Ok(px), Ok(py)) = (usize::try_from(x), usize::try_from(y)) {
                pairs.push((px, py));
            }
        }
        x = prev_x;
        y = prev_y;
    }
    pairs.reverse();
    pairs
}
