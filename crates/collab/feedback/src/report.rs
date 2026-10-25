//! A report about parts of the screen the person picked.
//!
//! The desktop resolves each pick from the retained widget tree, never from
//! pixels: which app it belongs to, its widget path, its role, the record it
//! shows, and the texts under it. Each text says where it came from: the
//! app's own fixed, localized copy ([`TextKind::Interface`]), the same for
//! everyone, or something a person or a resource put there
//! ([`TextKind::Content`]), which review treats as personal until the
//! reporter says otherwise.
//!
//! [`ReportDraft::review`] runs the deterministic floor of
//! `tilo-feedback-review`, then lays a review model's proposals over it
//! ([`model_prompt`], [`model_proposals`]). What leaves the device is the
//! structured [`ReviewedFeedback`]: hidden spans are typed [`Hidden`] parts,
//! never the original text, and each reader renders them its own way.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use tilo_feedback_review::{
    Confidence, DeclaredSensitivity, RedactionBasis, RedactionCategory, RedactionProposal,
    ReviewProposals, ReviewStep, ReviewText, ReviewTraceRequest, StepKind, StepOutcome, TextOrigin,
};
pub use tilo_feedback_review::{Exposure, ModelText};

use crate::{ContextField, Error, ReviewedFeedback, ScreenshotRef};

/// Most parts of the screen one report is about.
pub const MAX_PICKS: usize = 6;
/// Most texts read from one pick.
pub const MAX_PICK_TEXTS: usize = 24;
/// Characters kept of one text read from a pick.
pub const MAX_PICKED_TEXT_CHARS: usize = 300;
/// Characters of text read from one pick, all its texts together.
pub const MAX_PICK_CHARS: usize = 1_000;
/// Characters of the note about one pick.
pub const MAX_NOTE_CHARS: usize = 500;
/// Characters of the note about the whole report.
pub const MAX_SUMMARY_CHARS: usize = 2_000;
/// Most recent interactions a report carries.
pub const MAX_INTERACTIONS: usize = 20;
/// Most recent frames a report's timings carry.
pub const MAX_FRAMES: usize = 120;

/// Where a text under a pick came from.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    tilo_resource::Model,
)]
#[serde(rename_all = "snake_case")]
pub enum TextKind {
    /// The app's own fixed, localized copy: a button's label, a heading.
    Interface,
    /// What a person or a resource put there: a message, a name, a file.
    Content,
}

/// A rectangle in the window's logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Area {
    #[must_use]
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }

    /// Whether `other` lies wholly inside this area.
    #[must_use]
    pub fn encloses(&self, other: &Self) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.x + other.width <= self.x + self.width
            && other.y + other.height <= self.y + self.height
    }

    /// Whether the two areas overlap.
    #[must_use]
    pub fn meets(&self, other: &Self) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }
}

/// One text read from a pick, before review. Kept on this device.
#[derive(Clone, Debug, PartialEq)]
pub struct PickedText {
    pub kind: TextKind,
    pub text: String,
    /// Where it is drawn, when its node has bounds of its own.
    pub area: Option<Area>,
}

/// One picked part of the screen, resolved from the retained widget tree.
/// Kept on this device; [`ReportDraft::review`] decides what of it leaves.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pick {
    /// The app's id, as grants and launchers key it.
    pub app: String,
    /// The app's display name.
    pub app_name: String,
    /// The build the app runs: a source revision.
    pub build: Option<String>,
    /// The widget path inside the window.
    pub path: String,
    /// The semantic role, as assistive technology reads it.
    pub role: String,
    /// What kind of thing it is in the app's own words ("Message"), where
    /// the role says too little.
    pub kind: Option<String>,
    /// What the person saw it called while picking ("Message in Chat"),
    /// in their language: the review's heading for it. Never sent.
    pub caption: String,
    /// The accessible name of the picked node.
    pub label: Option<PickedText>,
    /// The kind of record the window shows (`chat.workspace`).
    pub record: Option<String>,
    /// Where it is on screen.
    pub area: Area,
    /// The texts under it, in reading order.
    pub texts: Vec<PickedText>,
}

