//! The names a caller writes, read as what they mean: an app by any spelling a
//! model writes for it, an action by the ids close to one nobody offers, a
//! choice by the label a person reads, and a resource by the name a path
//! writes for it.
//!
//! An app is named exactly by its id, by the last part of its id and by the name the listing
//! shows it by. A model asked to work in the word processor writes `word-processor`,
//! `word_processor`, `Word Processor`, `WordProcessor`, `Word` or `so.tilo.word_processor`, and
//! each of those is that one app: a name is compared in lower case and without the hyphens,
//! underscores and blanks that part its words, and the start of a name stands for it where one
//! app alone begins so.
//!
//! A name is only ever read among the apps the caller was listed already, so reading it more
//! freely finds no app a stricter spelling could not have named; what the caller may do with
//! the app it finds is decided where it always was. A name that fits several apps equally well
//! chooses none of them.

use crate::ActionDescriptor;
use serde_json::Value;

/// What a bound UI resource's id starts with (ui-core's publication prefix).
pub const BOUND_PREFIX: &str = "tilo-ui-bound:v1:";

/// A name written into a path: `/`, `#` and `\` escaped.
#[must_use]
pub fn path_name(name: &str) -> String {
    let mut written = String::with_capacity(name.len());
    for character in name.chars() {
        if matches!(character, '/' | '#' | '\\') {
            written.push('\\');
        }
        written.push(character);
    }
    written
}

