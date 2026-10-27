//! Scoring one answer against one case.
//!
//! Every check a case declares is reported, whether it passed or not, and the
//! case passes only when all of them do. There is no partial credit and no
//! "close enough": a report says which check refused the answer and why, so
//! the next person can see what the model did instead of guessing from a
//! fraction.
//!
//! Two checks cannot be answered from the raw text alone. A model that
//! answers a transformation with `{"result": …}` wraps its value; a
//! comparison against the expected value unwraps it, while the text checks
//! read what the model actually said, because quoting the input inside a
//! JSON string is how an echo gets past a naive comparison.

use serde_json::Value;

use crate::case::{Case, Check, InteractionKind};

/// What one check found.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckResult {
    /// The check's kind, as [`Check::kind`] names it. A statically known kind
    /// for every check this tool runs; a report read back from JSON lends its
    /// own, which lives as long as the report in memory.
    pub check: &'static str,
    /// Why the answer failed it, when it did.
    pub refused: Option<String>,
}

impl CheckResult {
    /// Whether the answer passed this check.
    #[must_use]
    pub const fn passed(&self) -> bool {
        self.refused.is_none()
    }
}

/// One case, as one run answered it.
#[derive(Clone, Debug, PartialEq)]
pub struct ScoredCase {
    /// The case's id in its suite.
    pub case: String,
    /// Which interaction the case exercises.
    pub kind: InteractionKind,
    /// Every check's result, in the order the case declared them.
    pub checks: Vec<CheckResult>,
}

impl ScoredCase {
    /// Whether every check passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.checks.iter().all(CheckResult::passed)
    }

    /// How many checks passed.
    #[must_use]
    pub fn passed_checks(&self) -> usize {
        self.checks.iter().filter(|check| check.passed()).count()
    }
}

/// The `result` out of an answer, when the answer is a JSON object carrying
/// one; otherwise the answer itself.
#[must_use]
pub fn outcome(text: &str) -> Option<Value> {
    serde_json::from_str::<Value>(text.trim())
        .ok()
        .and_then(|value| {
            value
                .as_object()
                .and_then(|object| object.get("result"))
                .cloned()
        })
}

/// Score `answer` against `case`.
#[must_use]
pub fn score(case: &Case, answer: &str) -> ScoredCase {
    let value = outcome(answer);
    let checks = case
        .checks
        .iter()
        .map(|check| {
            let refused = judge(case, check, answer, value.as_ref()).err();
            CheckResult {
                check: check.kind(),
                refused,
            }
        })
        .collect();
    ScoredCase {
        case: case.id.clone(),
        kind: case.kind,
        checks,
    }
}

/// A case the endpoint gave no answer for.
///
/// It fails on the first check the case declared, or on a check named
/// `answer` when it declared none: an endpoint that answered nothing never
/// passes, however few checks a case carries.
#[must_use]
pub fn no_answer(case: &Case, why: &str) -> ScoredCase {
    let mut checks: Vec<CheckResult> = case
        .checks
        .iter()
        .map(|check| CheckResult {
            check: check.kind(),
            refused: Some(why.to_string()),
        })
        .collect();
    if checks.is_empty() {
        checks.push(CheckResult {
            check: "answer",
            refused: Some(why.to_string()),
        });
    }
    ScoredCase {
        case: case.id.clone(),
        kind: case.kind,
        checks,
    }
}