impl Pick {
    /// Keep the texts within the report's bounds: each one cut to
    /// [`MAX_PICKED_TEXT_CHARS`], at most [`MAX_PICK_TEXTS`] of them and
    /// [`MAX_PICK_CHARS`] together. Empty texts are dropped.
    pub fn bound(&mut self) {
        let mut budget = MAX_PICK_CHARS;
        let mut kept = Vec::new();
        for mut text in std::mem::take(&mut self.texts) {
            if kept.len() >= MAX_PICK_TEXTS || budget == 0 {
                break;
            }
            truncate_chars(&mut text.text, MAX_PICKED_TEXT_CHARS.min(budget));
            if text.text.trim().is_empty() {
                continue;
            }
            budget = budget.saturating_sub(text.text.chars().count());
            kept.push(text);
        }
        self.texts = kept;
        if let Some(label) = &mut self.label {
            truncate_chars(&mut label.text, MAX_PICKED_TEXT_CHARS);
        }
        if self
            .label
            .as_ref()
            .is_some_and(|label| label.text.trim().is_empty())
        {
            self.label = None;
        }
        truncate_chars(&mut self.path, 256);
        truncate_chars(&mut self.role, 64);
        truncate_chars(&mut self.app, 128);
        truncate_chars(&mut self.app_name, 128);
    }

    /// Where content is drawn under this pick: what a screenshot of it
    /// hides. The pick itself when it is content without bounds of its own.
    #[must_use]
    pub fn content_areas(&self) -> Vec<Area> {
        let mut areas: Vec<Area> = self
            .label
            .iter()
            .chain(&self.texts)
            .filter(|text| text.kind == TextKind::Content)
            .map(|text| text.area.unwrap_or(self.area))
            .collect();
        areas.dedup();
        areas
    }
}

fn truncate_chars(text: &mut String, limit: usize) {
    if let Some((at, _)) = text.char_indices().nth(limit) {
        text.truncate(at);
    }
}

/// How an interaction ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Done,
    /// Refused before it ran.
    Refused,
    /// Ran and failed.
    Failed,
}

/// One thing the person did in an app: an action, by its id, on a resource
/// of a kind. Identifiers only; never what a field held.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[serde(deny_unknown_fields)]
pub struct Interaction {
    /// Milliseconds since the Unix epoch.
    pub at: i64,
    /// The app's id.
    pub app: String,
    /// The action's id.
    pub action: String,
    /// The kind of resource it acted on.
    pub resource: String,
    /// The action's label as the app words it ("Post a message"), in the
    /// reporter's language, for readers whose apps do not publish it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub outcome: Outcome,
}

/// How long the last frames took, oldest first, in microseconds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[serde(deny_unknown_fields)]
pub struct FrameTimings {
    pub frames: Vec<u32>,
    /// The frame budget at the display's rate.
    pub budget: u32,
}

impl FrameTimings {
    /// Frames over budget.
    #[must_use]
    pub fn slow_frames(&self) -> usize {
        self.frames
            .iter()
            .filter(|frame| **frame > self.budget)
            .count()
    }

    /// The slowest frame.
    #[must_use]
    pub fn worst(&self) -> u32 {
        self.frames.iter().copied().max().unwrap_or(0)
    }
}

/// What a hidden part of a reviewed text was.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    tilo_resource::Model,
)]
#[serde(rename_all = "snake_case")]
pub enum Hidden {
    Name,
    Email,
    Phone,
    Address,
    Account,
    Credential,
    Personal,
}

impl Hidden {
    #[must_use]
    pub const fn of(category: RedactionCategory) -> Self {
        match category {
            RedactionCategory::Name => Self::Name,
            RedactionCategory::Email => Self::Email,
            RedactionCategory::Phone => Self::Phone,
            RedactionCategory::Address => Self::Address,
            RedactionCategory::AccountNumber => Self::Account,
            RedactionCategory::CredentialLike => Self::Credential,
            RedactionCategory::FreeFormPersonal => Self::Personal,
        }
    }

    /// The typed placeholder review shows and plain-text readers print.
    #[must_use]
    pub const fn placeholder(self) -> &'static str {
        match self {
            Self::Name => "\u{2039}name\u{203A}",
            Self::Email => "\u{2039}email\u{203A}",
            Self::Phone => "\u{2039}phone\u{203A}",
            Self::Address => "\u{2039}address\u{203A}",
            Self::Account => "\u{2039}account number\u{203A}",
            Self::Credential => "\u{2039}credential\u{203A}",
            Self::Personal => "\u{2039}personal\u{203A}",
        }
    }
}

