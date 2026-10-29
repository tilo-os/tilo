//! What the tier-1 commands share: GNU-style option parsing, reading an
//! operand as a stream, local times, sizes for people, and the glob matcher
//! `find -name` and `grep --include` use.

use std::io::{self, BufRead, Read};

use super::Ctx;
use crate::io::Input;

/// One command-line argument, after option parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    /// An option by its spelling (`-n`, `--lines`), with its value if it
    /// takes one.
    Flag(String, Option<String>),
    Operand(String),
}

/// How a command's options are spelled.
pub struct Spec<'a> {
    /// The short options that take a value (`n` for `head -n 3`).
    pub short_values: &'a str,
    /// Every long option the command knows, without `--`, and whether it
    /// takes a value. An unambiguous prefix stands for the whole name, as in
    /// GNU getopt.
    pub longs: &'a [(&'a str, bool)],
    /// Whether an argument that reads as a negative number is an operand
    /// (`seq -5 -1`), not a cluster of short options.
    pub numbers_are_operands: bool,
}

impl Spec<'_> {
    pub const fn new<'a>(short_values: &'a str, longs: &'a [(&'a str, bool)]) -> Spec<'a> {
        Spec {
            short_values,
            longs,
            numbers_are_operands: false,
        }
    }
}

/// Split `args` into options and operands the way GNU getopt does: options
/// may come after operands, `--` ends them, and a lone `-` is an operand.
/// A missing value is a usage error (status 2), already reported.
pub fn parse(ctx: &Ctx<'_>, args: &[String], spec: &Spec<'_>) -> Result<Vec<Arg>, i32> {
    let mut out = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if arg == "--" {
            out.extend(rest.by_ref().cloned().map(Arg::Operand));
            break;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (long, None),
            };
            let known = resolve_long(spec.longs, name);
            let (full, takes) = known.unwrap_or((name, false));
            let value = if takes && inline.is_none() {
                let Some(v) = rest.next() else {
                    ctx.error(format!("option '--{full}' requires an argument"));
                    return Err(2);
                };
                Some(v.clone())
            } else {
                inline
            };
            out.push(Arg::Flag(format!("--{full}"), value));
            continue;
        }
        let is_number = spec.numbers_are_operands
            && arg.len() > 1
            && arg
                .chars()
                .nth(1)
                .is_some_and(|c| c.is_ascii_digit() || c == '.');
        let Some(cluster) = arg
            .strip_prefix('-')
            .filter(|c| !c.is_empty() && !is_number)
        else {
            out.push(Arg::Operand(arg.clone()));
            continue;
        };
        for (at, c) in cluster.char_indices() {
            if spec.short_values.contains(c) {
                let after = cluster.get(at.saturating_add(c.len_utf8())..).unwrap_or("");
                let value = if after.is_empty() {
                    if let Some(v) = rest.next() {
                        v.clone()
                    } else {
                        ctx.error(format!("option requires an argument -- '{c}'"));
                        return Err(2);
                    }
                } else {
                    after.to_string()
                };
                out.push(Arg::Flag(format!("-{c}"), Some(value)));
                break;
            }
            out.push(Arg::Flag(format!("-{c}"), None));
        }
    }
    Ok(out)
}

fn resolve_long<'a>(longs: &[(&'a str, bool)], name: &str) -> Option<(&'a str, bool)> {
    if let Some(&exact) = longs.iter().find(|(n, _)| *n == name) {
        return Some(exact);
    }
    let mut matches = longs.iter().filter(|(n, _)| n.starts_with(name));
    let first = matches.next()?;
    matches.next().is_none().then_some(*first)
}

/// A flag's value, which [`parse`] guarantees for options that take one.
pub fn value(v: Option<&String>) -> &str {
    v.map_or("", String::as_str)
}

/// Standard input as a reader.
pub struct Stdin(pub Input);

impl Read for Stdin {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

/// `operand` as a stream: standard input for `-`, else the file. A
/// directory is an error, as the GNU tools say it.
pub fn open(ctx: &Ctx<'_>, operand: &str) -> io::Result<Box<dyn Read + Send>> {
    if operand == "-" {
        return Ok(Box::new(Stdin(ctx.io.stdin.clone())));
    }
    let path = ctx.resolve(operand);
    if ctx.host.metadata(&path)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::IsADirectory,
            "Is a directory",
        ));
    }
    ctx.host.open_read(&path)
}

/// Hand each line of `reader` to `each`, with its newline if it had one.
/// Stops at the first error `each` returns.
pub fn lines<E: From<io::Error>>(
    reader: impl Read,
    delimiter: u8,
    mut each: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), E> {
    let mut reader = io::BufReader::new(reader);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(delimiter, &mut line)? == 0 {
            return Ok(());
        }
        each(&line)?;
    }
}

/// `bytes` split into lines, each with its terminator if it had one.
pub fn split_lines(bytes: &[u8], delimiter: u8) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (at, &b) in bytes.iter().enumerate() {
        if b == delimiter {
            let end = at.saturating_add(1);
            out.push(bytes.get(start..end).unwrap_or_default());
            start = end;
        }
    }
    if start < bytes.len() {
        out.push(bytes.get(start..).unwrap_or_default());
    }
    out
}

