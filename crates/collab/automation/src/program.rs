//! A Shortcut as a file. A bot directory entry signs the BLAKE3 of these
//! bytes and a room stores the same bytes, so there is exactly one spelling
//! of a program: JSON with object keys sorted by their UTF-8 bytes at every
//! depth, no insignificant whitespace, and serde's own number and string
//! formatting. Anything else is refused, never re-encoded, because a
//! re-encoding would no longer match the hash someone reviewed.

use serde_json::Value;

use crate::check::{CheckError, CheckErrorKind, NoShortcuts, check};
use crate::{ActionCatalog, Shortcut};

/// Most bytes a program file may have; the directory publishes the same bound.
pub const MAX_PROGRAM_BYTES: usize = 262_144;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProgramError {
    #[error("a program is at most 262144 bytes, this one is {0}")]
    TooLarge(usize),
    #[error("the program is not a Shortcut: {0}")]
    Malformed(String),
    #[error("the program is not in canonical form")]
    NotCanonical,
    #[error("a published program may not name who saved it")]
    SavedBy,
}

/// The canonical bytes of `shortcut`.
///
/// # Errors
/// [`ProgramError::Malformed`] if the shortcut does not serialize, which only
/// a hand-built value with non-string map keys could cause.
pub fn canonical_bytes(shortcut: &Shortcut) -> Result<Vec<u8>, ProgramError> {
    let value =
        serde_json::to_value(shortcut).map_err(|e| ProgramError::Malformed(e.to_string()))?;
    Ok(canonical_json(&value))
}

/// `value` with every object's keys sorted, written without whitespace.
pub(crate) fn canonical_json(value: &Value) -> Vec<u8> {
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut entries: Vec<(&String, &Value)> = map.iter().collect();
                entries.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
                // With serde_json's preserve_order a map keeps insertion
                // order, so inserting in sorted order is what makes the
                // output sorted whichever map the build links.
                Value::Object(
                    entries
                        .into_iter()
                        .map(|(key, value)| (key.clone(), sorted(value)))
                        .collect(),
                )
            }
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    // Writing a Value to a Vec cannot fail.
    serde_json::to_vec(&sorted(value)).unwrap_or_default()
}

/// Parse a program file, accepting only its canonical spelling.
///
/// # Errors
/// [`ProgramError`] for an oversized, malformed, non-canonical or attributed file.
pub fn parse_program(bytes: &[u8]) -> Result<Shortcut, ProgramError> {
    if bytes.len() > MAX_PROGRAM_BYTES {
        return Err(ProgramError::TooLarge(bytes.len()));
    }
    let shortcut: Shortcut =
        serde_json::from_slice(bytes).map_err(|e| ProgramError::Malformed(e.to_string()))?;
    if shortcut.saved_by.is_some() {
        return Err(ProgramError::SavedBy);
    }
    if canonical_bytes(&shortcut)? != bytes {
        return Err(ProgramError::NotCanonical);
    }
    Ok(shortcut)
}

/// Lowercase BLAKE3 hex of a program file: its identity in a directory entry
/// and in a room's content store.
#[must_use]
pub fn program_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// The checks that need no action descriptors: names, scoping, templates and
/// recursion. Typing against descriptors happens at install, against the
/// installing room's catalogue.
///
/// # Errors
/// Every structural problem found.
pub fn check_structure(shortcut: &Shortcut) -> Result<(), Vec<CheckError>> {
    let errors: Vec<CheckError> =
        match check(shortcut, &ActionCatalog::new(Vec::new()), &NoShortcuts) {
            Ok(()) => return Ok(()),
            Err(errors) => errors
                .into_iter()
                .filter(|error| !matches!(error.kind, CheckErrorKind::UnknownAction { .. }))
                .collect(),
        };
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Binding, Selector, Shortcut, Step, StepKind};
    use std::collections::BTreeMap;
    use tilo_actions::ActionKey;

    fn echo() -> Shortcut {
        Shortcut {
            id: "echo".into(),
            name: "Echo".into(),
            inputs: Vec::new(),
            steps: vec![Step::new(
                "post",
                StepKind::Invoke {
                    action: ActionKey::new("so.tilo.chat", "chat.workspace", "post"),
                    resource: Selector::FirstOfKind("chat.workspace".into()),
                    args: BTreeMap::from([(
                        "text".into(),
                        Binding::Literal(serde_json::json!({"b": 1, "a": [2, {"d": 3, "c": 4}]})),
                    )]),
                    appearance: None,
                },
            )],
            output: None,
            grant: Vec::new(),
            triggers: Vec::new(),
            saved_by: None,
        }
    }

    #[test]
    fn canonical_bytes_sort_keys_at_every_depth_and_round_trip() {
        let bytes = canonical_bytes(&echo()).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains(r#"{"a":[2,{"c":4,"d":3}],"b":1}"#), "{text}");
        assert!(
            !text.contains(": ") && !text.contains(", ") && !text.contains('\n'),
            "no insignificant whitespace"
        );
        assert_eq!(parse_program(&bytes).unwrap(), echo());
        assert_eq!(
            program_hash(&bytes),
            blake3::hash(&bytes).to_hex().to_string()
        );
    }

    #[test]
    fn parse_program_refuses_every_non_canonical_spelling() {
        let canonical = canonical_bytes(&echo()).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&canonical).unwrap();
        let pretty = serde_json::to_vec_pretty(&value).unwrap();
        assert_eq!(parse_program(&pretty), Err(ProgramError::NotCanonical));
        let mut with_default = value.clone();
        with_default["inputs"] = serde_json::json!([]);
        let with_default = canonical_json(&with_default);
        assert_eq!(
            parse_program(&with_default),
            Err(ProgramError::NotCanonical)
        );
        let mut unknown = value;
        unknown["zzz"] = serde_json::json!(1);
        assert_eq!(
            parse_program(&canonical_json(&unknown)),
            Err(ProgramError::NotCanonical)
        );
        let mut saved = echo();
        saved.saved_by = Some("ada".into());
        assert_eq!(
            parse_program(&canonical_bytes(&saved).unwrap()),
            Err(ProgramError::SavedBy)
        );
        assert!(matches!(
            parse_program(b"{"),
            Err(ProgramError::Malformed(_))
        ));
        let big = vec![b' '; MAX_PROGRAM_BYTES + 1];
        assert_eq!(
            parse_program(&big),
            Err(ProgramError::TooLarge(MAX_PROGRAM_BYTES + 1))
        );
    }

    #[test]
    fn structure_is_checked_without_descriptors() {
        assert_eq!(check_structure(&echo()), Ok(()));
        let mut broken = echo();
        broken.steps.push(broken.steps[0].clone()); // duplicate step id
        let errors = check_structure(&broken).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e.kind, CheckErrorKind::DuplicateStep(_)))
        );
        assert!(
            errors
                .iter()
                .all(|e| !matches!(e.kind, CheckErrorKind::UnknownAction { .. }))
        );
    }
}