/// One run of a reviewed text: shown words, or a hidden span's kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[serde(deny_unknown_fields)]
pub struct Part {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<Hidden>,
}

/// A text as it leaves the device: what it was, in runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[serde(deny_unknown_fields)]
pub struct ReviewedText {
    pub kind: TextKind,
    pub parts: Vec<Part>,
}

impl ReviewedText {
    /// The text with each hidden span printed as `hidden` says.
    pub fn render(&self, hidden: impl Fn(Hidden) -> &'static str) -> String {
        let mut text = String::new();
        for part in &self.parts {
            match part.hidden {
                Some(kind) => text.push_str(hidden(kind)),
                None => text.push_str(&part.text),
            }
        }
        text
    }

    /// The text with typed placeholders.
    #[must_use]
    pub fn plain(&self) -> String {
        self.render(Hidden::placeholder)
    }

    fn hidden_count(&self) -> usize {
        self.parts
            .iter()
            .filter(|part| part.hidden.is_some())
            .count()
    }
}

/// Where one pick's screenshot is in the report's picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[serde(deny_unknown_fields)]
pub struct CropRef {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// One picked part of the screen as it leaves the device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[serde(deny_unknown_fields)]
pub struct ReviewedSelection {
    pub app: String,
    pub app_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    pub path: String,
    pub role: String,
    /// What kind of thing it is in the app's own words ("Message").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<ReviewedText>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub texts: Vec<ReviewedText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<ReviewedText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<CropRef>,
}

impl ReviewedSelection {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        let short = |value: &str, limit: usize| {
            value.len() <= limit && !value.chars().any(char::is_control)
        };
        if self.app.trim().is_empty()
            || !short(&self.app, 128)
            || !short(&self.app_name, 128)
            || !short(&self.path, 1_024)
            || !short(&self.role, 64)
            || self.kind.as_deref().is_some_and(|kind| !short(kind, 64))
            || self.build.as_deref().is_some_and(|build| !short(build, 64))
            || self
                .record
                .as_deref()
                .is_some_and(|record| !short(record, 128))
            || self.texts.len() > MAX_PICK_TEXTS
        {
            return Err(Error::Invalid("selection"));
        }
        for text in self.label.iter().chain(&self.texts).chain(&self.note) {
            if text.parts.len() > 256
                || text.parts.iter().any(|part| {
                    part.text.len() > 4_000 || (part.hidden.is_some() && !part.text.is_empty())
                })
            {
                return Err(Error::Invalid("selection text"));
            }
        }
        if let Some(crop) = &self.crop
            && (crop.width == 0 || crop.height == 0)
        {
            return Err(Error::Invalid("selection crop"));
        }
        Ok(())
    }
}

/// Which hidden span the reporter chose to show: in the text addressed by
/// `node`, every occurrence of `value`. A whole text shown is the text
/// itself.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Reveal {
    pub node: String,
    pub value: String,
}

/// Why a span is hidden, strongest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    /// It is content, which is personal unless the reporter shows it.
    Content,
    /// A rule found it: an address, a number, a key, a name of someone here.
    Rule,
    /// The review model proposed it.
    Model,
}

/// One span review hid, as the review sheet lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redaction {
    /// What showing or hiding it again toggles.
    pub key: Reveal,
    /// The pick whose text it is in; `None` for the report's summary.
    pub pick: Option<usize>,
    pub hidden_as: Hidden,
    pub basis: Basis,
    /// Whether it is hidden in the outgoing report now.
    pub hidden: bool,
    /// Credentials stay hidden whatever the reporter chooses.
    pub revealable: bool,
}

impl Redaction {
    /// What it hides, for the reporter's own eyes only.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.key.value
    }
}

/// How the review model took part in a review.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ModelPass {
    /// No model is available here: the rules alone reviewed it.
    #[default]
    Unavailable,
    /// Asked; the rules' review stands until it answers.
    Pending,
    /// Its proposals are laid over the rules' review.
    Applied { added: usize },
    /// It answered nothing usable; the rules' review stands.
    Failed,
}

/// Everything the person picked and wrote, before review. Kept on this
/// device; nothing in it is sent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReportDraft {
    pub picks: Vec<Pick>,
    /// A note about each pick, by its place in `picks`.
    pub notes: Vec<String>,
    /// The note about the whole report.
    pub summary: String,
    /// "This was slow": the frame timings are attached.
    pub slow: bool,
    /// The last things the person did in the picked apps, oldest first.
    pub interactions: Vec<Interaction>,
    pub timings: Option<FrameTimings>,
    /// The names of the people here, which review hides wherever they occur.
    pub names: Vec<String>,
    /// The spans the reporter chose to show.
    pub revealed: BTreeSet<Reveal>,
}