/// `line` without its terminator.
pub fn chomp(line: &[u8], delimiter: u8) -> &[u8] {
    line.strip_suffix(&[delimiter]).unwrap_or(line)
}

/// A calendar date and time of day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    pub year: i64,
    /// 1 to 12.
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// Seconds since the epoch as a calendar time (proleptic Gregorian), after
/// Howard Hinnant's `civil_from_days`.
pub fn civil(secs: i64) -> Civil {
    let days = secs.div_euclid(86_400);
    let of_day = secs.rem_euclid(86_400);
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = doe
        .saturating_sub(doe / 1460)
        .saturating_add(doe / 36_524)
        .saturating_sub(doe / 146_096)
        / 365;
    let doy = doe.saturating_sub(
        yoe.saturating_mul(365)
            .saturating_add(yoe / 4)
            .saturating_sub(yoe / 100),
    );
    let mp = doy.saturating_mul(5).saturating_add(2) / 153;
    let day = doy
        .saturating_sub(mp.saturating_mul(153).saturating_add(2) / 5)
        .saturating_add(1);
    let month = if mp < 10 {
        mp.saturating_add(3)
    } else {
        mp.saturating_sub(9)
    };
    let year = yoe
        .saturating_add(era.saturating_mul(400))
        .saturating_add(i64::from(month <= 2));
    Civil {
        year,
        month: u32::try_from(month).unwrap_or(1),
        day: u32::try_from(day).unwrap_or(1),
        hour: u32::try_from(of_day / 3600).unwrap_or(0),
        minute: u32::try_from(of_day % 3600 / 60).unwrap_or(0),
        second: u32::try_from(of_day % 60).unwrap_or(0),
    }
}

/// The English abbreviation of a month, 1 to 12, as `ls -l` prints it.
pub fn month_name(month: u32) -> &'static str {
    const NAMES: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    NAMES
        .get(month.saturating_sub(1) as usize)
        .copied()
        .unwrap_or("???")
}

/// `at` in the host's local time.
pub fn local(ctx: &Ctx<'_>, at: i64) -> Civil {
    civil(at.saturating_add(i64::from(ctx.host.utc_offset(at))))
}

/// A UTC offset as `+0100`.
pub fn offset(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let minutes = seconds.unsigned_abs() / 60;
    format!("{sign}{:02}{:02}", minutes / 60, minutes % 60)
}

/// `at` as `stat` and `diff -u` print it: `2026-10-28 22:55:17.000000000
/// +0100`. Hosts report whole seconds.
pub fn full_time(ctx: &Ctx<'_>, at: i64) -> String {
    let t = local(ctx, at);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.000000000 {}",
        t.year,
        t.month,
        t.day,
        t.hour,
        t.minute,
        t.second,
        offset(ctx.host.utc_offset(at))
    )
}

/// A size as `ls -h` and `du -h` print it: powers of 1024, rounded up, one
/// decimal below ten.
pub fn human(size: u64) -> String {
    const UNITS: [char; 8] = ['K', 'M', 'G', 'T', 'P', 'E', 'Z', 'Y'];
    if size < 1024 {
        return size.to_string();
    }
    // Tenths of a unit, rounded up, so 1025 bytes is 1.1K as GNU says.
    let mut unit = 0_usize;
    let mut base = 1024_u128;
    let size = u128::from(size);
    loop {
        let tenths = size.saturating_mul(10).div_ceil(base);
        if tenths < 100 {
            return format!(
                "{}.{}{}",
                tenths / 10,
                tenths % 10,
                UNITS.get(unit).copied().unwrap_or('?')
            );
        }
        let whole = size.div_ceil(base);
        if whole < 1024 || unit.saturating_add(1) >= UNITS.len() {
            return format!("{whole}{}", UNITS.get(unit).copied().unwrap_or('?'));
        }
        unit = unit.saturating_add(1);
        base = base.saturating_mul(1024);
    }
}

/// Whether `name` matches the shell glob `pattern` (`*`, `?`, `[...]`,
/// backslash escapes). As `fnmatch` without flags, `*` matches `/` too,
/// which is what `find -path` wants.
pub fn glob(pattern: &str, name: &str, fold_case: bool) -> bool {
    let fold = |s: &str| -> Vec<char> {
        if fold_case {
            s.chars().flat_map(char::to_lowercase).collect()
        } else {
            s.chars().collect()
        }
    };
    glob_chars(&fold(pattern), &fold(name))
}

