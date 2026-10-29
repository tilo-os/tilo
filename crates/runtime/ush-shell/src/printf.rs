//! `printf`, as POSIX and bash write it: the format is used again until the
//! arguments run out, `%b` expands escapes, `%q` quotes, and `-v` assigns.
//! Output is bytes, so `\xE2\x98\xA0` is the three bytes it names.

use std::fmt::Write as _;

use crate::shell::{Exec, Shell};

pub(crate) fn printf(sh: &mut Shell, args: &[String]) -> Exec {
    let (var, args) = match args {
        [flag, name, rest @ ..] if flag == "-v" => (Some(name.clone()), rest),
        [dashes, rest @ ..] if dashes == "--" => (None, rest),
        _ => (None, args),
    };
    let Some((format, mut rest)) = args.split_first() else {
        sh.error("printf: usage: printf [-v var] format [arguments]");
        return Ok(2);
    };
    let mut out = Vec::new();
    let mut status = 0;
    loop {
        let before = rest.len();
        let pass = format_once(format, &mut rest, &mut out);
        if let Some(message) = &pass.error {
            sh.error(format!("printf: {message}"));
            status = 1;
        }
        if pass.stop || pass.error.is_some() && pass.fatal {
            break;
        }
        if rest.is_empty() || rest.len() == before {
            break;
        }
    }
    match var {
        Some(name) => {
            let text = String::from_utf8_lossy(&out).into_owned();
            let Some(assignment) = crate::builtins::literal_assignment(&format!("{name}={text}"))
            else {
                sh.error(format!("printf: `{name}': not a valid identifier"));
                return Ok(2);
            };
            sh.assign(&assignment)?;
        }
        None => {
            if !sh.out_bytes(&out) {
                return Ok(1);
            }
        }
    }
    Ok(status)
}

#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is one of printf's independent flags"
)]
struct Spec {
    left: bool,
    plus: bool,
    space: bool,
    alt: bool,
    zero: bool,
    width: Option<usize>,
    precision: Option<usize>,
}

/// What one pass over the format came to.
#[derive(Default)]
struct Pass {
    /// `\c` ended all output.
    stop: bool,
    /// The last thing that went wrong, to report.
    error: Option<String>,
    /// The error ends the command (a bad conversion), rather than only
    /// failing it (a bad number).
    fatal: bool,
}

