//! A case: one situation an AI feature is asked to handle, the answer it is
//! expected to give, and the checks a run of it is scored against.
//!
//! A case is data. It names the interaction it exercises, the instructions and
//! input the model is sent, and an optional expected answer. It never holds a
//! model, an API key or a provider: which model a case is asked of is the
//! run's business, so the same suite compares a local model, a hosted one and
//! yesterday's prompt.
//!
//! ## The checks are typed, and they are all that score
//!
//! Nothing scores an answer by reading it. [`Check`] covers the shape a
//! caller asked for, the expected value, the classification a label set
//! asked for and the property an output must have; a case states those as
//! data, so a regression is a diff in a report rather than a judge's opinion.
//! A judge may arrive later, for what a schema cannot express; it can only
//! ever add a named check of its own, never replace the typed ones.
//!
//! ## Cases come from real runs
//!
//! `Case::from_run` turns a run a person marked good or bad into a case,
//! with the model's answer kept when they marked it bad, so the regression it
//! caught is reproduced from then on. It is built from what the run already
//! recorded and stored where the caller says — nothing leaves the machine.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::schema;

/// The interaction kinds Tilo asks a model to carry out, one suite each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InteractionKind {
    /// One-shot text work: a rewrite, a translation, a summary.
    OneShot,
    /// A person's request decided into an answer or a named piece of work.
    Routing,
    /// A task carried out through the actions its grant allows.
    Task,
    /// A skill's instructions against a situation.
    Skill,
    /// The `Ai` and `Task` steps of a shortcut.
    ShortcutStep,
}

impl InteractionKind {
    /// Every kind, in the order a report lists them.
    pub const ALL: &'static [Self] = &[
        Self::OneShot,
        Self::Routing,
        Self::Task,
        Self::Skill,
        Self::ShortcutStep,
    ];

    /// The kind's name as a case file spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OneShot => "one-shot",
            Self::Routing => "routing",
            Self::Task => "task",
            Self::Skill => "skill",
            Self::ShortcutStep => "shortcut-step",
        }
    }
}

impl std::fmt::Display for InteractionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One case of a suite.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    /// Stable within its suite; it names the case in a report and in the
    /// `--case` of a rerun, so renaming one loses the history it had.
    pub id: String,
    /// Which interaction this exercises.
    pub kind: InteractionKind,
    /// What the feature tells the model to do, as the shipped prompt or a
    /// skill's instructions would.
    pub instructions: String,
    /// The input the model is asked about.
    #[serde(default)]
    pub input: String,
    /// The answer a correct run gives, when there is one. A case without it
    /// is scored on its checks alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<Value>,
    /// What the answer must satisfy, over and above `expected`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
    /// Where the case came from, when it was captured from a run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CaseSource>,
    /// One line saying why this case earns its place: the regression it
    /// caught, the ambiguity it decides.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
}

/// The run a captured case was taken from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseSource {
    /// What the person said about the run.
    pub verdict: Verdict,
    /// The answer the run gave, kept whenever the person marked it bad.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

/// Whether a run was good or bad.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    /// The answer was right: the run is a case the feature must keep passing.
    #[default]
    Good,
    /// The answer was wrong: the run is a case the feature must stop failing.
    Bad,
}

impl Verdict {
    /// Whether this verdict names the run as wrong.
    #[must_use]
    pub const fn is_bad(self) -> bool {
        matches!(self, Self::Bad)
    }
}

/// One thing an answer is checked against.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum Check {
    /// The answer is, or is a value of, this JSON Schema — the shape the
    /// caller asked for.
    Shape {
        /// The schema the answer's value must satisfy.
        schema: Value,
    },
    /// The answer says the same as this value.
    Equal {
        /// The value the answer must be.
        to: Value,
    },
    /// The answer is the same text as this, up to surrounding whitespace.
    Text {
        /// The text the answer must be.
        is: String,
    },
    /// The answer says what it decided *and* why: a routing or classification
    /// case whose wrong answer a reader can learn from. It is one typed check,
    /// not a reading in place of one: it passes on giving both parts.
    Reasoned {
        /// What the deciding value must be, matched case-insensitively.
        decided: String,
        /// The words the reason must use, at least one of — the vocabulary the
        /// feature is supposed to reason with.
        words: Vec<String>,
    },
    /// The answer does not contain this text, in any letter case. The check a
    /// hostile input is built to trip.
    NotContaining {
        /// The text the answer must not say.
        text: String,
    },
    /// The answer contains this text, in any letter case.
    Containing {
        /// The text the answer must say.
        text: String,
    },
    /// The answer is no longer than this many characters.
    AtMostChars {
        /// The most characters the answer may hold.
        chars: usize,
    },
    /// No three words of the input, in order, appear in the answer: an
    /// answer that repeats its input has not transformed it.
    NotEchoingInput {
        /// How many words of the input must not appear in the answer, in
        /// order. Three catches a copied sentence without failing an answer
        /// that reuses a phrase.
        words: usize,
    },
}