fn glob_chars(p: &[char], n: &[char]) -> bool {
    // Backtracking over the last `*` only: linear for the patterns people
    // write, and never exponential.
    let (mut pi, mut ni) = (0_usize, 0_usize);
    let mut star: Option<(usize, usize)> = None;
    loop {
        if let Some(&pc) = p.get(pi) {
            if pc == '*' {
                star = Some((pi, ni));
                pi = pi.saturating_add(1);
                continue;
            }
            if let Some(&nc) = n.get(ni) {
                let step = match pc {
                    '?' => Some(1),
                    '[' => bracket(p, pi, nc),
                    '\\' => p
                        .get(pi.saturating_add(1))
                        .and_then(|&e| (e == nc).then_some(2)),
                    c => (c == nc).then_some(1),
                };
                if let Some(width) = step {
                    pi = pi.saturating_add(width);
                    ni = ni.saturating_add(1);
                    continue;
                }
            }
        } else if ni == n.len() {
            return true;
        }
        match star {
            Some((sp, sn)) if sn < n.len() => {
                star = Some((sp, sn.saturating_add(1)));
                pi = sp.saturating_add(1);
                ni = sn.saturating_add(1);
            }
            _ => return false,
        }
    }
}

/// Match `c` against the bracket expression at `p[start]`; the width of the
/// expression in pattern characters if it matches. An unclosed `[` is a
/// literal.
fn bracket(p: &[char], start: usize, c: char) -> Option<usize> {
    let mut i = start.saturating_add(1);
    let negate = matches!(p.get(i), Some('!' | '^'));
    if negate {
        i = i.saturating_add(1);
    }
    let mut matched = false;
    let mut first = true;
    loop {
        let &pc = p.get(i)?;
        if pc == ']' && !first {
            break;
        }
        first = false;
        if pc == '[' && p.get(i.saturating_add(1)) == Some(&':') {
            let rest = p.get(i.saturating_add(2)..).unwrap_or_default();
            if let Some(end) = rest.windows(2).position(|w| w == [':', ']']) {
                let class: String = rest.get(..end).unwrap_or_default().iter().collect();
                matched |= in_class(&class, c);
                i = i.saturating_add(end).saturating_add(4);
                continue;
            }
        }
        let lo = if pc == '\\' {
            i = i.saturating_add(1);
            *p.get(i)?
        } else {
            pc
        };
        if p.get(i.saturating_add(1)) == Some(&'-')
            && p.get(i.saturating_add(2)).is_some_and(|&h| h != ']')
        {
            let hi = *p.get(i.saturating_add(2))?;
            matched |= lo <= c && c <= hi;
            i = i.saturating_add(3);
        } else {
            matched |= lo == c;
            i = i.saturating_add(1);
        }
    }
    if matched == negate {
        return None;
    }
    Some(i.saturating_add(1).saturating_sub(start))
}

/// Whether `c` is in the POSIX character class `class` (`alpha`, `digit`, …).
pub fn in_class(class: &str, c: char) -> bool {
    match class {
        "alpha" => c.is_alphabetic(),
        "digit" => c.is_ascii_digit(),
        "alnum" => c.is_alphanumeric(),
        "upper" => c.is_uppercase(),
        "lower" => c.is_lowercase(),
        "space" => c.is_whitespace(),
        "blank" => c == ' ' || c == '\t',
        "punct" => c.is_ascii_punctuation(),
        "print" => !c.is_control(),
        "graph" => !c.is_control() && !c.is_whitespace(),
        "cntrl" => c.is_control(),
        "xdigit" => c.is_ascii_hexdigit(),
        _ => false,
    }
}

/// A count as GNU tools parse it: digits, and for `head`/`tail` a multiplier
/// suffix (`k`, `K`, `M`, `G`, `b`, `kB`, `MB`, …).
pub fn parse_size(text: &str) -> Option<u64> {
    let digits_end = text
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map_or(text.len(), |(at, _)| at);
    let (digits, suffix) = text.split_at(digits_end);
    let number: u64 = digits.parse().ok()?;
    let multiplier: u64 = match suffix {
        "" => 1,
        "b" => 512,
        "kB" => 1000,
        "K" | "k" | "KiB" => 1024,
        "MB" => 1_000_000,
        "M" | "MiB" => 1 << 20,
        "GB" => 1_000_000_000,
        "G" | "GiB" => 1 << 30,
        "TB" => 1_000_000_000_000,
        "T" | "TiB" => 1 << 40,
        _ => return None,
    };
    number.checked_mul(multiplier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_like_fnmatch() {
        assert!(glob("*.rs", "lib.rs", false));
        assert!(!glob("*.rs", "lib.rsx", false));
        assert!(glob("a?c", "abc", false));
        assert!(glob("[a-c]x", "bx", false));
        assert!(!glob("[!a-c]x", "bx", false));
        assert!(glob("*/b/*", "a/b/c", false));
        assert!(glob("*.TXT", "x.txt", true));
        assert!(glob("[[:digit:]]*", "1a", false));
        assert!(glob("\\*", "*", false));
    }

    #[test]
    fn dates_come_out_right() {
        assert_eq!(
            civil(1_700_000_000),
            Civil {
                year: 2023,
                month: 11,
                day: 14,
                hour: 22,
                minute: 13,
                second: 20
            }
        );
        assert_eq!(civil(0).year, 1970);
        assert_eq!(civil(-1).year, 1969);
    }

    #[test]
    fn sizes_for_people() {
        assert_eq!(human(1023), "1023");
        assert_eq!(human(1024), "1.0K");
        assert_eq!(human(1025), "1.1K");
        assert_eq!(human(10 * 1024), "10K");
        assert_eq!(human(1_048_576), "1.0M");
    }
}
