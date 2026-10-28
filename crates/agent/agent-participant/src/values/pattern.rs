//! A part of a value taken out by a pattern (`{{…|match:INV-\d+}}`): a
//! reference number, an amount or an address a model would otherwise copy
//! out of a sentence by hand.

use regex::RegexBuilder;

/// The longest pattern a step may name, in bytes.
const MOST_PATTERN: usize = 256;

/// The most memory one compiled pattern may take. `regex` matches in time
/// linear in the text, so this and [`MOST_PATTERN`] bound all it costs.
const MOST_COMPILED: usize = 1 << 20;

/// The first group of `pattern` that took part in its first match in
/// `text`, else that whole match.
///
/// # Errors
///
/// The pattern is empty, too long or not a pattern, or matches nothing.
pub(super) fn first_match(text: &str, pattern: &str) -> Result<String, String> {
    if pattern.is_empty() {
        return Err("|match: names no pattern".into());
    }
    if pattern.len() > MOST_PATTERN {
        return Err(format!(
            "the pattern is longer than {MOST_PATTERN} characters"
        ));
    }
    let regex = RegexBuilder::new(pattern)
        .size_limit(MOST_COMPILED)
        .dfa_size_limit(MOST_COMPILED)
        .build()
        .map_err(|why| {
            let why = why.to_string();
            let reason = why
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .unwrap_or_default()
                .trim()
                .trim_start_matches("error: ")
                .to_owned();
            format!("`{pattern}` is not a pattern: {reason}")
        })?;
    let found = regex
        .captures(text)
        .and_then(|captures| {
            captures
                .iter()
                .skip(1)
                .flatten()
                .next()
                .or_else(|| captures.get(0))
        })
        .map(|found| found.as_str().to_owned())
        .filter(|found| !found.trim().is_empty());
    found.ok_or_else(|| {
        format!(
            "`{pattern}` matches nothing in \"{}\"",
            super::select::brief(text)
        )
    })
}