/// One review of a draft: exactly what would be sent, and every span it hid.
#[derive(Clone, Debug, PartialEq)]
pub struct ReportReview {
    pub feedback: ReviewedFeedback,
    pub redactions: Vec<Redaction>,
}

impl ReportReview {
    /// Spans hidden now.
    #[must_use]
    pub fn hidden(&self) -> usize {
        self.redactions
            .iter()
            .filter(|redaction| redaction.hidden)
            .count()
    }
}

const SUMMARY_NODE: &str = "summary";
const NAMES_STEP_APP: &str = "People here";

fn text_node(pick: usize, text: usize) -> String {
    format!("p{pick}.t{text}")
}

fn label_node(pick: usize) -> String {
    format!("p{pick}.label")
}

fn note_node(pick: usize) -> String {
    format!("p{pick}.note")
}

/// What one reviewed text addresses in a draft.
struct Addressed {
    step: u32,
    node: String,
    pick: Option<usize>,
    kind: TextKind,
    text: String,
}

impl ReportDraft {
    /// Show `key`'s span in the outgoing report, or hide it again.
    pub fn toggle(&mut self, key: &Reveal) {
        if !self.revealed.remove(key) {
            self.revealed.insert(key.clone());
        }
    }

    fn content_shown(&self, node: &str, text: &str) -> bool {
        self.revealed.contains(&Reveal {
            node: node.to_owned(),
            value: text.to_owned(),
        })
    }

    /// The review request for this draft and what each of its texts is.
    /// One step per pick, one for the summary, and one naming the people
    /// here, whose names are hidden wherever they occur.
    ///
    /// # Errors
    /// Refuses a draft with nothing to report or beyond the report's bounds.
    pub fn request(&self) -> Result<ReviewTraceRequest, Error> {
        Ok(self.addressed()?.0)
    }

    /// The step naming the people here, if any name is long enough to find.
    fn names_step(&self) -> Option<ReviewStep> {
        let names: Vec<ReviewText> = self
            .names
            .iter()
            .filter(|name| name.trim().chars().nth(2).is_some())
            .take(tilo_feedback_review::MAX_TEXTS_PER_STEP)
            .enumerate()
            .map(|(index, name)| ReviewText {
                node: format!("name{index}"),
                sensitivity: DeclaredSensitivity::Personal,
                kind: Some(RedactionCategory::Name),
                origin: TextOrigin::Semantic,
                text: name.trim().chars().take(MAX_PICKED_TEXT_CHARS).collect(),
            })
            .collect();
        (!names.is_empty()).then(|| step(NAMES_STEP_APP, "people", names))
    }

