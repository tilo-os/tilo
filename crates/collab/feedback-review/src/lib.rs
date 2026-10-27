//! Review of a feedback recording, before anything leaves the device.
//!
//! This crate restores the portable review contract from `feat/feedback`
//! (815371ae89); desktop capture, transport and the review UI are separate
//! integration work. This crate is the contract for that review:
//!
//! - [`ReviewTraceRequest`] is the bounded description of the recorded steps.
//!   It carries only text whose declared sensitivity is
//!   [`DeclaredSensitivity::Public`] or [`DeclaredSensitivity::Personal`];
//!   `Secret` has no representation here, so a secret value cannot be put
//!   into a review request by mistake.
//! - [`ReviewProposals`] is the answer: redaction proposals addressed by step,
//!   node and byte span, and short step titles for the walk-through.
//! - [`floor`] is the deterministic answer that needs no model: declared
//!   `Personal` values plus pattern detectors.
//! - [`merge`] lays a model's proposals over the floor. Review always runs the
//!   floor, so a missing or failing local model never weakens it.
//! - [`model_view`] is everything a model is ever shown. A model in Tilo's
//!   own supervised, egress-free runtime sees the whole trace; any other sees
//!   every declared `Personal` value, wherever it occurs, as a typed
//!   placeholder such as `‹name›`. The floor already redacts those values, so
//!   such a model does not need them.
//!
//! [`review`] validates the bounded request and every model proposal before use.
//! This contract performs no capture, network request or model invocation.

mod detect;
mod merge;
mod view;

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use detect::floor;
pub use merge::{label_title, merge};

#[cfg(test)]
mod locale_tests {
    use super::*;

    #[test]
    fn every_label_title_has_an_english_message_with_its_argument() {
        let titles = [
            LabelTitle::Opened { app: "a".into() },
            LabelTitle::Invoked { action: "a".into() },
            LabelTitle::Refused { action: "a".into() },
            LabelTitle::Failed { action: "a".into() },
            LabelTitle::Edited { field: "a".into() },
            LabelTitle::Appeared { label: "a".into() },
        ];
        for title in titles {
            let line = FLUENT_EN_US
                .lines()
                .find(|line| line.starts_with(&format!("{} =", title.fluent_key())))
                .unwrap_or_else(|| panic!("{} has no message", title.fluent_key()));
            for (name, _) in title.args() {
                assert!(line.contains(&format!("{{ ${name} }}")), "{line}");
            }
        }
    }
}
pub use view::{Exposure, ModelText, model_view, placeholder};

/// Steps in one review request. Longer recordings are reviewed as several
/// bounded requests, so one model call stays inside a small local context.
pub const MAX_STEPS: usize = 64;
pub const MAX_TEXTS_PER_STEP: usize = 48;
/// Unicode scalar values in one text.
pub const MAX_TEXT_CHARS: usize = 2_000;
/// Unicode scalar values across every text of one request.
pub const MAX_TOTAL_TEXT_CHARS: usize = 12_000;
/// Unicode scalar values in an app name, action label or resource kind.
pub const MAX_LABEL_CHARS: usize = 160;
pub const MAX_NODE_ID_BYTES: usize = 256;
/// Unicode scalar values in a step title.
pub const MAX_TITLE_CHARS: usize = 96;
/// Redaction proposals in one answer.
pub const MAX_REDACTIONS: usize = 512;
/// The glyph that replaces redacted text in a title, one per character, so a
/// title keeps the shape of what it hides.
pub const MASK: char = '\u{25AE}';

/// The declared sensitivity of a text in a review request.
///
/// This is the semantic layer's `Sensitivity` without `Secret`: a secret node
/// never reaches inspection or a trace, so it cannot reach review either.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclaredSensitivity {
    Public,
    Personal,
}

/// Where a text was read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextOrigin {
    /// A semantic node's label or value.
    Semantic,
    /// OCR over an attached frame region; the node id names the region.
    Ocr,
}

/// What a step did, as far as titling it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Opened,
    /// The person invoked the action named by the step's label.
    Invoked,
    /// The person changed a resource's content.
    Edited,
    /// An overlay (dialog, toast, banner, menu) named by the label appeared.
    Appeared,
}