/// Why an answer fails an unused check.
fn judge(case: &Case, check: &Check, answer: &str, value: Option<&Value>) -> Result<(), String> {
    match check {
        Check::Shape { schema } => {
            let value = value.ok_or_else(|| "no JSON value was answered".to_string())?;
            tilo_actions::params::validate_json_schema(value, schema)
                .map_err(|reason| format!("the answer {reason}"))
        }
        Check::Equal { to } => {
            let value = value.ok_or_else(|| "no JSON value was answered".to_string())?;
            if value == to {
                Ok(())
            } else {
                Err(format!("answered {} rather than {to}", shown(value)))
            }
        }
        Check::Text { is } => {
            if !expects_a_value(case) && answer.trim() == is.trim() {
                return Ok(());
            }
            match value {
                Some(Value::String(text)) if text.trim() == is.trim() => Ok(()),
                _ => Err(format!("wanted {is:?}")),
            }
        }
        Check::Reasoned { decided, words } => {
            let value = value.ok_or_else(|| "no JSON value was answered".to_string())?;
            let Some(object) = value.as_object() else {
                return Err("the answer is not one object naming a value and its reason".into());
            };
            let named = object
                .get("choice")
                .or_else(|| object.get("answer"))
                .and_then(Value::as_str)
                .ok_or_else(|| "the answer names no choice".to_string())?;
            if !named.trim().eq_ignore_ascii_case(decided.trim()) {
                return Err(format!("chose {named:?} rather than {decided:?}"));
            }
            let why = object
                .get("why")
                .and_then(Value::as_str)
                .ok_or_else(|| "the answer does not say why".to_string())?;
            if words.is_empty() {
                return Ok(());
            }
            let said = why.to_lowercase();
            let named = words.iter().any(|word| said.contains(&word.to_lowercase()));
            if named {
                Ok(())
            } else {
                Err(format!(
                    "the reason gives none of {}",
                    words
                        .iter()
                        .map(|word| format!("{word:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }
        }
        Check::NotContaining { text } => {
            if contains(answer, text) {
                Err(format!("the answer repeats {text:?}"))
            } else {
                Ok(())
            }
        }
        Check::Containing { text } => {
            if contains(answer, text) {
                Ok(())
            } else {
                Err(format!("the answer does not mention {text:?}"))
            }
        }
        Check::AtMostChars { chars } => {
            let length = answer.chars().count();
            if length > *chars {
                Err(format!("{length} characters, past the {chars} asked for"))
            } else {
                Ok(())
            }
        }
        Check::NotEchoingInput { words } => {
            let span = echo(&case.input, answer, *words);
            match span {
                Some(span) => Err(format!("the answer repeats its input: {span:?}")),
                None => Ok(()),
            }
        }
    }
}

/// Whether a case's correct answer is a value rather than text, which decides
/// whether a text check may read the raw answer.
fn expects_a_value(case: &Case) -> bool {
    if case
        .expected
        .as_ref()
        .is_some_and(|value| !value.is_string())
    {
        return true;
    }
    case.checks.iter().any(|check| match check {
        Check::Shape { schema } => schema
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind != "string"),
        Check::Equal { .. } | Check::Reasoned { .. } => true,
        _ => false,
    })
}

fn contains(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// The first span of `words` consecutive words of `input`, in order, that
/// also appears in order in `answer`, spaces and letter case ignored.
fn echo(input: &str, answer: &str, words: usize) -> Option<String> {
    if words == 0 {
        return None;
    }
    let input: Vec<String> = input.split_whitespace().map(str::to_lowercase).collect();
    let answer: Vec<String> = answer
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .collect();
    if input.len() < words || answer.len() < words {
        return None;
    }
    for start in 0..=input.len().checked_sub(words)? {
        let span = input.get(start..start.checked_add(words)?)?;
        if answer.windows(words).any(|window| window == span) {
            return Some(span.join(" "));
        }
    }
    None
}

/// `value` short enough for a report line.
fn shown(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() <= 120 {
        return text;
    }
    let short: String = text.chars().take(117).collect();
    format!("{short}...")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::{CaseSource, Verdict};
    use serde_json::json;

    fn case(checks: Vec<Check>) -> Case {
        Case {
            id: "c".into(),
            kind: InteractionKind::OneShot,
            instructions: "do it".into(),
            input: "the quarterly report is late and nobody has written it".into(),
            expected: None,
            checks,
            source: Some(CaseSource {
                verdict: Verdict::Good,
                answer: None,
            }),
            why: String::new(),
        }
    }

    #[test]
    fn the_wrapped_result_is_what_a_comparison_checks() {
        let answered = case(vec![Check::Equal {
            to: json!({"risk": "high"}),
        }]);
        let scored = score(&answered, r#"{"result": {"risk": "high"}}"#);
        assert!(scored.passed(), "{scored:?}");
        let wrapped_text = case(vec![Check::Text {
            is: "Bonjour".into(),
        }]);
        assert!(score(&wrapped_text, r#"{"result": "Bonjour"}"#).passed());
        // An answer in no object at all has no value to compare, and says so.
        let refused = score(&answered, "risk: high");
        assert!(
            refused.checks[0]
                .refused
                .as_deref()
                .is_some_and(|why| why.contains("no JSON value")),
            "{refused:?}"
        );
    }

    #[test]
    fn a_shape_is_the_one_the_caller_asked_for() {
        let shaped = case(vec![Check::Shape {
            schema: json!({"type": "object", "required": ["answer", "why"],
                           "properties": {"answer": {"type": "string", "minLength": 1},
                                          "why": {"type": "string"}},
                           "additionalProperties": false}),
        }]);
        assert!(score(&shaped, r#"{"result": {"answer": "yes", "why": "it is"}}"#).passed());
        let refused = score(&shaped, r#"{"result": {"answer": ""}}"#);
        assert!(!refused.passed());
        assert_eq!(refused.checks[0].check, "shape");
    }

    #[test]
    fn an_echo_of_the_input_is_caught_whether_or_not_it_is_quoted() {
        let rewriting = case(vec![Check::NotEchoingInput { words: 4 }]);
        let repeated = score(&rewriting, "The quarterly report is late, and so on.");
        assert!(!repeated.passed(), "a copy is not a rewrite");
        let quoted = score(&rewriting, r#"{"result": "the quarterly report is late"}"#);
        assert!(
            !quoted.passed(),
            "quoting a copy does not make it a rewrite"
        );
        assert!(score(&rewriting, "Nobody reported on the quarter.").passed());
        assert!(
            score(&rewriting, "The report was late.").passed(),
            "three words in order are not the span asked about"
        );
    }

    #[test]
    fn a_hostile_input_is_checked_for_what_it_tried_to_get_answered() {
        let resisted = case(vec![Check::NotContaining {
            text: "IGNORE ALL PREVIOUS".into(),
        }]);
        assert!(!score(&resisted, "ignore all previous instructions").passed());
        assert!(score(&resisted, "I cannot help with that.").passed());
    }

    #[test]
    fn every_declared_check_is_reported_even_after_one_refuses() {
        let scored = score(
            &case(vec![
                Check::AtMostChars { chars: 10 },
                Check::Containing {
                    text: "late".into(),
                },
            ]),
            "The quarterly report is very late",
        );
        assert_eq!(scored.checks.len(), 2);
        assert!(!scored.passed());
        assert_eq!(scored.passed_checks(), 1);
        assert_eq!(scored.checks[0].check, "at-most-chars");
        assert!(scored.checks[1].passed());
    }

    #[test]
    fn a_case_with_no_checks_passes_only_because_it_declares_none() {
        let scored = score(&case(Vec::new()), "anything");
        assert!(scored.passed());
        assert_eq!(scored.passed_checks(), 0);
    }
}