    fn addressed(&self) -> Result<(ReviewTraceRequest, Vec<Addressed>), Error> {
        if self.picks.len() > MAX_PICKS || self.interactions.len() > MAX_INTERACTIONS {
            return Err(Error::Invalid("report size"));
        }
        if self.picks.is_empty() && self.summary.trim().is_empty() {
            return Err(Error::Invalid("feedback"));
        }
        let mut steps = Vec::new();
        let mut addressed = Vec::new();
        // The people here come first: a name they go by is hidden as a
        // name even where a content text holds the same words.
        if let Some(names) = self.names_step() {
            steps.push(names);
        }
        for (index, pick) in self.picks.iter().enumerate() {
            let step_index =
                u32::try_from(steps.len()).map_err(|_| Error::Invalid("report size"))?;
            let mut texts = Vec::new();
            let mut add = |node: String, kind: TextKind, text: &str, content_shown: bool| {
                let text: String = text
                    .chars()
                    .take(MAX_PICKED_TEXT_CHARS.max(MAX_NOTE_CHARS))
                    .collect();
                if text.trim().is_empty() {
                    return;
                }
                let personal = kind == TextKind::Content && !content_shown;
                texts.push(ReviewText {
                    node: node.clone(),
                    sensitivity: if personal {
                        DeclaredSensitivity::Personal
                    } else {
                        DeclaredSensitivity::Public
                    },
                    kind: personal.then_some(RedactionCategory::FreeFormPersonal),
                    origin: TextOrigin::Semantic,
                    text: text.clone(),
                });
                addressed.push(Addressed {
                    step: step_index,
                    node,
                    pick: Some(index),
                    kind,
                    text,
                });
            };
            if let Some(label) = &pick.label {
                let node = label_node(index);
                let shown = self.content_shown(&node, &label.text);
                add(node, label.kind, &label.text, shown);
            }
            for (at, text) in pick.texts.iter().enumerate() {
                let node = text_node(index, at);
                let shown = self.content_shown(&node, &text.text);
                add(node, text.kind, &text.text, shown);
            }
            if let Some(note) = self.notes.get(index) {
                // The reporter's own words: public, though the rules and the
                // model still look through them.
                add(note_node(index), TextKind::Interface, note, true);
            }
            // The app's display name; never its id, which only routes.
            let mut picked = step(&pick.app_name, "Picked", texts);
            picked.kind = StepKind::Appeared;
            picked.resource_kind = pick
                .record
                .clone()
                .filter(|record| !record.trim().is_empty())
                .unwrap_or_else(|| "ui".into());
            steps.push(picked);
        }
        if !self.summary.trim().is_empty() {
            let step_index =
                u32::try_from(steps.len()).map_err(|_| Error::Invalid("report size"))?;
            let text: String = self.summary.chars().take(MAX_SUMMARY_CHARS).collect();
            steps.push(step(
                "Feedback",
                "Summary",
                vec![ReviewText {
                    node: SUMMARY_NODE.into(),
                    sensitivity: DeclaredSensitivity::Public,
                    kind: None,
                    origin: TextOrigin::Semantic,
                    text: text.clone(),
                }],
            ));
            addressed.push(Addressed {
                step: step_index,
                node: SUMMARY_NODE.into(),
                pick: None,
                kind: TextKind::Interface,
                text,
            });
        }
        let request = ReviewTraceRequest { steps };
        request.validate()?;
        Ok((request, addressed))
    }

    /// One text with the redactions proposed in it laid over, each listed
    /// in `redactions` so the reporter can toggle it.
    fn review_text(
        &self,
        text: &Addressed,
        proposals: &ReviewProposals,
        redactions: &mut Vec<Redaction>,
    ) -> ReviewedText {
        // Content the reporter showed stays listed, to hide it again.
        if text.kind == TextKind::Content && self.content_shown(&text.node, &text.text) {
            redactions.push(Redaction {
                key: Reveal {
                    node: text.node.clone(),
                    value: text.text.clone(),
                },
                pick: text.pick,
                hidden_as: Hidden::Personal,
                basis: Basis::Content,
                hidden: false,
                revealable: true,
            });
        }
        let mut spans: Vec<(usize, usize, RedactionCategory, Basis)> = proposals
            .redactions
            .iter()
            .filter(|proposal| proposal.step == text.step && proposal.node == text.node)
            .filter_map(|proposal| {
                let start = usize::try_from(proposal.span.start).ok()?;
                let end = usize::try_from(proposal.span.end).ok()?;
                let basis = match proposal.basis {
                    RedactionBasis::Declared
                        if text.kind == TextKind::Content
                            && start == 0
                            && end == text.text.len() =>
                    {
                        Basis::Content
                    }
                    RedactionBasis::Declared | RedactionBasis::Pattern => Basis::Rule,
                    RedactionBasis::Model => Basis::Model,
                };
                Some((start, end, proposal.category, basis))
            })
            .collect();
        spans.sort_unstable_by_key(|(start, end, ..)| (*start, std::cmp::Reverse(*end)));
        let mut parts = Vec::new();
        let mut cursor = 0_usize;
        for (start, end, category, basis) in spans {
            if start < cursor {
                continue;
            }
            let Some(value) = text.text.get(start..end) else {
                continue;
            };
            let key = Reveal {
                node: text.node.clone(),
                value: value.to_owned(),
            };
            let revealable = category != RedactionCategory::CredentialLike;
            let hidden = !(revealable && self.revealed.contains(&key));
            if !redactions.iter().any(|known| known.key == key) {
                redactions.push(Redaction {
                    key,
                    pick: text.pick,
                    hidden_as: Hidden::of(category),
                    basis,
                    hidden,
                    revealable,
                });
            }
            if !hidden {
                continue;
            }
            push_shown(&mut parts, text.text.get(cursor..start).unwrap_or_default());
            parts.push(Part {
                text: String::new(),
                hidden: Some(Hidden::of(category)),
            });
            cursor = end;
        }
        push_shown(&mut parts, text.text.get(cursor..).unwrap_or_default());
        ReviewedText {
            kind: text.kind,
            parts,
        }
    }

