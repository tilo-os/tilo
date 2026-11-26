//! What a log keeps of a tool call of a turn.
//!
//! A call's input is its caller's words: a person's sentence, a document's text, a search, and
//! where a model errs a password. What a tool answers is what the workspace holds. So no value
//! of an argument and nothing of an answer stands in a line of the log, at any level. A line
//! keeps the call's tool, each argument by its name and the bytes of its value, which is where
//! making the call again by hand begins, and of a refusal its sentence without anything the
//! call wrote.

use serde_json::Value;

/// How much of a refusal's sentence a line keeps.
pub const LOGGED_CHARS: usize = 300;

/// What stands in a refusal's sentence where something the call wrote was cut out of it.
const CUT: &str = "…";

/// The arguments of a call as a line names them: each by its name and the bytes of its value
/// as the call wrote it, `name:bytes`, with a comma between, and `-` for a call without any.
/// Only a name a tool can declare is written: a key that has not the form of one, and an
/// input that names nothing, is `?`.
#[must_use]
pub fn arguments(input: &Value) -> String {
    let named = match input {
        Value::Null => return "-".to_owned(),
        Value::Object(named) if named.is_empty() => return "-".to_owned(),
        Value::Object(named) => named,
        unnamed => return format!("?:{}", unnamed.to_string().len()),
    };
    named
        .iter()
        .map(|(name, value)| {
            let name = if plain(name) { name.as_str() } else { "?" };
            format!("{name}:{}", value.to_string().len())
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// `tool` as a line names it: a name a tool can have, or `other` for anything else a model wrote
/// in its place.
#[must_use]
pub fn offered_name(tool: &str) -> &str {
    let plain = !tool.is_empty()
        && tool.len() <= 48
        && tool
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if plain { tool } else { "other" }
}

/// Whether `name` has the form of an argument's name as a tool declares one: ASCII letters,
/// digits, `_` and `-`.
fn plain(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// What the log keeps of the refusal `said` to a call that wrote `input`: its first sentence,
/// and in it [`CUT`] for everything the call wrote.
///
/// The sentence ends at the first line break, at the first full stop that another sentence
/// follows, or at the first semicolon, where a refusal goes on to what there is instead; at
/// most [`LOGGED_CHARS`] of it are kept. What the call wrote is cut twice over: each value
/// whole wherever the sentence quotes it, then every stretch of letters and digits, and every
/// stretch of other signs, that stands somewhere in a value, whatever the case of its letters.
/// So a part of a value, as a refusal names one step of a path, goes as the whole value does.
#[must_use]
pub fn refusal(said: &str, input: &Value) -> String {
    let line = said.lines().next().unwrap_or_default();
    let line = line.split_once(". ").map_or(line, |(first, _)| first);
    let line = line.split_once(';').map_or(line, |(first, _)| first);
    let mut wrote = Vec::new();
    written(input, true, &mut wrote);
    wrote.sort_by_key(|value| std::cmp::Reverse(value.len()));
    let mut sentence = line.to_owned();
    for value in &wrote {
        sentence = sentence.replace(value.as_str(), CUT);
    }
    let wrote: Vec<String> = wrote.iter().map(|value| value.to_lowercase()).collect();
    let mut kept = String::new();
    for (made, stretch) in stretches(&sentence) {
        let theirs = matches!(made, Made::Words | Made::Signs) && {
            let stretch = stretch.to_lowercase();
            wrote.iter().any(|value| value.contains(stretch.as_str()))
        };
        kept.push_str(if theirs { CUT } else { stretch.as_str() });
    }
    let twice = CUT.repeat(2);
    while kept.contains(twice.as_str()) {
        kept = kept.replace(twice.as_str(), CUT);
    }
    kept.chars().take(LOGGED_CHARS).collect()
}

/// Everything the call wrote into `value`, each piece as a refusal may quote it: a text as it
/// is and as JSON writes it, a number and a yes or no as JSON writes them, and every key below
/// the arguments' own names. An argument's own name is the call's too where it has not the
/// form of one ([`plain`]); a name of that form stands in the line by itself.
fn written(value: &Value, named_first: bool, wrote: &mut Vec<String>) {
    match value {
        Value::Null => {}
        Value::Bool(_) | Value::Number(_) => wrote.push(value.to_string()),
        Value::String(text) => {
            if text.is_empty() {
                return;
            }
            wrote.push(text.clone());
            let quoted = value.to_string();
            if let Some(inner) = quoted
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                && inner != text
            {
                wrote.push(inner.to_owned());
            }
        }
        Value::Array(items) => {
            for item in items {
                written(item, false, wrote);
            }
        }
        Value::Object(named) => {
            for (name, inner) in named {
                // An argument's own name in the form of one stands in the line by itself.
                let own = named_first && plain(name);
                if !(own || name.is_empty()) {
                    wrote.push(name.clone());
                }
                written(inner, false, wrote);
            }
        }
    }
}

/// What a stretch of a sentence is made of.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Made {
    /// Letters and digits.
    Words,
    /// What parts one word from the next.
    Gap,
    /// Any other sign.
    Signs,
    /// What was cut out already.
    Cut,
}

/// `text` in stretches, each as long as what it is made of goes on.
fn stretches(text: &str) -> Vec<(Made, String)> {
    let mut found: Vec<(Made, String)> = Vec::new();
    for sign in text.chars() {
        let made = if CUT.contains(sign) {
            Made::Cut
        } else if sign.is_alphanumeric() {
            Made::Words
        } else if sign.is_whitespace() {
            Made::Gap
        } else {
            Made::Signs
        };
        match found.last_mut() {
            Some((last, stretch)) if *last == made => stretch.push(sign),
            _ => found.push((made, String::from(sign))),
        }
    }
    found
}
