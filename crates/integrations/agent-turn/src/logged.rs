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
/// whole wherever the sentence quotes it, then every stretch of letters and digits that stands
/// somewhere in a value, whatever the case of its letters, and every stretch of other signs
/// that stands in a value and touches what is cut.
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
    let found = stretches(&sentence);
    let stands_in_a_value = |stretch: &str| {
        let stretch = stretch.to_lowercase();
        wrote.iter().any(|value| value.contains(stretch.as_str()))
    };
    let words_theirs: Vec<bool> = found
        .iter()
        .map(|(made, stretch)| match made {
            Made::Words => stands_in_a_value(stretch),
            Made::Cut => true,
            Made::Gap | Made::Signs => false,
        })
        .collect();
    let mut kept = String::new();
    for (at, ((made, stretch), theirs_at)) in found.iter().zip(&words_theirs).enumerate() {
        // Signs are the call's only beside what the call wrote: the `_` of the server's own
        // `invalid_arguments` stays, the `/` between two steps of the call's path goes.
        let theirs = match made {
            Made::Words => *theirs_at,
            Made::Signs => {
                stands_in_a_value(stretch)
                    && (at
                        .checked_sub(1)
                        .is_some_and(|before| words_theirs.get(before).copied().unwrap_or(false))
                        || words_theirs
                            .get(at.saturating_add(1))
                            .copied()
                            .unwrap_or(false))
            }
            Made::Gap | Made::Cut => false,
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

#[cfg(test)]
mod tests {
    use super::{LOGGED_CHARS, arguments, offered_name, refusal};
    use serde_json::{Value, json};

    #[test]
    fn a_line_names_each_argument_with_the_bytes_of_its_value_and_no_value() {
        let line = arguments(&json!({
            "path": "notes/Plums#rename",
            "args": {"title": "For Ada"},
        }));
        let mut named: Vec<&str> = line.split(',').collect();
        named.sort_unstable();
        assert_eq!(named, ["args:19", "path:20"]);
        assert!(!line.contains("Plums") && !line.contains("Ada"), "{line}");
        // A call without arguments, and one whose input names nothing.
        assert_eq!(arguments(&json!({})), "-");
        assert_eq!(arguments(&Value::Null), "-");
        assert_eq!(arguments(&json!("only words")), "?:12");
        // A key that has not the form of a name is its caller's words, and is not written.
        assert_eq!(arguments(&json!({"the tide is out": 1})), "?:1");
    }

    #[test]
    fn a_refusals_sentence_is_kept_without_anything_the_call_wrote() {
        // As an agent tool refuses: what the call named, a step of its path at a time, then
        // what there is instead, then the call that works on a line of its own.
        let named = "notes/Plums has no action rename/title; close matches: rename\n\
            The call that works: tilo_do {\"path\":\"notes/Plums#rename\",\"args\":{\"title\":\"For Ada\"}}";
        let asked = json!({"path": "notes/Plums#rename/title", "args": {"title": "For Ada"}});
        assert_eq!(refusal(named, &asked), "… has … action …");
        // A sentence that quotes nothing of its call is kept as the server wrote it.
        assert_eq!(
            refusal(
                "invalid_arguments: required argument `title` was not supplied",
                &json!({"path": "so.tilo.tasks#new_task", "args": {}})
            ),
            "invalid_arguments: required argument `title` was not supplied"
        );
        // A refusal in three parts: its sentence, then what was wrong with what the caller
        // wrote, then the arguments to ask again with. The sentence loses the words the call
        // wrote too.
        let secret = "failed: action_failed: a question for a secret says where the secret \
            belongs. In your detail: a place is written with what it is reached by, and yours \
            is `mail.plum.example`. Ask again with exactly these arguments: \
            {\"text\":\"The password of ada@plum.example\",\"kind\":\"secret\"}";
        let asked = json!({
            "path": "chat/Messages#ask",
            "args": {
                "text": "The password of ada@plum.example",
                "kind": "secret",
                "detail": {"place": "mail.plum.example"},
            },
        });
        assert_eq!(
            refusal(secret, &asked),
            "failed: action_failed: … question for … … says where … … belongs"
        );
        // A value inside a longer word, a piece of a value in another case, and a text as
        // JSON writes it.
        assert_eq!(
            refusal(
                "nothing here is called superheronlike",
                &json!({"query": "heron"})
            ),
            "nothing here is called super…like"
        );
        assert_eq!(
            refusal("no plumz in Notes", &json!({"path": "notes/Plumz"})),
            "… … in …"
        );
        assert_eq!(
            refusal(
                "could not write \"line one\\nline two\"",
                &json!({"args": {"text": "line one\nline two"}})
            ),
            "could not write \"…\""
        );
        // An empty value cuts nothing, and a long sentence is kept to the log's bound.
        assert_eq!(refusal("refused", &json!({"text": ""})), "refused");
        assert_eq!(
            refusal(&"a".repeat(1000), &json!({})).chars().count(),
            LOGGED_CHARS
        );
    }

    #[test]
    fn a_name_no_tool_can_have_is_not_written() {
        for name in ["tilo_ls", "answer_with_view", "learned"] {
            assert_eq!(offered_name(name), name);
        }
        for name in [
            "",
            "tilo_ls notes/Budget",
            "Tilo_Read",
            "a/b",
            "the-quarter's-figures",
            &"x".repeat(49),
        ] {
            assert_eq!(offered_name(name), "other", "{name:?}");
        }
    }
}