impl Check {
    /// The check kind, as a report names it.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Shape { .. } => "shape",
            Self::Equal { .. } => "equal",
            Self::Text { .. } => "text",
            Self::Reasoned { .. } => "reasoned",
            Self::NotContaining { .. } => "not-containing",
            Self::Containing { .. } => "containing",
            Self::AtMostChars { .. } => "at-most-chars",
            Self::NotEchoingInput { .. } => "not-echoing-input",
        }
    }
}

/// Why a case or a suite cannot be loaded.
#[derive(Debug)]
pub struct CaseError {
    /// The case, when the refusal is about one.
    pub case: Option<String>,
    /// What is wrong, in the words the case author reads.
    pub reason: String,
}

impl std::fmt::Display for CaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.case {
            Some(id) => write!(f, "case `{id}`: {}", self.reason),
            None => f.write_str(&self.reason),
        }
    }
}

impl std::error::Error for CaseError {}

impl CaseError {
    fn suite(reason: impl Into<String>) -> Self {
        Self {
            case: None,
            reason: reason.into(),
        }
    }
}

/// A suite: every case for one interaction kind, in one file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    /// Which interaction the suite's cases exercise.
    pub kind: InteractionKind,
    /// What the suite covers, and which prompt or skill it holds to.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The cases, in the order a run asks them.
    pub cases: Vec<Case>,
}

impl Suite {
    /// The suite in `path`, checked against the format and the case rules a
    /// suite holds itself to.
    ///
    /// # Errors
    ///
    /// When the file is not a suite, and for the first case that is
    /// malformed, repeats an id, is of another kind than the suite, declares
    /// a schema Tilo would refuse, or carries an answer it could not be
    /// compared with.
    pub fn load(path: &Path) -> Result<Self, CaseError> {
        let bytes = std::fs::read(path).map_err(|error| {
            CaseError::suite(format!("{} cannot be read: {error}", path.display()))
        })?;
        let suite: Self = serde_json::from_slice(&bytes).map_err(|error| {
            CaseError::suite(format!("{} is not a case suite: {error}", path.display()))
        })?;
        suite.validate()?;
        Ok(suite)
    }

    /// The suite as the case file spells it: pretty JSON with a trailing
    /// newline, so a captured case is a readable diff.
    ///
    /// # Errors
    ///
    /// When a case holds something JSON cannot carry.
    pub fn to_json(&self) -> Result<String, CaseError> {
        let mut text = serde_json::to_string_pretty(self)
            .map_err(|error| CaseError::suite(format!("the suite cannot be written: {error}")))?;
        text.push('\n');
        Ok(text)
    }

    /// Check the suite against its own rules.
    ///
    /// # Errors
    ///
    /// As [`Suite::load`], over a suite held in memory.
    pub fn validate(&self) -> Result<(), CaseError> {
        let at = |case: &Case, reason: String| CaseError {
            case: Some(case.id.clone()),
            reason,
        };
        let mut seen = std::collections::BTreeSet::new();
        for case in &self.cases {
            if case.id.trim().is_empty() {
                return Err(at(case, "a case needs an id".into()));
            }
            if !seen.insert(case.id.as_str()) {
                return Err(at(case, "two cases share this id".into()));
            }
            if case.kind != self.kind {
                return Err(at(
                    case,
                    format!("is {}, in a {} suite", case.kind, self.kind),
                ));
            }
            if case.instructions.trim().is_empty() {
                return Err(at(case, "a case needs instructions".into()));
            }
            for check in &case.checks {
                if let Check::Shape { schema: shape } = check {
                    schema::check_declaration(shape).map_err(|reason| {
                        at(case, format!("its shape cannot be checked: {reason}"))
                    })?;
                }
            }
        }
        Ok(())
    }

