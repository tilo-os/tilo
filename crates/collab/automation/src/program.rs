//! A Shortcut as a file. A bot directory entry signs the BLAKE3 of these
//! bytes and a workspace stores the same bytes, so there is exactly one
//! spelling of a program: JSON with object keys sorted by their UTF-8 bytes
//! at every depth, no insignificant whitespace, and serde's own number and
//! string formatting. Anything else is refused, never re-encoded, because a
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
    tilo_canonical_json::to_vec(value)
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
    if shortcut.saved_by.is_some() || shortcut.saved_proof.is_some() {
        return Err(ProgramError::SavedBy);
    }
    if canonical_bytes(&shortcut)? != bytes {
        return Err(ProgramError::NotCanonical);
    }
    Ok(shortcut)
}

/// Lowercase BLAKE3 hex of a program file: its identity in a directory entry
/// and in a workspace's content store.
#[must_use]
pub fn program_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// The checks that need no action descriptors: names, scoping, templates and
/// recursion. Typing against descriptors happens at install, against the
/// installing workspace's catalogue.
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
            step_timeout_secs: None,
            output: None,
            grant: Vec::new(),
            triggers: Vec::new(),
            saved_by: None,
            saved_proof: None,
        }
    }

    tilo_test_env::scenario! {
        fn a_stored_program_is_canonical_and_structurally_checked;

        phase canonical_bytes_sort_keys_at_every_depth_and_round_trip;
        {
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

        phase parse_program_refuses_every_non_canonical_spelling;
        {
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

        phase structure_is_checked_without_descriptors;
        {
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

    const CANONICAL_JSON_CORPUS: &[(&str, &str, &[u8])] = &[
        (
            "nested_unsorted",
            r#"{"z":{"b":2,"a":1},"a":{"d":{"y":true,"x":false},"c":[3,2,1]},"m":0}"#,
            br#"{"a":{"c":[3,2,1],"d":{"x":false,"y":true}},"m":0,"z":{"a":1,"b":2}}"#,
        ),
        (
            "utf8_vs_utf16_key_order",
            r#"{"\uD83D\uDE00":4,"\uE000":3,"é":2,"z":1}"#,
            b"{\"z\":1,\"\xc3\xa9\":2,\"\xee\x80\x80\":3,\"\xf0\x9f\x98\x80\":4}",
        ),
        (
            "non_bmp_vs_bmp_high_key_order",
            r#"{"\uD83D\uDE80":1,"\uFFFF":2,"\uE000":3}"#,
            b"{\"\xee\x80\x80\":3,\"\xef\xbf\xbf\":2,\"\xf0\x9f\x9a\x80\":1}",
        ),
        (
            "ascii_empty_prefix_key_order",
            r#"{"b":1,"":2,"a":3,"ab":4,"B":5,"A":6}"#,
            br#"{"":2,"A":6,"B":5,"a":3,"ab":4,"b":1}"#,
        ),
        (
            "nested_ten_keys",
            r#"{"k9":9,"k8":8,"k7":{"j9":9,"j8":8,"j7":7,"j6":6,"j5":5,"j4":4,"j3":3,"j2":2,"j1":1,"j0":0},"k6":6,"k5":5,"k4":4,"k3":3,"k2":2,"k1":1,"k0":0}"#,
            br#"{"k0":0,"k1":1,"k2":2,"k3":3,"k4":4,"k5":5,"k6":6,"k7":{"j0":0,"j1":1,"j2":2,"j3":3,"j4":4,"j5":5,"j6":6,"j7":7,"j8":8,"j9":9},"k8":8,"k9":9}"#,
        ),
        ("empty_object", "{}", b"{}"),
        ("empty_array", "[]", b"[]"),
        ("empty_string", r#""""#, br#""""#),
        (
            "line_seps_fffd_and_solidus",
            r#""\u2028\u2029\ufffd/""#,
            b"\"\xe2\x80\xa8\xe2\x80\xa9\xef\xbf\xbd/\"",
        ),
        (
            "all_32_control_bytes_value",
            r#""\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f""#,
            br#""\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\b\t\n\u000b\f\r\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f""#,
        ),
        (
            "all_32_control_bytes_key",
            r#"{"a":2,"\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f":1}"#,
            br#"{"\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\b\t\n\u000b\f\r\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f":1,"a":2}"#,
        ),
        (
            "escaped_and_unicode_string",
            r#""q\"b\\c\u0000\u0001\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u000e\u001f\u007f café 日本語 🚀""#,
            b"\"q\\\"b\\\\c\\u0000\\u0001\\u0007\\b\\t\\n\\u000b\\f\\r\\u000e\\u001f\x7f caf\xc3\xa9 \xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e \xf0\x9f\x9a\x80\"",
        ),
        (
            "escaped_keys_object",
            r#"{"q\"b\\c\u0000\b\t\n\f\r\u001f\u007f":2,"a":1}"#,
            b"{\"a\":1,\"q\\\"b\\\\c\\u0000\\b\\t\\n\\f\\r\\u001f\x7f\":2}",
        ),
        ("i64_min", "-9223372036854775808", b"-9223372036854775808"),
        ("i64_max", "9223372036854775807", b"9223372036854775807"),
        (
            "u64_above_i64_max",
            "9223372036854775808",
            b"9223372036854775808",
        ),
        ("u64_max", "18446744073709551615", b"18446744073709551615"),
        ("zero", "0", b"0"),
        ("negative_zero", "-0.0", b"-0.0"),
        ("float_whole", "1.0", b"1.0"),
        ("float_tenth", "0.1", b"0.1"),
        ("float_1e20", "1e20", b"1e+20"),
        ("float_1e21", "1e21", b"1e+21"),
        ("float_1e_neg5", "1e-5", b"0.00001"),
        ("float_1e_neg7", "1e-7", b"1e-7"),
        ("float_min_subnormal", "5e-324", b"5e-324"),
        (
            "float_max_finite",
            "1.7976931348623157e308",
            b"1.7976931348623157e+308",
        ),
        ("float_1e15", "1e15", b"1000000000000000.0"),
        ("float_0_3_ulp", "0.30000000000000004", b"0.30000000000000004"),
        ("bool_true", "true", b"true"),
        ("bool_false", "false", b"false"),
        ("null", "null", b"null"),
        (
            "mixed_array",
            r#"[null,true,false,0,-1,1.0,0.1,-0.0,"a\"b",[],{},{"b":2,"a":1}]"#,
            br#"[null,true,false,0,-1,1.0,0.1,-0.0,"a\"b",[],{},{"a":1,"b":2}]"#,
        ),
    ];

    #[test]
    fn canonical_json_pins_bytes_and_program_hash_for_shared_corpus() {
        for &(name, input, expected) in CANONICAL_JSON_CORPUS {
            let value: Value = serde_json::from_str(input).unwrap();
            assert_eq!(canonical_json(&value), expected, "row {name}");
        }
        let nested: Value = serde_json::from_str(CANONICAL_JSON_CORPUS[0].1).unwrap();
        assert_eq!(
            program_hash(&canonical_json(&nested)),
            "948c6e61fa0aef3c6bf726c59aaea925e2728f6066dbf3262c3586702453ef94"
        );
    }
}