/// The logical id a bound UI resource's id carries in hex at its end
/// (`tilo-ui-bound:v1:<publication>:<instance>:<hex>`): what the app calls
/// the resource, which survives the publication the rest of the id names.
#[must_use]
pub fn bound_key(id: &str) -> Option<String> {
    let hex = id.strip_prefix(BOUND_PREFIX)?;
    let hex = hex.rsplit(':').next()?;
    if hex.is_empty() || hex.len() % 2 != 0 {
        return None;
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(hex.get(at..at.checked_add(2)?)?, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    String::from_utf8(bytes)
        .ok()
        .filter(|key| !key.is_empty() && !key.chars().any(char::is_control))
}

/// The fewest letters the start of a name stands for the name with.
const START_CHARS: usize = 3;

/// `name` as names are compared: in lower case, without the hyphens, underscores and blanks
/// that part its words.
#[must_use]
pub fn folded(name: &str) -> String {
    name.trim()
        .chars()
        .filter(|character| !matches!(character, '-' | '_' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

/// The app `name` means among `apps`, each an id with the name the listing shows it by (empty
/// where it shows none): the app whose id it is, else whose id ends in it, else that is listed
/// by it, else the one whose name begins with it. `None` when it names none of them. A name
/// with a dot in it is an id and is compared with ids whole.
///
/// # Errors
/// `name` fits several apps equally well: they are named, and none is chosen.
pub fn app_meant(name: &str, apps: &[(&str, &str)]) -> Result<Option<String>, String> {
    let wanted = folded(name);
    if wanted.is_empty() {
        return Ok(None);
    }
    let dotted = wanted.contains('.');
    let begins = wanted.chars().count() >= START_CHARS;
    let last = |app_id: &str| folded(app_id.rsplit('.').next().unwrap_or(app_id));
    let fits = |how: u8, app_id: &str, label: &str| match how {
        0 => folded(app_id) == wanted,
        1 => !dotted && last(app_id) == wanted,
        2 => !dotted && !label.is_empty() && folded(label) == wanted,
        _ => {
            !dotted
                && begins
                && (last(app_id).starts_with(&wanted)
                    || (!label.is_empty() && folded(label).starts_with(&wanted)))
        }
    };
    for how in 0..4u8 {
        let mut meant: Vec<&str> = Vec::new();
        for &(app_id, label) in apps {
            if fits(how, app_id, label) && !meant.contains(&app_id) {
                meant.push(app_id);
            }
        }
        match meant.as_slice() {
            [] => {}
            [one] => return Ok(Some((*one).to_owned())),
            several => {
                return Err(format!(
                    "{name} names several apps: {}; name one",
                    several.join(", ")
                ));
            }
        }
    }
    Ok(None)
}

/// `arguments` with each parameter's value written as the label a person reads
/// replaced by the value the app takes, whatever its case or spacing
/// (`In review` for `in-review`, a status or a choice of a kind of thing alike).
pub fn bind_choices(descriptor: &ActionDescriptor, arguments: &mut serde_json::Map<String, Value>) {
    for param in &descriptor.params {
        let Some(given) = arguments
            .get(param.name.as_ref())
            .and_then(Value::as_str)
            .map(plain)
        else {
            continue;
        };
        let choice = param
            .choices
            .iter()
            .find(|choice| plain(choice) == given)
            .map(|choice| Value::String(choice.to_string()))
            .or_else(|| {
                forms(param.schema.as_deref()?)
                    .into_iter()
                    .find(|(name, _)| plain(name) == given)
                    .map(|(_, value)| value)
            });
        if let Some(choice) = choice {
            arguments.insert(param.name.to_string(), choice);
        }
    }
}

/// The action ids closest to `name`, best first, at most `limit`: those it is
/// part of, one of whose words it is, or whose start it shares.
#[must_use]
pub fn close_matches(name: &str, ids: &[String], limit: usize) -> Vec<String> {
    let lower = name.to_lowercase();
    let mut scored: Vec<(u8, &String)> = ids
        .iter()
        .filter_map(|id| {
            let named = id.to_lowercase();
            let rank = if named.contains(&lower) {
                0
            } else if lower.contains(&named) {
                1
            } else if shared_start(&named, &lower) >= 3 {
                2
            } else {
                return None;
            };
            Some((rank, id))
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, id)| id.clone())
        .collect()
}

/// How many bytes the start of `a` and `b` share, to the last `-`.
fn shared_start(a: &str, b: &str) -> usize {
    let common = a
        .bytes()
        .zip(b.bytes())
        .take_while(|(left, right)| left == right)
        .count();
    a.get(..common)
        .and_then(|start| start.rfind('-'))
        .unwrap_or(common)
}

/// `text` as a value is compared to a parameter's choices: its letters alone,
/// in one case, so `In review`, `In Review` and `in-review` are one value.
fn plain(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

/// The values a parameter's schema offers as alternatives, each as the name a
/// person reads it by and the value the app takes (`{"title": "In review",
/// "const": "in-review"}`, or a one-field object with the constant).
fn forms(schema: &Value) -> Vec<(String, Value)> {
    let Some(alternatives) = schema
        .get("anyOf")
        .or_else(|| schema.get("oneOf"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut forms = Vec::new();
    for alternative in alternatives {
        let Some(fields) = alternative.as_object() else {
            continue;
        };
        let constant = fields.get("const").cloned().or_else(|| {
            let only = fields.get("required")?.as_array()?.first()?.as_str()?;
            fields.get("properties")?.get(only)?.get("const").cloned()
        });
        let name = fields
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| constant.as_ref().and_then(Value::as_str).map(str::to_owned));
        if let (Some(name), Some(constant)) = (name, constant) {
            forms.push((name, constant));
        }
    }
    forms
}

#[cfg(test)]
mod tests {
    use super::{app_meant, bound_key, close_matches, folded};
    use crate::tree::CLOSE_MATCHES;

    const APPS: &[(&str, &str)] = &[
        ("so.tilo.word-processor", "Word Processor"),
        ("so.tilo.notes", "Notes"),
        ("so.tilo.tasks", "Tasks"),
        ("so.tilo.file-explorer", "Files"),
        ("so.tilo.video", "Video"),
        ("so.tilo.video-editor", "Video Editor"),
        ("so.tilo.word-processor", "Word Processor"),
    ];

    #[test]
    fn a_name_is_its_app_in_every_spelling_a_model_writes() {
        for spelled in [
            "word-processor",
            "word_processor",
            "word processor",
            "Word Processor",
            "WordProcessor",
            "WORD-PROCESSOR",
            "Word",
            " word_processor ",
            "so.tilo.word-processor",
            "so.tilo.word_processor",
            "So.Tilo.Word-Processor",
        ] {
            assert_eq!(
                app_meant(spelled, APPS),
                Ok(Some("so.tilo.word-processor".to_owned())),
                "{spelled}"
            );
        }
        for spelled in [
            "files",
            "Files",
            "file_explorer",
            "File Explorer",
            "FILE-EXPLORER",
        ] {
            assert_eq!(
                app_meant(spelled, APPS),
                Ok(Some("so.tilo.file-explorer".to_owned())),
                "{spelled}"
            );
        }
        assert_eq!(folded(" Word_Processor "), "wordprocessor");
    }

    #[test]
    fn a_name_that_fits_several_apps_chooses_none_of_them() {
        let several = app_meant("video", APPS).expect_err("two apps begin so");
        assert!(
            several.contains("names several")
                && several.contains("so.tilo.video")
                && several.contains("so.tilo.video-editor"),
            "{several}"
        );
        // A whole name is its app, whatever else begins with it.
        let apps = [
            ("so.tilo.notes", "Notes"),
            ("so.tilo.notes-archive", "Notes Archive"),
        ];
        assert_eq!(
            app_meant("Notes", &apps),
            Ok(Some("so.tilo.notes".to_owned()))
        );
        assert!(app_meant("not", &apps).is_err());
    }

    #[test]
    fn a_name_no_app_has_names_none() {
        for spelled in [
            "",
            "  ",
            "-",
            "wo",
            "window",
            "processor",
            "tilo",
            "so.tilo",
            "so.tilo.word",
            "word.processor",
            "tasks.tracker",
        ] {
            assert_eq!(app_meant(spelled, APPS), Ok(None), "{spelled}");
        }
    }

    #[test]
    fn an_action_no_resource_offers_names_at_most_five_close_matches() {
        // An action nobody has is answered with what may have been meant, at most
        // [`CLOSE_MATCHES`] of them, never the app's whole list.
        let ids: Vec<String> = [
            "mark-read",
            "read-aloud",
            "read",
            "archive",
            "reply",
            "remove",
            "rename",
            "restore",
            "reload",
            "read-more",
            "read-later",
            "react",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let close = close_matches("read", &ids, CLOSE_MATCHES);
        assert_eq!(close.len(), CLOSE_MATCHES, "{close:?}");
        assert!(close.iter().all(|id| id.contains("read")), "{close:?}");
        assert_eq!(close_matches("arch", &ids, CLOSE_MATCHES), ["archive"]);
        assert!(close_matches("explode", &ids, CLOSE_MATCHES).is_empty());
    }

    #[test]
    fn bound_ids_decode_to_the_apps_own_name_and_nothing_else_does() {
        let key = "notes.notebook/page/Todo/body";
        let bound = format!(
            "tilo-ui-bound:v1:a:c0fe4ad7-b7d0-46c3-ab7e-78d12d5d86f4:{}",
            hex::encode(key)
        );
        assert_eq!(bound_key(&bound).as_deref(), Some(key));
        assert_eq!(bound_key("notes.notebook"), None);
        assert_eq!(bound_key("tilo-ui-bound:v1:a:b:zz"), None);
        assert_eq!(bound_key("tilo-ui-bound:v1:a:b:abc"), None);
    }
}