/// How an attempted action ended, when it did not simply succeed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepOutcome {
    Done,
    /// Refused before it ran: permission denied, disabled, invalid input.
    Rejected,
    /// Ran and failed.
    Failed,
}

/// One text attached to a step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewText {
    /// The node's stable semantic id, or the frame region's id for OCR text.
    pub node: String,
    pub sensitivity: DeclaredSensitivity,
    /// What a `Personal` value is, when the widget declaring it knows (a
    /// contact field holds a name or an address). It names the placeholder a
    /// model sees instead. Always `None` for a `Public` text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<RedactionCategory>,
    pub origin: TextOrigin,
    pub text: String,
}

/// One recorded step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewStep {
    pub kind: StepKind,
    pub outcome: StepOutcome,
    /// The app's display name.
    pub app: String,
    /// The action's (or appeared overlay's) public UI label. Content the
    /// person wrote is never a label; it arrives as a [`ReviewText`].
    pub action: String,
    /// The addressed resource's kind, e.g. `mail.draft`.
    pub resource_kind: String,
    pub texts: Vec<ReviewText>,
}

/// The bounded description of recorded steps that review works on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewTraceRequest {
    pub steps: Vec<ReviewStep>,
}

/// A byte range in a text, on UTF-8 character boundaries, `start < end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.start <= other.start && other.end <= self.end
    }
}

/// What a proposed redaction hides.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionCategory {
    Name,
    Email,
    Phone,
    Address,
    /// Bank account (IBAN) or payment-card number.
    AccountNumber,
    /// Passwords, API keys, bearer tokens, signed URL parameters.
    CredentialLike,
    /// Anything else personal, including a whole declared `Personal` value.
    FreeFormPersonal,
}

/// Why a redaction was proposed, strongest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionBasis {
    /// The app or widget declared the value `Personal`.
    Declared,
    /// A deterministic pattern detector matched.
    Pattern,
    /// The device-local review model proposed it.
    Model,
}

/// Confidence in percent, `0..=100`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Confidence(u8);

impl Confidence {
    pub const CERTAIN: Self = Self(100);

    /// # Errors
    /// Returns [`InvalidConfidence`] above 100.
    pub const fn new(percent: u8) -> Result<Self, InvalidConfidence> {
        if percent <= 100 {
            Ok(Self(percent))
        } else {
            Err(InvalidConfidence(percent))
        }
    }

    #[must_use]
    pub const fn percent(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("confidence {0} is above 100")]
pub struct InvalidConfidence(pub u8);

impl TryFrom<u8> for Confidence {
    type Error = InvalidConfidence;
    fn try_from(percent: u8) -> Result<Self, Self::Error> {
        Self::new(percent)
    }
}

impl From<Confidence> for u8 {
    fn from(confidence: Confidence) -> Self {
        confidence.0
    }
}

/// One proposed redaction of `span` in the text of `node` at `step`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactionProposal {
    pub step: u32,
    pub node: String,
    pub span: Span,
    pub category: RedactionCategory,
    pub confidence: Confidence,
    pub basis: RedactionBasis,
}

/// A step title built from the step itself. The caller renders it through
/// its own i18n: [`Self::fluent_key`] with [`Self::args`], from the messages
/// in [`FLUENT_EN_US`] and its translations. The crate holds no display text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum LabelTitle {
    /// The person opened `app`.
    Opened { app: String },
    /// The person invoked `action` and it happened.
    Invoked { action: String },
    /// The person invoked `action` and it was refused.
    Refused { action: String },
    /// The person invoked `action` and it failed.
    Failed { action: String },
    /// The person changed the content of `field`.
    Edited { field: String },
    /// An overlay named `label` appeared.
    Appeared { label: String },
}