    /// Review this draft: the rules' floor, with `model`'s proposals laid
    /// over it when there are any. The floor is never weakened; a span is
    /// shown only where the reporter chose to show it.
    ///
    /// # Errors
    /// Refuses a draft beyond the report's bounds or an invalid model answer.
    pub fn review(&self, model: Option<ReviewProposals>) -> Result<ReportReview, Error> {
        let (request, addressed) = self.addressed()?;
        let proposals = tilo_feedback_review::review(&request, model)?;
        let mut redactions: Vec<Redaction> = Vec::new();
        let mut reviewed: Vec<ReviewedText> = Vec::new();
        for text in &addressed {
            reviewed.push(self.review_text(text, &proposals, &mut redactions));
        }
        let mut selections: Vec<ReviewedSelection> = self
            .picks
            .iter()
            .map(|pick| ReviewedSelection {
                app: pick.app.clone(),
                app_name: pick.app_name.clone(),
                build: pick.build.clone(),
                path: masked_path(&pick.path, pick),
                role: pick.role.clone(),
                kind: pick.kind.clone(),
                record: pick.record.clone(),
                label: None,
                texts: Vec::new(),
                note: None,
                crop: None,
            })
            .collect();
        let mut summary = None;
        for (text, reviewed) in addressed.iter().zip(reviewed) {
            let Some(pick) = text.pick else {
                summary = Some(reviewed);
                continue;
            };
            let Some(selection) = selections.get_mut(pick) else {
                continue;
            };
            if text.node == label_node(pick) {
                selection.label = Some(reviewed);
            } else if text.node == note_node(pick) {
                selection.note = Some(reviewed);
            } else {
                selection.texts.push(reviewed);
            }
        }
        let feedback = ReviewedFeedback {
            text: summary.map(|summary| summary.plain()).unwrap_or_default(),
            context: Vec::<ContextField>::new(),
            screenshot: None,
            selections,
            interactions: self.interactions.clone(),
            timings: self.slow.then(|| self.timings.clone()).flatten(),
        };
        Ok(ReportReview {
            feedback,
            redactions,
        })
    }
}

fn push_shown(parts: &mut Vec<Part>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = parts.last_mut()
        && last.hidden.is_none()
    {
        last.text.push_str(text);
        return;
    }
    parts.push(Part {
        text: text.to_owned(),
        hidden: None,
    });
}

/// A widget path never names content: any content text of the pick that
/// occurs in it is hidden.
fn masked_path(path: &str, pick: &Pick) -> String {
    let mut path = path.to_owned();
    for text in pick.label.iter().chain(&pick.texts) {
        if text.kind == TextKind::Content && text.text.trim().chars().nth(2).is_some() {
            path = path.replace(text.text.trim(), Hidden::Personal.placeholder());
        }
    }
    path
}

fn step(app: &str, action: &str, texts: Vec<ReviewText>) -> ReviewStep {
    ReviewStep {
        kind: StepKind::Invoked,
        outcome: StepOutcome::Done,
        app: if app.trim().is_empty() {
            "App".into()
        } else {
            app.chars()
                .take(tilo_feedback_review::MAX_LABEL_CHARS)
                .collect()
        },
        action: action.into(),
        resource_kind: "feedback".into(),
        texts,
    }
}