fn format_once(format: &str, args: &mut &[String], out: &mut Vec<u8>) -> Pass {
    let mut pass = Pass::default();
    let mut chars = format.chars().peekable();
    let next_arg = |args: &mut &[String]| -> Option<String> {
        let (first, rest) = args.split_first()?;
        *args = rest;
        Some(first.clone())
    };
    while let Some(c) = chars.next() {
        if c == '\\' {
            let Some(esc) = escape(&mut chars) else {
                pass.stop = true;
                return pass;
            };
            out.extend(crate::expand::ansi_c_bytes(&esc));
            continue;
        }
        if c != '%' {
            let mut buf = [0_u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        if chars.peek() == Some(&'%') {
            chars.next();
            out.push(b'%');
            continue;
        }
        let mut spec = Spec::default();
        while let Some(&f) = chars.peek() {
            match f {
                '-' => spec.left = true,
                '+' => spec.plus = true,
                ' ' => spec.space = true,
                '#' => spec.alt = true,
                '0' => spec.zero = true,
                _ => break,
            }
            chars.next();
        }
        let number = |chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
                      args: &mut &[String]| {
            if chars.peek() == Some(&'*') {
                chars.next();
                return next_arg(args)
                    .and_then(|a| a.parse::<i64>().ok())
                    .map(|n| usize::try_from(n.max(0)).unwrap_or(0));
            }
            let mut digits = String::new();
            while chars.peek().is_some_and(char::is_ascii_digit) {
                digits.extend(chars.next());
            }
            digits.parse().ok()
        };
        spec.width = number(&mut chars, args);
        if chars.peek() == Some(&'.') {
            chars.next();
            spec.precision = Some(number(&mut chars, args).unwrap_or(0));
        }
        // Length modifiers mean nothing here.
        while chars.peek().is_some_and(|c| "hlLjzt".contains(*c)) {
            chars.next();
        }
        let Some(conv) = chars.next() else {
            pass.error = Some("`%': missing format character".into());
            pass.fatal = true;
            return pass;
        };
        if conv == '(' {
            pass.error = Some("`%(…)T' (time formats) is not supported".into());
            pass.fatal = true;
            return pass;
        }
        if !"sbqcdiouxXeEfFgG".contains(conv) {
            pass.error = Some(format!("`{conv}': invalid format character"));
            pass.fatal = true;
            return pass;
        }
        let arg = next_arg(args);
        let kind = match conv {
            'd' | 'i' | 'o' | 'u' | 'x' | 'X' => Kind::Integer,
            'e' | 'E' | 'f' | 'F' | 'g' | 'G' => Kind::Float,
            _ => Kind::Text,
        };
        let (text, stop) = convert(&spec, conv, arg, &mut pass);
        out.extend(pad(&spec, text, kind));
        if stop {
            pass.stop = true;
            return pass;
        }
    }
    pass
}

/// The text of one conversion, unpadded, and whether a `\c` in a `%b`
/// argument ended all output.
fn convert(spec: &Spec, conv: char, arg: Option<String>, pass: &mut Pass) -> (Vec<u8>, bool) {
    let text = match conv {
        's' => {
            let s = arg.unwrap_or_default();
            match spec.precision {
                Some(p) => s.chars().take(p).collect::<String>().into_bytes(),
                None => s.into_bytes(),
            }
        }
        'b' => {
            let s = arg.unwrap_or_default();
            let (bytes, stop) = expand_b(&s);
            return (bytes, stop);
        }
        'q' => backslash_quote(&arg.unwrap_or_default()).into_bytes(),
        'c' => arg
            .and_then(|a| a.chars().next())
            .map(|c| c.to_string().into_bytes())
            .unwrap_or_default(),
        'd' | 'i' | 'o' | 'u' | 'x' | 'X' => {
            let (n, error) = number_arg(arg.as_deref());
            if let Some(e) = error {
                pass.error = Some(e);
            }
            integer(spec, conv, n).into_bytes()
        }
        _ => {
            let raw = arg.unwrap_or_default();
            let value = if raw.trim().is_empty() {
                0.0
            } else {
                raw.trim().parse::<f64>().unwrap_or_else(|_| {
                    pass.error = Some(format!("{raw}: invalid number"));
                    0.0
                })
            };
            float(spec, conv, value).into_bytes()
        }
    };
    (text, false)
}

/// The rest of a backslash escape in the format, with its backslash, or
/// `None` for `\c`, which ends all output.
fn escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<String> {
    let mut esc = String::from('\\');
    if let Some(n) = chars.next() {
        esc.push(n);
        if n == 'c' {
            return None;
        }
        let more = match n {
            'x' | '0'..='7' => 2,
            'u' => 4,
            'U' => 8,
            _ => 0,
        };
        for _ in 0..more {
            let ok = chars.peek().is_some_and(|d| {
                if n.is_digit(8) {
                    d.is_digit(8)
                } else {
                    d.is_ascii_hexdigit()
                }
            });
            if !ok {
                break;
            }
            esc.extend(chars.next());
        }
    }
    Some(esc)
}

/// A number argument: decimal, `0x` hex, `0` octal, or `'c` for a
/// character's code. Text after the number is an error, but the number is
/// still used, as bash does.
fn number_arg(arg: Option<&str>) -> (i64, Option<String>) {
    let Some(arg) = arg else { return (0, None) };
    let trimmed = arg.trim_start();
    if let Some(c) = trimmed
        .strip_prefix('\'')
        .or_else(|| trimmed.strip_prefix('"'))
    {
        return (
            c.chars().next().map_or(0, |c| i64::from(u32::from(c))),
            None,
        );
    }
    if trimmed.is_empty() {
        return (0, None);
    }
    if let Some(n) = crate::arith::parse_number(trimmed) {
        return (n, None);
    }
    // The longest leading decimal number.
    let sign = usize::from(trimmed.starts_with(['+', '-']));
    let digits = trimmed
        .get(sign..)
        .unwrap_or("")
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    let end = sign.saturating_add(digits);
    let value = trimmed
        .get(..end)
        .and_then(crate::arith::parse_number)
        .unwrap_or(0);
    (value, Some(format!("{arg}: invalid number")))
}

fn integer(spec: &Spec, conv: char, n: i64) -> String {
    let mut digits = match conv {
        'o' => format!("{n:o}"),
        'u' => format!("{}", n.cast_unsigned()),
        'x' => format!("{n:x}"),
        'X' => format!("{n:X}"),
        _ => n.unsigned_abs().to_string(),
    };
    if let Some(p) = spec.precision {
        while digits.len() < p {
            digits.insert(0, '0');
        }
    }
    if spec.alt && n != 0 {
        match conv {
            'o' => digits.insert(0, '0'),
            'x' => digits.insert_str(0, "0x"),
            'X' => digits.insert_str(0, "0X"),
            _ => {}
        }
    }
    if matches!(conv, 'd' | 'i') {
        if n < 0 {
            digits.insert(0, '-');
        } else if spec.plus {
            digits.insert(0, '+');
        } else if spec.space {
            digits.insert(0, ' ');
        }
    }
    digits
}

/// `floor(log10(|value|))`, the exponent `%g` chooses its form by.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a finite double's decimal exponent is within ±324; `as` saturates the rest"
)]
fn decimal_exponent(value: f64) -> i32 {
    value.abs().log10().floor() as i32
}

