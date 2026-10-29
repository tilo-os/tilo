//! Brace expansion, bash's: `a{b,c}d` is `abd acd`, `{1..3}` is `1 2 3`,
//! `{01..10}` keeps the width, and a brace that is quoted, part of `${…}`,
//! or holds neither a comma nor a range is text.

/// Every word `text` stands for, in order; `text` itself when it has no
/// brace expression.
pub(crate) fn expand(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0_usize;
    while let Some(&c) = chars.get(i) {
        match c {
            '\\' => i = i.saturating_add(2),
            '\'' => i = skip_quote(&chars, i, '\''),
            '"' => i = skip_quote(&chars, i, '"'),
            '$' if chars.get(i.saturating_add(1)) == Some(&'{') => {
                i = matching(&chars, i.saturating_add(1))
                    .map_or(chars.len(), |j| j.saturating_add(1));
            }
            '{' => {
                if let Some(words) = at(&chars, i) {
                    return words;
                }
                i = i.saturating_add(1);
            }
            _ => i = i.saturating_add(1),
        }
    }
    vec![text.to_string()]
}

/// The words of the brace expression opening at `open`, if it is one.
fn at(chars: &[char], open: usize) -> Option<Vec<String>> {
    let close = matching(chars, open)?;
    let pre: String = chars.get(..open)?.iter().collect();
    let post: String = chars.get(close.saturating_add(1)..)?.iter().collect();
    let inner = chars.get(open.saturating_add(1)..close)?;
    let commas = top_level_commas(inner);
    let alternatives: Vec<String> = if commas.is_empty() {
        sequence(&inner.iter().collect::<String>())?
    } else {
        let mut parts = Vec::new();
        let mut start = 0_usize;
        for comma in commas.iter().copied().chain(std::iter::once(inner.len())) {
            parts.push(inner.get(start..comma).unwrap_or(&[]).iter().collect());
            start = comma.saturating_add(1);
        }
        parts
    };
    Some(
        alternatives
            .into_iter()
            .flat_map(|alt| expand(&format!("{pre}{alt}{post}")))
            .collect(),
    )
}

/// Past the quote that opens at `i`.
fn skip_quote(chars: &[char], i: usize, quote: char) -> usize {
    let mut j = i.saturating_add(1);
    while let Some(&c) = chars.get(j) {
        if c == '\\' && quote == '"' {
            j = j.saturating_add(2);
            continue;
        }
        if c == quote {
            return j.saturating_add(1);
        }
        j = j.saturating_add(1);
    }
    chars.len()
}

/// The `}` that closes the `{` at `open`.
fn matching(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0_usize;
    let mut j = open;
    while let Some(&c) = chars.get(j) {
        match c {
            '\\' => {
                j = j.saturating_add(2);
                continue;
            }
            '\'' | '"' => {
                j = skip_quote(chars, j, c);
                continue;
            }
            '{' => depth = depth.saturating_add(1),
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
        j = j.saturating_add(1);
    }
    None
}

fn top_level_commas(inner: &[char]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut depth = 0_usize;
    let mut j = 0_usize;
    while let Some(&c) = inner.get(j) {
        match c {
            '\\' => {
                j = j.saturating_add(2);
                continue;
            }
            '\'' | '"' => {
                j = skip_quote(inner, j, c);
                continue;
            }
            '{' => depth = depth.saturating_add(1),
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => out.push(j),
            _ => {}
        }
        j = j.saturating_add(1);
    }
    out
}

/// `1..5`, `5..1..2`, `a..e`, `01..10`.
fn sequence(inner: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = inner.split("..").collect();
    let (start, end, step) = match parts.as_slice() {
        [a, b] => (*a, *b, None),
        [a, b, c] => (*a, *b, Some(*c)),
        _ => return None,
    };
    let step: i64 = match step {
        Some(s) => s.parse().ok()?,
        None => 1,
    };
    let step = step.unsigned_abs().max(1);
    if let (Ok(a), Ok(b)) = (start.parse::<i64>(), end.parse::<i64>()) {
        let padded = |t: &str| {
            let digits = t.trim_start_matches(['-', '+']);
            digits.len() > 1 && digits.starts_with('0')
        };
        let width = if padded(start) || padded(end) {
            start.len().max(end.len())
        } else {
            0
        };
        let mut out = Vec::new();
        let mut n = a;
        loop {
            out.push(if width > 0 {
                if n < 0 {
                    format!("-{:0w$}", n.unsigned_abs(), w = width.saturating_sub(1))
                } else {
                    format!("{n:0width$}")
                }
            } else {
                n.to_string()
            });
            let next = if a <= b {
                n.checked_add_unsigned(step).filter(|x| *x <= b)
            } else {
                n.checked_sub_unsigned(step).filter(|x| *x >= b)
            };
            match next {
                Some(x) => n = x,
                None => break,
            }
        }
        return Some(out);
    }
    let one = |t: &str| {
        let mut cs = t.chars();
        let c = cs.next()?;
        (cs.next().is_none() && c.is_ascii_alphabetic()).then_some(c)
    };
    let (a, b) = (u32::from(one(start)?), u32::from(one(end)?));
    let step = usize::try_from(step).unwrap_or(1);
    let range: Vec<u32> = if a <= b {
        (a..=b).step_by(step).collect()
    } else {
        (b..=a).rev().step_by(step).collect()
    };
    Some(
        range
            .into_iter()
            .filter_map(char::from_u32)
            .map(String::from)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_ranges_and_widths() {
        assert_eq!(expand("a{b,c}d"), ["abd", "acd"]);
        assert_eq!(expand("{1..3}"), ["1", "2", "3"]);
        assert_eq!(expand("-{01..03}-"), ["-01-", "-02-", "-03-"]);
        assert_eq!(expand("{a,{1...3}}"), ["a", "{1...3}"]);
        assert_eq!(expand("{x}"), ["{x}"]);
        assert_eq!(expand("'{a,b}'"), ["'{a,b}'"]);
        assert_eq!(expand("${x}{a,b}"), ["${x}a", "${x}b"]);
        assert_eq!(expand("{a..e..2}"), ["a", "c", "e"]);
        assert_eq!(expand("{a,b}{1,2}"), ["a1", "a2", "b1", "b2"]);
    }
}