impl ReviewedFeedback {
    /// The report as plain text, for a reader with no richer view: the
    /// summary, then each pick with what it said and the reporter's note.
    #[must_use]
    pub fn plain(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        if !self.text.trim().is_empty() {
            lines.push(self.text.trim().to_owned());
        }
        for (index, selection) in self.selections.iter().enumerate() {
            let what = selection
                .label
                .as_ref()
                .map(ReviewedText::plain)
                .filter(|label| !label.trim().is_empty())
                .unwrap_or_else(|| selection.role.clone());
            // The app's display name; never its id, which only routes.
            let app = selection.app_name.trim();
            lines.push(if app.is_empty() {
                format!("{}. {what} ({})", index.saturating_add(1), selection.role)
            } else {
                format!(
                    "{}. {app}: {what} ({})",
                    index.saturating_add(1),
                    selection.role
                )
            });
            if let Some(note) = &selection.note {
                lines.push(format!("   \u{201C}{}\u{201D}", note.plain()));
            }
            // One line per text: a text may hold any punctuation, so no
            // separator between them can be told apart from the text itself.
            lines.extend(
                selection
                    .texts
                    .iter()
                    .map(ReviewedText::plain)
                    .filter(|text| !text.trim().is_empty())
                    .take(6)
                    .map(|text| format!("   {text}")),
            );
        }
        if let Some(timings) = &self.timings {
            lines.push(format!(
                "Slow: {} of the last {} frames over {} ms, the slowest {} ms",
                timings.slow_frames(),
                timings.frames.len(),
                timings.budget / 1_000,
                timings.worst() / 1_000
            ));
        }
        lines.join("\n")
    }

    pub(crate) fn validate_report(&self) -> Result<(), Error> {
        if self.selections.len() > MAX_PICKS
            || self.interactions.len() > MAX_INTERACTIONS
            || self
                .timings
                .as_ref()
                .is_some_and(|timings| timings.frames.len() > MAX_FRAMES)
        {
            return Err(Error::Invalid("report size"));
        }
        for selection in &self.selections {
            selection.validate()?;
        }
        for interaction in &self.interactions {
            if [&interaction.app, &interaction.action, &interaction.resource]
                .into_iter()
                .chain(&interaction.label)
                .any(|value| value.len() > 160 || value.chars().any(char::is_control))
            {
                return Err(Error::Invalid("interaction"));
            }
        }
        Ok(())
    }

    /// Hidden spans in the whole report.
    #[must_use]
    pub fn hidden_count(&self) -> usize {
        self.selections
            .iter()
            .flat_map(|selection| {
                selection
                    .label
                    .iter()
                    .chain(&selection.texts)
                    .chain(&selection.note)
            })
            .map(ReviewedText::hidden_count)
            .sum()
    }
}

/// What a review model is asked, with the texts it is shown: every text of
/// `request` as [`tilo_feedback_review::model_view`] shows it at
/// `exposure`, numbered. Declared personal values reach a model outside
/// this device only as typed placeholders.
#[must_use]
pub fn model_prompt(request: &ReviewTraceRequest, exposure: Exposure) -> (String, Vec<ModelText>) {
    let view = tilo_feedback_review::model_view(request, exposure);
    let prompt = crate::model_review::prompt(
        view.iter()
            .enumerate()
            .filter(|(_, text)| text.quotable())
            .map(|(index, text)| (index, text.shown.as_str())),
    );
    (prompt, view)
}

/// The redactions a model's `answer` to [`model_prompt`] proposes, each
/// quote addressed to the original text behind what the model saw. Quotes
/// that are not in the text, or reach into a placeholder, are dropped.
///
/// # Errors
/// Refuses an answer with no JSON object in it.
pub fn model_proposals(view: &[ModelText], answer: &str) -> Result<ReviewProposals, Error> {
    let parsed = crate::model_review::parse(answer)?;
    let confidence = Confidence::new(70).map_err(|_| Error::Invalid("model answer"))?;
    let mut redactions: Vec<RedactionProposal> = Vec::new();
    for hide in parsed
        .hide
        .into_iter()
        .take(tilo_feedback_review::MAX_REDACTIONS)
    {
        let Some(text) = view.get(hide.text) else {
            continue;
        };
        let quote = hide.quote.trim();
        if quote.is_empty() {
            continue;
        }
        for (at, found) in text.shown.match_indices(quote) {
            let Some(span) = text.original_span(at, at.saturating_add(found.len())) else {
                continue;
            };
            let proposal = RedactionProposal {
                step: text.step,
                node: text.node.clone(),
                span,
                category: hide.category.unwrap_or(RedactionCategory::FreeFormPersonal),
                basis: RedactionBasis::Model,
                confidence,
            };
            if !redactions.contains(&proposal) {
                redactions.push(proposal);
            }
        }
    }
    redactions.sort_by(|a, b| (a.step, &a.node, a.span).cmp(&(b.step, &b.node, b.span)));
    Ok(ReviewProposals {
        redactions,
        titles: Vec::new(),
    })
}