impl LabelTitle {
    /// The Fluent message id.
    #[must_use]
    pub const fn fluent_key(&self) -> &'static str {
        match self {
            Self::Opened { .. } => "feedback-review-step-opened",
            Self::Invoked { .. } => "feedback-review-step-invoked",
            Self::Refused { .. } => "feedback-review-step-refused",
            Self::Failed { .. } => "feedback-review-step-failed",
            Self::Edited { .. } => "feedback-review-step-edited",
            Self::Appeared { .. } => "feedback-review-step-appeared",
        }
    }

    /// The message's one named argument and its value.
    #[must_use]
    pub fn args(&self) -> [(&'static str, &str); 1] {
        match self {
            Self::Opened { app } => [("app", app)],
            Self::Invoked { action } | Self::Refused { action } | Self::Failed { action } => {
                [("action", action)]
            }
            Self::Edited { field } => [("field", field)],
            Self::Appeared { label } => [("label", label)],
        }
    }

    pub(crate) fn argument_mut(&mut self) -> &mut String {
        match self {
            Self::Opened { app } => app,
            Self::Invoked { action } | Self::Refused { action } | Self::Failed { action } => action,
            Self::Edited { field } => field,
            Self::Appeared { label } => label,
        }
    }
}

/// The English messages for every [`LabelTitle`]. Add them to the caller's
/// Fluent bundle (for `ui`, `i18n::add_resource("en-US", ..)`).
pub const FLUENT_EN_US: &str = include_str!("../locales/en-US.ftl");

/// What a step title says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TitleText {
    /// Built from the step; localized by the caller.
    Label(LabelTitle),
    /// Written by the review model, in the model's words and language. Show
    /// it as model text, not as a localized string.
    Model { text: String },
}

/// The walk-through title of one step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepTitle {
    pub step: u32,
    pub title: TitleText,
}

/// Review's answer for one request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewProposals {
    /// Sorted by step, node, span.
    pub redactions: Vec<RedactionProposal>,
    /// At most one per step, sorted by step.
    pub titles: Vec<StepTitle>,
}

/// Why a request or an answer does not fit the contract.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContractError {
    #[error("a review request holds 1 through {MAX_STEPS} steps")]
    StepCount,
    #[error("step {0} holds more than {MAX_TEXTS_PER_STEP} texts")]
    TextCount(usize),
    #[error("step {0} has an empty, oversized or control-character label")]
    Label(usize),
    #[error("step {0} has an empty, oversized or duplicate node id")]
    NodeId(usize),
    #[error("step {0} has an empty text or one above {MAX_TEXT_CHARS} characters")]
    Text(usize),
    #[error("step {0} declares a kind for a public text")]
    Kind(usize),
    #[error("the request's texts exceed {MAX_TOTAL_TEXT_CHARS} characters")]
    TotalText,
    #[error("a redaction does not address a text of the request")]
    UnknownText,
    #[error("a redaction span is empty, out of range or not on character boundaries")]
    BadSpan,
    #[error("a title addresses no step, repeats one, or is empty, oversized or multi-line")]
    BadTitle,
    #[error("more than {MAX_REDACTIONS} redactions")]
    TooManyRedactions,
}

fn chars_within(value: &str, limit: usize) -> bool {
    value.chars().nth(limit).is_none()
}

fn valid_label(value: &str) -> bool {
    !value.trim().is_empty()
        && chars_within(value, MAX_LABEL_CHARS)
        && !value.chars().any(char::is_control)
}

fn valid_node_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_NODE_ID_BYTES
        && !value.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// Whether `title` is one short line.
#[must_use]
pub fn valid_title(title: &str) -> bool {
    !title.trim().is_empty()
        && chars_within(title, MAX_TITLE_CHARS)
        && !title.chars().any(char::is_control)
}