fn float(spec: &Spec, conv: char, value: f64) -> String {
    let precision = spec.precision.unwrap_or(6);
    let mut text = match conv {
        'e' | 'E' => {
            let s = format!("{value:.precision$e}");
            // Rust writes `1.5e2`; C writes `1.5e+02`.
            let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
            let exp: i32 = exp.parse().unwrap_or(0);
            let sign = if exp < 0 { '-' } else { '+' };
            let t = format!("{mantissa}e{sign}{:02}", exp.unsigned_abs());
            if conv == 'E' { t.to_uppercase() } else { t }
        }
        'g' | 'G' => {
            let p = if precision == 0 { 1 } else { precision };
            let exp = if value == 0.0 {
                0
            } else {
                decimal_exponent(value)
            };
            let p_i32 = i32::try_from(p).unwrap_or(i32::MAX);
            let t = if exp < -4 || exp >= p_i32 {
                let e = Spec {
                    precision: Some(p.saturating_sub(1)),
                    ..Spec::default()
                };
                let t = float(&e, 'e', value);
                if spec.alt {
                    t
                } else {
                    let (m, e) = t.split_once('e').unwrap_or((&t, ""));
                    let m = if m.contains('.') {
                        m.trim_end_matches('0').trim_end_matches('.')
                    } else {
                        m
                    };
                    format!("{m}e{e}")
                }
            } else {
                let decimals = usize::try_from(p_i32.saturating_sub(1).saturating_sub(exp).max(0))
                    .unwrap_or(0);
                let t = format!("{value:.decimals$}");
                if t.contains('.') && !spec.alt {
                    t.trim_end_matches('0').trim_end_matches('.').to_string()
                } else {
                    t
                }
            };
            if conv == 'G' { t.to_uppercase() } else { t }
        }
        _ => format!("{value:.precision$}"),
    };
    if spec.alt && !text.contains('.') && matches!(conv, 'f' | 'F') {
        text.push('.');
    }
    if value >= 0.0 {
        if spec.plus {
            text.insert(0, '+');
        } else if spec.space {
            text.insert(0, ' ');
        }
    }
    text
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Integer,
    Float,
}

fn pad(spec: &Spec, text: Vec<u8>, kind: Kind) -> Vec<u8> {
    let Some(width) = spec.width else { return text };
    let len = String::from_utf8_lossy(&text).chars().count();
    if len >= width {
        return text;
    }
    let fill = width.saturating_sub(len);
    if spec.left {
        let mut out = text;
        out.extend(std::iter::repeat_n(b' ', fill));
        return out;
    }
    // A precision turns off `0` for integers, not for floats.
    let zero = match kind {
        Kind::Text => false,
        Kind::Integer => spec.precision.is_none(),
        Kind::Float => true,
    };
    if spec.zero && zero {
        let (sign, digits) = match text.first() {
            Some(c @ (b'-' | b'+' | b' ')) => (vec![*c], text.get(1..).unwrap_or(&[]).to_vec()),
            _ => (Vec::new(), text),
        };
        let mut out = sign;
        out.extend(std::iter::repeat_n(b'0', fill));
        out.extend(digits);
        return out;
    }
    let mut out: Vec<u8> = std::iter::repeat_n(b' ', fill).collect();
    out.extend(text);
    out
}

/// `%b`'s escapes; `\c` ends all output. Octal is written `\0nnn` here.
fn expand_b(text: &str) -> (Vec<u8>, bool) {
    let (text, stop) = match text.find("\\c") {
        Some(cut) => (text.get(..cut).unwrap_or(""), true),
        None => (text, false),
    };
    (
        crate::expand::ansi_c_bytes(&text.replace("\\0", "\\")),
        stop,
    )
}

/// `%q`: `value` with each character the shell would read specially
/// escaped by a backslash, as bash writes it; `$'…'` when it holds control
/// characters.
fn backslash_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".into();
    }
    if value.chars().any(char::is_control) {
        let mut out = String::from("$'");
        for c in value.chars() {
            match c {
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                '\'' => out.push_str("\\'"),
                '\\' => out.push_str("\\\\"),
                c if c.is_control() => {
                    let _ = write!(out, "\\{:03o}", u32::from(c));
                }
                c => out.push(c),
            }
        }
        out.push('\'');
        return out;
    }
    let mut out = String::new();
    for c in value.chars() {
        if !(c.is_alphanumeric() || "_./:=,+@%^-".contains(c)) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