/// One pick's screenshot before masking: what the host drew of it.
pub struct CropCapture {
    pub width: u32,
    pub height: u32,
    /// Premultiplied or straight RGBA8, row-major, no padding.
    pub rgba: Vec<u8>,
    /// Where content is drawn, in this crop's pixels: hidden before encoding.
    pub sensitive: Vec<crate::PixelRect>,
}

/// The ground between crops in the report's picture.
const GAP: u32 = 16;
/// The colour masked content is painted with: a calm grey bar.
const MASK_RGBA: [u8; 4] = [0x9a, 0xa0, 0xa8, 0xff];

/// The report's one picture: every crop masked where content is drawn,
/// stacked top to bottom, encoded once. Returns where each crop is in it.
///
/// # Errors
/// Refuses no crops, malformed pixels, a sensitive area outside its crop
/// or a picture above the attachment bound.
pub fn compose_crops(
    crops: Vec<CropCapture>,
) -> Result<(crate::MaskedScreenshot, Vec<CropRef>), Error> {
    use image::ImageEncoder as _;
    if crops.is_empty() || crops.len() > MAX_PICKS {
        return Err(Error::Invalid("crops"));
    }
    let width = crops.iter().map(|crop| crop.width).max().unwrap_or(0);
    let height = crops
        .iter()
        .map(|crop| crop.height)
        .fold(0_u32, u32::saturating_add)
        .saturating_add(
            GAP.saturating_mul(u32::try_from(crops.len().saturating_sub(1)).unwrap_or(0)),
        );
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0 || height == 0 || pixels > 4_000_000 {
        return Err(Error::Invalid("capture pixels"));
    }
    let row = usize::try_from(width)
        .map_err(|_| Error::Invalid("capture pixels"))?
        .saturating_mul(4);
    let mut canvas = vec![0_u8; row.saturating_mul(usize::try_from(height).unwrap_or(0))];
    let mut places = Vec::with_capacity(crops.len());
    let mut top = 0_u32;
    for mut crop in crops {
        let crop_row = usize::try_from(crop.width).unwrap_or(0).saturating_mul(4);
        if crop.width == 0
            || crop.height == 0
            || crop.rgba.len() != crop_row.saturating_mul(usize::try_from(crop.height).unwrap_or(0))
        {
            return Err(Error::Invalid("capture pixels"));
        }
        for rect in &crop.sensitive {
            let right = rect
                .x
                .checked_add(rect.width)
                .ok_or(Error::CaptureProtected)?;
            let bottom = rect
                .y
                .checked_add(rect.height)
                .ok_or(Error::CaptureProtected)?;
            if right > crop.width || bottom > crop.height {
                return Err(Error::CaptureProtected);
            }
            for y in rect.y..bottom {
                let start = usize::try_from(y)
                    .unwrap_or(0)
                    .saturating_mul(crop_row)
                    .saturating_add(usize::try_from(rect.x).unwrap_or(0).saturating_mul(4));
                let end = start
                    .saturating_add(usize::try_from(rect.width).unwrap_or(0).saturating_mul(4));
                if let Some(run) = crop.rgba.get_mut(start..end) {
                    for pixel in run.chunks_exact_mut(4) {
                        pixel.copy_from_slice(&MASK_RGBA);
                    }
                }
            }
        }
        for y in 0..crop.height {
            let from = usize::try_from(y).unwrap_or(0).saturating_mul(crop_row);
            let to = usize::try_from(top.saturating_add(y))
                .unwrap_or(0)
                .saturating_mul(row);
            if let (Some(source), Some(target)) = (
                crop.rgba.get(from..from.saturating_add(crop_row)),
                canvas.get_mut(to..to.saturating_add(crop_row)),
            ) {
                target.copy_from_slice(source);
            }
        }
        places.push(CropRef {
            x: 0,
            y: top,
            width: crop.width,
            height: crop.height,
        });
        top = top.saturating_add(crop.height).saturating_add(GAP);
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png).write_image(
        &canvas,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )?;
    Ok((
        crate::MaskedScreenshot::from_encoded(png, width, height),
        places,
    ))
}

/// Attach the composed picture to a review: the reference, and each pick's
/// place in it.
pub fn attach_crops(feedback: &mut ReviewedFeedback, picture: &ScreenshotRef, places: &[CropRef]) {
    feedback.screenshot = Some(picture.clone());
    for (selection, place) in feedback.selections.iter_mut().zip(places) {
        selection.crop = Some(*place);
    }
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