impl ReviewTraceRequest {
    /// Check every bound of the contract.
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.steps.is_empty() || self.steps.len() > MAX_STEPS {
            return Err(ContractError::StepCount);
        }
        let mut total = 0_usize;
        for (index, step) in self.steps.iter().enumerate() {
            if !valid_label(&step.app)
                || !valid_label(&step.action)
                || !valid_label(&step.resource_kind)
            {
                return Err(ContractError::Label(index));
            }
            if step.texts.len() > MAX_TEXTS_PER_STEP {
                return Err(ContractError::TextCount(index));
            }
            let mut nodes = BTreeSet::new();
            for text in &step.texts {
                if !valid_node_id(&text.node) || !nodes.insert(text.node.as_str()) {
                    return Err(ContractError::NodeId(index));
                }
                let chars = text
                    .text
                    .chars()
                    .take(MAX_TEXT_CHARS.saturating_add(1))
                    .count();
                if text.text.trim().is_empty() || chars > MAX_TEXT_CHARS {
                    return Err(ContractError::Text(index));
                }
                if text.kind.is_some() && text.sensitivity == DeclaredSensitivity::Public {
                    return Err(ContractError::Kind(index));
                }
                total = total.saturating_add(chars);
            }
        }
        if total > MAX_TOTAL_TEXT_CHARS {
            return Err(ContractError::TotalText);
        }
        Ok(())
    }

    #[must_use]
    pub fn text(&self, step: u32, node: &str) -> Option<&ReviewText> {
        self.steps
            .get(usize::try_from(step).ok()?)?
            .texts
            .iter()
            .find(|text| text.node == node)
    }

    /// The redacted characters of `proposal`, when it addresses this request.
    #[must_use]
    pub fn redacted<'a>(&'a self, proposal: &RedactionProposal) -> Option<&'a str> {
        let text = self.text(proposal.step, &proposal.node)?;
        let start = usize::try_from(proposal.span.start).ok()?;
        let end = usize::try_from(proposal.span.end).ok()?;
        text.text.get(start..end).filter(|slice| !slice.is_empty())
    }
}

impl ReviewProposals {
    /// Check that every proposal addresses this request and every title is
    /// one line for a step that exists.
    pub fn validate_against(&self, request: &ReviewTraceRequest) -> Result<(), ContractError> {
        if self.redactions.len() > MAX_REDACTIONS {
            return Err(ContractError::TooManyRedactions);
        }
        for proposal in &self.redactions {
            if request.text(proposal.step, &proposal.node).is_none() {
                return Err(ContractError::UnknownText);
            }
            if proposal.span.start >= proposal.span.end || request.redacted(proposal).is_none() {
                return Err(ContractError::BadSpan);
            }
        }
        let mut steps = BTreeSet::new();
        for title in &self.titles {
            let exists = usize::try_from(title.step).is_ok_and(|step| step < request.steps.len());
            let valid = match &title.title {
                TitleText::Model { text } => valid_title(text),
                TitleText::Label(label) => label.args().iter().all(|(_, value)| valid_label(value)),
            };
            if !exists || !steps.insert(title.step) || !valid {
                return Err(ContractError::BadTitle);
            }
        }
        Ok(())
    }
}

/// Validate and review without a model: the deterministic floor, with its titles.
pub fn review_without_model(
    request: &ReviewTraceRequest,
) -> Result<ReviewProposals, ContractError> {
    review(request, None)
}

/// Validate and review text fields without generating walkthrough titles.
/// Uses the same deterministic floor, proposal merging and review bounds as
/// recording review; callers displaying only fields need no unused titles.
///
/// # Errors
/// Refuses invalid requests and oversized redaction sets without truncating.
pub fn review_redactions_without_model(
    request: &ReviewTraceRequest,
) -> Result<Vec<RedactionProposal>, ContractError> {
    request.validate()?;
    let answer = ReviewProposals {
        redactions: merge::merge_redactions(request, floor(request), Vec::new()),
        titles: Vec::new(),
    };
    answer.validate_against(request)?;
    Ok(answer.redactions)
}