    /// Add `case`, replacing the case of the same id, and keep the cases in
    /// id order so a captured case lands where its neighbours are.
    ///
    /// # Errors
    ///
    /// When the case does not belong in this suite, or the suite would then
    /// be malformed.
    pub fn insert(&mut self, case: Case) -> Result<(), CaseError> {
        self.cases.retain(|held| held.id != case.id);
        self.cases.push(case);
        self.cases.sort_by(|left, right| left.id.cmp(&right.id));
        self.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn one_shot(id: &str) -> Case {
        Case {
            id: id.into(),
            kind: InteractionKind::OneShot,
            instructions: "Say hello".into(),
            input: "hi".into(),
            expected: Some(json!("hello")),
            checks: vec![Check::Text { is: "hello".into() }],
            source: Some(CaseSource {
                verdict: Verdict::Bad,
                answer: Some("hey".into()),
            }),
            why: "a greeting is not a rewrite".into(),
        }
    }

    #[test]
    fn a_suite_written_out_and_read_again_is_the_same_suite() {
        let suite = Suite {
            kind: InteractionKind::OneShot,
            description: "rewrites and translations".into(),
            cases: vec![one_shot("greet")],
        };
        let text = suite.to_json().unwrap();
        assert!(text.ends_with("}\n"), "{text}");
        assert!(text.contains("\"verdict\": \"bad\""), "{text}");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one-shot.json");
        std::fs::write(&path, &text).unwrap();
        assert_eq!(Suite::load(&path).ok().as_ref(), Some(&suite));
    }

    #[test]
    fn a_suite_whose_cases_disagree_with_it_is_refused_and_names_the_case() {
        let mut misplaced = one_shot("greet");
        misplaced.kind = InteractionKind::Task;
        let suite = Suite {
            kind: InteractionKind::OneShot,
            description: String::new(),
            cases: vec![misplaced],
        };
        let refusal = suite.validate().err().unwrap();
        assert_eq!(refusal.case.as_deref(), Some("greet"));
        assert!(
            refusal.to_string().starts_with("case `greet`:"),
            "{refusal}"
        );
        assert!(refusal.reason.contains("one-shot"), "{refusal}");

        let twice = Suite {
            kind: InteractionKind::OneShot,
            description: String::new(),
            cases: vec![one_shot("greet"), one_shot("greet")],
        };
        assert!(twice.validate().unwrap_err().reason.contains("share"));
    }

    #[test]
    fn a_shape_a_suite_declares_is_checked_the_way_a_parameter_is() {
        let mut case = one_shot("greet");
        case.checks = vec![Check::Shape {
            schema: json!({"type": "string", "unsupported": true}),
        }];
        let suite = Suite {
            kind: InteractionKind::OneShot,
            description: String::new(),
            cases: vec![case],
        };
        let refusal = suite.validate().unwrap_err();
        assert!(refusal.reason.contains("unsupported"), "{refusal}");
    }

    #[test]
    fn a_case_without_an_id_or_instructions_is_refused() {
        let suite = Suite {
            kind: InteractionKind::OneShot,
            description: String::new(),
            cases: vec![Case {
                id: "  ".into(),
                kind: InteractionKind::OneShot,
                instructions: "Say hello".into(),
                input: String::new(),
                expected: None,
                checks: Vec::new(),
                source: None,
                why: String::new(),
            }],
        };
        assert!(suite.validate().unwrap_err().reason.contains("needs an id"));
    }

    #[test]
    fn inserting_a_captured_case_replaces_the_one_it_came_from_in_id_order() {
        let mut suite = Suite {
            kind: InteractionKind::OneShot,
            description: String::new(),
            cases: vec![one_shot("a"), one_shot("c")],
        };
        let mut fixed = one_shot("c");
        fixed.why = "the second run was right".into();
        suite.insert(fixed).unwrap();
        suite.insert(one_shot("b")).unwrap();
        let ids: Vec<&str> = suite.cases.iter().map(|case| case.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        let kept = suite.cases.iter().find(|case| case.id == "c").unwrap();
        assert_eq!(kept.why, "the second run was right");
    }
}