/// Validate and review a request, retaining the deterministic floor even when
/// the model proposes no redactions. No capture or submission happens here.
///
/// # Errors
/// Returns an error for an invalid request, invalid model answer or an answer
/// exceeding the review bounds. The floor is never truncated to fit a limit.
pub fn review(
    request: &ReviewTraceRequest,
    model: Option<ReviewProposals>,
) -> Result<ReviewProposals, ContractError> {
    request.validate()?;
    if let Some(answer) = &model {
        answer.validate_against(request)?;
    }
    let answer = merge(request, floor(request), model);
    answer.validate_against(request)?;
    Ok(answer)
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub fn text(node: &str, sensitivity: DeclaredSensitivity, text: &str) -> ReviewText {
        ReviewText {
            node: node.into(),
            sensitivity,
            kind: None,
            origin: TextOrigin::Semantic,
            text: text.into(),
        }
    }

    pub fn step(kind: StepKind, action: &str, texts: Vec<ReviewText>) -> ReviewStep {
        ReviewStep {
            kind,
            outcome: StepOutcome::Done,
            app: "Mail".into(),
            action: action.into(),
            resource_kind: "mail.draft".into(),
            texts,
        }
    }

    pub fn request(steps: Vec<ReviewStep>) -> ReviewTraceRequest {
        ReviewTraceRequest { steps }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{request, step, text};
    use super::*;

    #[test]
    fn checked_review_refuses_invalid_input_and_model_spans() {
        assert_eq!(
            review(&request(Vec::new()), None),
            Err(ContractError::StepCount)
        );
        let trace = request(vec![step(
            StepKind::Invoked,
            "Send",
            vec![text("r:body", DeclaredSensitivity::Public, "é@example.org")],
        )]);
        let model = ReviewProposals {
            redactions: vec![RedactionProposal {
                step: 0,
                node: "r:body".into(),
                span: Span { start: 1, end: 2 },
                category: RedactionCategory::FreeFormPersonal,
                basis: RedactionBasis::Model,
                confidence: Confidence::CERTAIN,
            }],
            titles: Vec::new(),
        };
        assert_eq!(review(&trace, Some(model)), Err(ContractError::BadSpan));
    }

    #[test]
    fn checked_review_keeps_the_floor_when_the_model_has_no_proposals() {
        let trace = request(vec![step(
            StepKind::Invoked,
            "Send",
            vec![text(
                "r:body",
                DeclaredSensitivity::Personal,
                "private diagnosis",
            )],
        )]);
        let answer = review(
            &trace,
            Some(ReviewProposals {
                redactions: Vec::new(),
                titles: Vec::new(),
            }),
        )
        .unwrap();
        assert!(
            answer
                .redactions
                .iter()
                .any(|proposal| proposal.basis == RedactionBasis::Declared
                    && trace.redacted(proposal) == Some("private diagnosis"))
        );
    }

    #[test]
    fn checked_text_review_preserves_declared_and_pattern_redactions() {
        let trace = request(vec![step(
            StepKind::Invoked,
            "Send",
            vec![
                text("issue", DeclaredSensitivity::Public, "Jana token=abcd"),
                text("name", DeclaredSensitivity::Personal, "Jana"),
            ],
        )]);
        let redactions = review_redactions_without_model(&trace).unwrap();
        assert_eq!(redactions.len(), 3);
        for (node, value, basis) in [
            ("issue", "Jana", RedactionBasis::Declared),
            ("issue", "abcd", RedactionBasis::Pattern),
            ("name", "Jana", RedactionBasis::Declared),
        ] {
            assert!(redactions.iter().any(|proposal| {
                proposal.node == node
                    && proposal.basis == basis
                    && trace.redacted(proposal) == Some(value)
            }));
        }
        assert_eq!(redactions, review_without_model(&trace).unwrap().redactions);
        let mut invalid = trace;
        invalid.steps[0].texts[0].node.clear();
        assert!(review_redactions_without_model(&invalid).is_err());
    }

    #[test]
    fn checked_review_refuses_an_oversized_floor_without_dropping_redactions() {
        let mut steps = Vec::new();
        for index in 0..MAX_STEPS {
            let texts = (0..9)
                .map(|node| {
                    text(
                        &format!("r:{node}"),
                        DeclaredSensitivity::Personal,
                        &format!("value_{index}_{node}"),
                    )
                })
                .collect();
            steps.push(step(StepKind::Invoked, "Send", texts));
        }
        let trace = request(steps);
        trace.validate().unwrap();
        assert!(floor(&trace).len() > MAX_REDACTIONS);
        assert_eq!(review(&trace, None), Err(ContractError::TooManyRedactions));
        assert_eq!(
            review_redactions_without_model(&trace),
            Err(ContractError::TooManyRedactions)
        );
    }

    #[test]
    fn a_secret_sensitivity_cannot_be_deserialized_into_a_request() {
        let wire = r#"{"steps":[{"kind":"invoked","outcome":"done","app":"Mail","action":"Send","resource_kind":"mail.draft","texts":[{"node":"r:pw","sensitivity":"secret","origin":"semantic","text":"hunter2"}]}]}"#;
        assert!(serde_json::from_str::<ReviewTraceRequest>(wire).is_err());
    }

    #[test]
    fn requests_are_bounded() {
        assert_eq!(
            request(Vec::new()).validate(),
            Err(ContractError::StepCount)
        );
        let many = vec![step(StepKind::Invoked, "Send", Vec::new()); MAX_STEPS + 1];
        assert_eq!(request(many).validate(), Err(ContractError::StepCount));

        let duplicate = request(vec![step(
            StepKind::Edited,
            "Type",
            vec![
                text("r:body", DeclaredSensitivity::Public, "a"),
                text("r:body", DeclaredSensitivity::Public, "b"),
            ],
        )]);
        assert_eq!(duplicate.validate(), Err(ContractError::NodeId(0)));

        let long = "x".repeat(MAX_TEXT_CHARS + 1);
        let oversized = request(vec![step(
            StepKind::Edited,
            "Type",
            vec![text("r:body", DeclaredSensitivity::Public, &long)],
        )]);
        assert_eq!(oversized.validate(), Err(ContractError::Text(0)));

        let chunk = "y".repeat(MAX_TEXT_CHARS);
        let texts = (0..=MAX_TOTAL_TEXT_CHARS / MAX_TEXT_CHARS)
            .map(|index| text(&format!("r:{index}"), DeclaredSensitivity::Public, &chunk))
            .collect();
        let total = request(vec![step(StepKind::Edited, "Type", texts)]);
        assert_eq!(total.validate(), Err(ContractError::TotalText));

        let bad_label = request(vec![step(StepKind::Invoked, "Send\nnow", Vec::new())]);
        assert_eq!(bad_label.validate(), Err(ContractError::Label(0)));
    }

    #[test]
    fn answers_must_address_the_request() {
        let request = request(vec![step(
            StepKind::Edited,
            "Type",
            vec![text("r:to", DeclaredSensitivity::Public, "zoë@example.org")],
        )]);
        let proposal = |step, node: &str, start, end| RedactionProposal {
            step,
            node: node.into(),
            span: Span { start, end },
            category: RedactionCategory::Email,
            confidence: Confidence::CERTAIN,
            basis: RedactionBasis::Model,
        };
        let answer = |redaction| ReviewProposals {
            redactions: vec![redaction],
            titles: Vec::new(),
        };
        assert!(
            answer(proposal(0, "r:to", 0, 16))
                .validate_against(&request)
                .is_ok()
        );
        assert_eq!(
            answer(proposal(1, "r:to", 0, 4)).validate_against(&request),
            Err(ContractError::UnknownText)
        );
        // Byte 3 is inside `ë`.
        assert_eq!(
            answer(proposal(0, "r:to", 0, 3)).validate_against(&request),
            Err(ContractError::BadSpan)
        );
        assert_eq!(
            answer(proposal(0, "r:to", 4, 4)).validate_against(&request),
            Err(ContractError::BadSpan)
        );
        let titled = ReviewProposals {
            redactions: Vec::new(),
            titles: vec![StepTitle {
                step: 3,
                title: TitleText::Model {
                    text: "You typed".into(),
                },
            }],
        };
        assert_eq!(
            titled.validate_against(&request),
            Err(ContractError::BadTitle)
        );
    }

    #[test]
    fn confidence_is_a_percentage_on_the_wire() {
        assert!(serde_json::from_str::<Confidence>("101").is_err());
        assert_eq!(
            serde_json::from_str::<Confidence>("42").unwrap().percent(),
            42
        );
    }
}
