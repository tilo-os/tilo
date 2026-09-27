//! Review, on the device: the walk-through a recording passes before it
//! leaves.
//!
//! The walk-through is generated from the trace, not narrated over a video.
//! [`tilo_feedback_review::trace::review_input`] reads the recording into
//! steps and bounded review requests; each request is reviewed on its own
//! (the deterministic floor always, the local model's proposals over it
//! when one answered), and this walk-through lays the answers out step by
//! step. The person can hide more, and cut or keep each step. Nothing the
//! floor proposes can be taken back here: the floor is the promise that a
//! declared personal value never leaves.

use std::collections::BTreeSet;

use serde::Serialize;
use session_trace::{Offset, SessionTrace};
use tilo_actions::InvocationId;
use tilo_feedback_review::trace::{ReviewBatch, TraceRedaction, TraceReview, review_input};
use tilo_feedback_review::{
    Confidence, RedactionBasis, RedactionCategory, RedactionProposal, ReviewProposals, ReviewText,
    Span, StepKind, StepOutcome, TitleText,
};

/// One step of the walk-through: one step of the recording, which review
/// may have seen as several bounded parts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Step {
    pub index: u32,
    #[serde(skip)]
    pub at: Offset,
    pub app_id: String,
    pub app_label: String,
    #[serde(skip)]
    pub kind: StepKind,
    #[serde(skip)]
    pub outcome: StepOutcome,
    pub action: String,
    #[serde(skip)]
    pub invocation: Option<InvocationId>,
    /// `(batch, step within the batch's request)` for each part.
    #[serde(skip)]
    parts: Vec<(usize, u32)>,
}

impl Step {
    pub fn failed(&self) -> bool {
        matches!(self.outcome, StepOutcome::Failed | StepOutcome::Rejected)
    }

    /// Milliseconds into the recording.
    pub fn at_ms(&self) -> u64 {
        self.at.as_millis()
    }
}

/// The capture-protected apps a trace marks as present, each once, by the
/// name a person knows them by.
fn protected_apps(
    trace: &SessionTrace,
    app_name: &dyn Fn(&str) -> String,
) -> Vec<(String, String)> {
    let mut apps: Vec<(String, String)> = Vec::new();
    for marker in &trace.environment.protected {
        if !apps.iter().any(|(app, _)| *app == marker.app_id) {
            apps.push((marker.app_id.clone(), app_name(&marker.app_id)));
        }
    }
    apps
}

/// Whether replaying the session reproduces what its frames show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Verdict {
    /// The recording kept no frames, so there is nothing to check against.
    #[default]
    Unchecked,
    /// This device cannot replay the session.
    Unavailable,
    /// Replaying off the event loop.
    Checking,
    Checked(session_trace::divergence::DivergenceReport),
    Failed(String),
}

impl Verdict {
    /// The layer the session is summarized with once checked.
    pub const fn layer(&self) -> Option<tilo_reference::SessionLayer> {
        match self {
            Self::Checked(report) => Some(report.layer),
            _ => None,
        }
    }
}

/// The places replay does not vouch for, each once: the innermost region
/// of a step, not the containers around it too.
pub fn diverging_places(
    report: &session_trace::divergence::DivergenceReport,
) -> Vec<&session_trace::divergence::DivergentRegion> {
    report
        .steps
        .iter()
        .flat_map(|step| {
            step.regions.iter().filter(|region| {
                !step.regions.iter().any(|inner| {
                    inner.bounds != region.bounds && region.bounds.contains(&inner.bounds)
                })
            })
        })
        .collect()
}

/// Which model titled the steps and proposed redactions, and what it saw.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ModelUse {
    /// Not asked yet.
    #[default]
    Pending,
    /// A supervised model on this device: it saw everything.
    OnDevice { model: String },
    /// A hosted or unknown endpoint: it saw placeholders only.
    Hosted { model: String, location: String },
    /// No usable model; the deterministic floor and action labels stand.
    Floor,
    /// The hub's offer to set one up, shown in place and waiting for the
    /// person's answer: the recommendation first, then the alternatives.
    /// Nothing downloads until one is confirmed.
    Offer {
        choices: Vec<SetUpChoice>,
        /// Whether the alternatives are shown.
        expanded: bool,
    },
    /// A model is being set up in place; the floor stands meanwhile.
    SettingUp {
        model: String,
        progress: Option<String>,
    },
    /// The model did not answer usably; the floor stands.
    Failed { reason: String },
}

/// One way to set the review model up, as the AI hub offers it: its
/// set-up action on the review task, and that action's own words.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SetUpChoice {
    /// The hub action that sets it up (`set-up-on-device`, or
    /// `set-up-on-device-<rank>` for an alternative).
    pub action: String,
    /// What would be set up: the model, its engine and the device.
    pub label: String,
    /// What it is confirmed against: size, license, memory and speed.
    pub terms: String,
    /// How much room it leaves (`fit-excellent`, `fit-good`, `fit-tight`).
    pub fit: Option<String>,
    /// How fast it answers (`speed-interactive`, `speed-slow`).
    pub speed: Option<String>,
}

/// The review of one session.
#[derive(Clone, Debug, PartialEq)]
pub struct Review {
    /// Record id of the session under review.
    pub record: String,
    pub input: TraceReview,
    /// Per batch: the floor's answer, or the merged answer once the model
    /// replied.
    pub proposals: Vec<ReviewProposals>,
    /// Per batch: what the person hid by pointing at it.
    added: Vec<Vec<RedactionProposal>>,
    pub steps: Vec<Step>,
    pub cut: BTreeSet<u32>,
    pub selected: u32,
    pub model: ModelUse,
    pub duration: Offset,
    pub generation: Option<u64>,
    pub verdict: Verdict,
    /// Capture-protected apps that were present, as `(app id, name)`: the
    /// recording kept nothing of them, and the walk-through shows the
    /// capture placeholder for each.
    pub protected: Vec<(String, String)>,
}

/// One text of a step, with the batch it belongs to.
#[derive(Clone, Copy, Debug)]
pub struct StepText<'a> {
    batch: usize,
    local: u32,
    pub text: &'a ReviewText,
}

impl Review {
    /// Build the walk-through of `trace`, reviewed by the floor until a
    /// model answers. `app_name` is the name a person knows an app by.
    pub fn from_trace(
        record: &str,
        trace: &SessionTrace,
        app_name: &dyn Fn(&str) -> String,
    ) -> Self {
        let input = review_input(trace, app_name);
        let proposals: Vec<ReviewProposals> = input
            .batches
            .iter()
            .map(|batch| tilo_feedback_review::review_without_model(&batch.request))
            .collect();
        let mut steps: Vec<Step> = input
            .steps
            .iter()
            .enumerate()
            .map(|(index, step)| Step {
                index: u32::try_from(index).unwrap_or(u32::MAX),
                at: step.at,
                app_id: step
                    .route
                    .as_ref()
                    .map_or_else(String::new, |route| route.app_id.clone()),
                app_label: String::new(),
                kind: StepKind::Invoked,
                outcome: StepOutcome::Done,
                action: String::new(),
                invocation: step.invocation,
                parts: Vec::new(),
            })
            .collect();
        for (batch_index, batch) in input.batches.iter().enumerate() {
            for (local, (review_step, trace_step)) in batch
                .request
                .steps
                .iter()
                .zip(&batch.trace_steps)
                .enumerate()
            {
                let Some(step) = steps.get_mut(*trace_step) else {
                    continue;
                };
                if step.parts.is_empty() {
                    step.app_label.clone_from(&review_step.app);
                    step.kind = review_step.kind;
                    step.outcome = review_step.outcome;
                    step.action.clone_from(&review_step.action);
                }
                step.parts
                    .push((batch_index, u32::try_from(local).unwrap_or(u32::MAX)));
            }
        }
        Self {
            record: record.to_string(),
            added: vec![Vec::new(); input.batches.len()],
            input,
            proposals,
            steps,
            cut: BTreeSet::new(),
            selected: 0,
            model: ModelUse::Pending,
            duration: trace.duration,
            generation: trace
                .environment
                .image
                .as_ref()
                .and_then(|image| image.generation),
            verdict: Verdict::Unchecked,
            protected: protected_apps(trace, app_name),
        }
    }

    /// The step an offset of the trace falls in: the last one begun by
    /// then, or the first.
    pub fn step_at(&self, at: Offset) -> Option<u32> {
        self.steps
            .iter()
            .rev()
            .find(|step| step.at <= at)
            .or_else(|| self.steps.first())
            .map(|step| step.index)
    }

    /// The regions replay does not vouch for, by the step they fall in.
    pub fn divergent_regions(
        &self,
        step: u32,
    ) -> Vec<(
        &session_trace::Route,
        &session_trace::divergence::DivergentRegion,
    )> {
        let Verdict::Checked(report) = &self.verdict else {
            return Vec::new();
        };
        report
            .steps
            .iter()
            .filter(|diverging| self.step_at(diverging.at) == Some(step))
            .flat_map(|diverging| {
                diverging
                    .regions
                    .iter()
                    .map(move |region| (&diverging.route, region))
            })
            .collect()
    }

    pub fn batches(&self) -> &[ReviewBatch] {
        &self.input.batches
    }

    /// Lay a batch's merged answer (floor plus model) in place of the floor's.
    pub fn apply_model(&mut self, batch: usize, proposals: ReviewProposals) {
        if let Some(slot) = self.proposals.get_mut(batch) {
            *slot = proposals;
        }
    }

    fn step(&self, step: u32) -> Option<&Step> {
        self.steps.iter().find(|candidate| candidate.index == step)
    }

    /// The step's walk-through title: the model's words when a model wrote
    /// it, else the localized sentence built from the step.
    pub fn title(&self, step: u32) -> String {
        let Some(&(batch, local)) = self.step(step).and_then(|step| step.parts.first()) else {
            return String::new();
        };
        let text = self
            .proposals
            .get(batch)
            .and_then(|proposals| proposals.titles.iter().find(|title| title.step == local))
            .map(|title| title.title.clone())
            .or_else(|| {
                self.input
                    .batches
                    .get(batch)
                    .and_then(|batch| {
                        usize::try_from(local)
                            .ok()
                            .and_then(|l| batch.request.steps.get(l))
                    })
                    .map(|step| TitleText::Label(tilo_feedback_review::label_title(step)))
            });
        match text {
            Some(TitleText::Model { text }) => text,
            Some(TitleText::Label(label)) => crate::shell::i18n::step_title(&label),
            None => String::new(),
        }
    }

    /// Whether a model wrote the step's title.
    pub fn titled_by_model(&self, step: u32) -> bool {
        self.step(step)
            .and_then(|step| step.parts.first())
            .and_then(|&(batch, local)| {
                self.proposals
                    .get(batch)?
                    .titles
                    .iter()
                    .find(|title| title.step == local)
            })
            .is_some_and(|title| matches!(title.title, TitleText::Model { .. }))
    }

    /// Every text of a step, across its parts.
    pub fn texts(&self, step: u32) -> Vec<StepText<'_>> {
        let Some(step) = self.step(step) else {
            return Vec::new();
        };
        step.parts
            .iter()
            .filter_map(|&(batch, local)| {
                let request_step = self
                    .input
                    .batches
                    .get(batch)?
                    .request
                    .steps
                    .get(usize::try_from(local).ok()?)?;
                Some(
                    request_step
                        .texts
                        .iter()
                        .map(move |text| StepText { batch, local, text }),
                )
            })
            .flatten()
            .collect()
    }

    /// The texts on screen at a step: its own, or, for a step that brought
    /// nothing new to read (pressing Send), those of the latest step before
    /// it in the same app, which is where they are sent from and hidden.
    pub fn shown_texts(&self, step: u32) -> Vec<StepText<'_>> {
        let Some(at) = self
            .steps
            .iter()
            .position(|candidate| candidate.index == step)
        else {
            return Vec::new();
        };
        let Some(app) = self.steps.get(at).map(|step| step.app_id.as_str()) else {
            return Vec::new();
        };
        self.steps
            .iter()
            .take(at.saturating_add(1))
            .rev()
            .filter(|earlier| earlier.app_id == app)
            .map(|earlier| self.texts(earlier.index))
            .find(|texts| !texts.is_empty())
            .unwrap_or_default()
    }

    fn locate_text(&self, step: u32, node: &str) -> Option<StepText<'_>> {
        self.shown_texts(step)
            .into_iter()
            .find(|text| text.text.node == node)
    }

    /// Every redaction on one text, the floor's, the model's and the
    /// person's, as sorted, merged spans.
    pub fn spans(&self, step: u32, node: &str) -> Vec<Span> {
        let Some(text) = self.locate_text(step, node) else {
            return Vec::new();
        };
        let mut spans: Vec<Span> = self
            .proposals
            .get(text.batch)
            .map(|proposals| proposals.redactions.as_slice())
            .unwrap_or_default()
            .iter()
            .chain(
                self.added
                    .get(text.batch)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            )
            .filter(|proposal| proposal.step == text.local && proposal.node == node)
            .map(|proposal| proposal.span)
            .collect();
        spans.sort();
        let mut merged: Vec<Span> = Vec::new();
        for span in spans {
            match merged.last_mut() {
                Some(last) if span.start <= last.end => last.end = last.end.max(span.end),
                _ => merged.push(span),
            }
        }
        merged
    }

    /// The text as it will leave: every redacted run masked to its shape.
    pub fn redacted(&self, step: u32, node: &str) -> Option<String> {
        let text = self.locate_text(step, node)?;
        Some(apply_spans(&text.text.text, &self.spans(step, node)))
    }

    /// Redact `span` of one text, by the person's own hand.
    ///
    /// # Errors
    ///
    /// A message key when the step, text or span does not exist, or the span
    /// is not on character boundaries.
    pub fn redact(&mut self, step: u32, node: &str, span: Span) -> Result<(), &'static str> {
        let (batch, local, len, ok) = {
            let text = self
                .locate_text(step, node)
                .ok_or("remark-review-no-such-text")?;
            let value = &text.text.text;
            let start = usize::try_from(span.start).map_err(|_| "remark-review-bad-span")?;
            let end = usize::try_from(span.end).map_err(|_| "remark-review-bad-span")?;
            (
                text.batch,
                text.local,
                value.len(),
                start < end && value.is_char_boundary(start) && value.is_char_boundary(end),
            )
        };
        if !ok || usize::try_from(span.end).unwrap_or(usize::MAX) > len {
            return Err("remark-review-bad-span");
        }
        let added = self
            .added
            .get_mut(batch)
            .ok_or("remark-review-no-such-text")?;
        if !added
            .iter()
            .any(|p| p.step == local && p.node == node && p.span.contains(span))
        {
            added.push(RedactionProposal {
                step: local,
                node: node.to_string(),
                span,
                category: RedactionCategory::FreeFormPersonal,
                confidence: Confidence::CERTAIN,
                basis: RedactionBasis::Declared,
            });
        }
        Ok(())
    }

    /// Hide one text whole.
    ///
    /// # Errors
    ///
    /// When the step or text does not exist, or the text is empty.
    pub fn hide(&mut self, step: u32, node: &str) -> Result<Span, &'static str> {
        let len = self
            .locate_text(step, node)
            .ok_or("remark-review-no-such-text")?
            .text
            .text
            .len();
        let span = Span {
            start: 0,
            end: u32::try_from(len).map_err(|_| "remark-review-bad-span")?,
        };
        self.redact(step, node, span)?;
        Ok(span)
    }

    /// Redact the word under byte offset `at`: what a click on text does.
    ///
    /// # Errors
    ///
    /// As [`Self::redact`], or when `at` is not inside a word.
    pub fn redact_word_at(
        &mut self,
        step: u32,
        node: &str,
        at: usize,
    ) -> Result<Span, &'static str> {
        let value = self
            .locate_text(step, node)
            .ok_or("remark-review-no-such-text")?
            .text
            .text
            .clone();
        let span = word_at(&value, at).ok_or("remark-review-bad-span")?;
        self.redact(step, node, span)?;
        Ok(span)
    }

    /// Take back one of the person's own redactions. The floor's stay.
    pub fn unredact(&mut self, step: u32, node: &str, span: Span) -> bool {
        let Some((batch, local)) = self
            .locate_text(step, node)
            .map(|text| (text.batch, text.local))
        else {
            return false;
        };
        let Some(added) = self.added.get_mut(batch) else {
            return false;
        };
        let before = added.len();
        added.retain(|p| !(p.step == local && p.node == node && span.contains(p.span)));
        before != added.len()
    }

    pub fn set_cut(&mut self, step: u32, cut: bool) -> bool {
        if self.step(step).is_none() {
            return false;
        }
        if cut {
            self.cut.insert(step);
        } else {
            self.cut.remove(&step);
        }
        true
    }

    pub fn is_cut(&self, step: u32) -> bool {
        self.cut.contains(&step)
    }

    /// Steps that will be sent.
    pub fn kept(&self) -> impl Iterator<Item = &Step> {
        self.steps
            .iter()
            .filter(|step| !self.cut.contains(&step.index))
    }

    /// The invocations of cut steps, which the duplicate leaves out.
    pub fn cut_invocations(&self) -> std::collections::HashSet<InvocationId> {
        self.steps
            .iter()
            .filter(|step| self.cut.contains(&step.index))
            .filter_map(|step| step.invocation)
            .collect()
    }

    /// Each batch's final answer: its proposals plus the person's own.
    fn finals(&self) -> Vec<ReviewProposals> {
        self.proposals
            .iter()
            .zip(&self.added)
            .map(|(proposals, added)| {
                let mut out = proposals.clone();
                out.redactions.extend(added.iter().cloned());
                out
            })
            .collect()
    }

    /// Every value hidden anywhere in what is sent, longest first: what the
    /// duplicate masks wherever else it meets them.
    pub fn hidden_values(&self) -> Vec<String> {
        let mut values: Vec<String> = self
            .input
            .batches
            .iter()
            .zip(self.finals())
            .flat_map(|(batch, proposals)| batch.hidden_values(&proposals))
            .collect();
        values.sort_unstable_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        values.dedup();
        values
    }

    /// Every hidden run, addressed to the keyframe that showed it.
    pub fn located(&self) -> Vec<TraceRedaction> {
        self.input
            .batches
            .iter()
            .zip(self.finals())
            .flat_map(|(batch, proposals)| batch.locate(&proposals))
            .collect()
    }

    /// How many text runs are masked in what is sent.
    pub fn redaction_count(&self) -> u32 {
        let count: usize = self
            .kept()
            .map(|step| {
                self.texts(step.index)
                    .iter()
                    .map(|text| self.spans(step.index, &text.text.node).len())
                    .sum::<usize>()
            })
            .sum();
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    /// The bounded summary the attachment card shows.
    pub fn summary(&self, layer: tilo_reference::SessionLayer) -> tilo_reference::SessionSummary {
        let mut apps: Vec<String> = self
            .kept()
            .map(|step| step.app_id.clone())
            .filter(|app| !app.is_empty())
            .collect();
        apps.sort();
        apps.dedup();
        apps.truncate(16);
        tilo_reference::SessionSummary {
            apps,
            duration_ms: self.duration.as_millis(),
            steps: u32::try_from(self.kept().count()).unwrap_or(u32::MAX),
            layer,
            redactions: tilo_reference::Redactions {
                text: self.redaction_count(),
                regions: 0,
            },
            generation: self.generation,
        }
    }
}

/// `text` with every span replaced by the mask, one mask per character, so
/// the result keeps the shape of what it hides.
pub fn apply_spans(text: &str, spans: &[Span]) -> String {
    let mut out = String::with_capacity(text.len());
    for (offset, c) in text.char_indices() {
        let at = u32::try_from(offset).unwrap_or(u32::MAX);
        let hidden = spans.iter().any(|span| span.start <= at && at < span.end);
        if hidden && !c.is_whitespace() {
            out.push(tilo_feedback_review::MASK);
        } else {
            out.push(c);
        }
    }
    out
}

/// The word (a run of non-whitespace) holding byte offset `at`.
pub fn word_at(text: &str, at: usize) -> Option<Span> {
    let mut start: Option<usize> = None;
    for (offset, c) in text.char_indices() {
        let end = offset.saturating_add(c.len_utf8());
        if c.is_whitespace() {
            if let Some(begin) = start.take()
                && (begin..offset).contains(&at)
            {
                return span(begin, offset);
            }
        } else {
            if start.is_none() {
                start = Some(offset);
            }
            if end == text.len()
                && let Some(begin) = start
                && (begin..end).contains(&at)
            {
                return span(begin, end);
            }
        }
    }
    None
}

fn span(start: usize, end: usize) -> Option<Span> {
    Some(Span {
        start: u32::try_from(start).ok()?,
        end: u32::try_from(end).ok()?,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use session_trace::{
        Event, InspectKeyframe, Origin, PresenceChange, PresenceUpdate, Route, TimedEvent,
    };
    use tilo_actions::{
        ActionApp, ActionResource, InteractionAction, InteractionActor, InteractionActorKind,
        InteractionAnchor, InteractionPhase, SemanticInteraction, Sensitivity,
    };
    use tilo_reference::inspect::{
        Content, Identity, InspectReply, InspectedNode, NodeState, Pixels, SurfaceRect,
    };

    fn interaction(invocation: u128, phase: InteractionPhase, label: &str) -> SemanticInteraction {
        SemanticInteraction {
            version: tilo_actions::INTERACTION_VERSION,
            invocation: InvocationId(uuid::Uuid::from_u128(invocation)),
            sequence: 1,
            actor: InteractionActor {
                id: "me".into(),
                kind: InteractionActorKind::Person,
            },
            phase,
            action: InteractionAction {
                id: format!("a{invocation}"),
                label: label.into(),
                app: Some(ActionApp {
                    id: "so.tilo.mail".into(),
                    label: "Mail".into(),
                }),
            },
            resource: ActionResource {
                id: "mail.draft:1".into(),
                kind: "mail.draft".into(),
                label: "Draft".into(),
                sensitivity: tilo_actions::Sensitivity::Public,
            },
            anchor: InteractionAnchor::Resource,
            group: None,
        }
    }

    fn presence(at: u64, interaction: SemanticInteraction) -> TimedEvent {
        TimedEvent {
            at: Offset(at),
            event: Event::Presence(PresenceUpdate {
                participant: "me".into(),
                room: "room".into(),
                scope: Vec::new(),
                change: PresenceChange::Interaction(Box::new(interaction)),
            }),
        }
    }

    pub(crate) fn node(
        id: &str,
        role: &str,
        label: &str,
        value: Option<&str>,
        sensitivity: Sensitivity,
    ) -> InspectedNode {
        InspectedNode {
            id: id.into(),
            identity: Identity::Durable,
            role: role.into(),
            label: Some(label.into()),
            value: value.map(Into::into),
            sensitivity,
            content: if value.is_some() {
                Content::Value
            } else {
                Content::Label
            },
            bounds: Some(SurfaceRect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 20.0,
            }),
            visible: Some(SurfaceRect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 20.0,
            }),
            pixels: Pixels::Described,
            state: NodeState::default(),
            children: Vec::new(),
            reference: None,
        }
    }

    pub(crate) fn keyframe(at: u64, nodes: Vec<InspectedNode>) -> TimedEvent {
        let reply = InspectReply {
            nodes,
            ..InspectReply::empty()
        };
        TimedEvent {
            at: Offset(at),
            event: Event::Inspect(InspectKeyframe {
                route: Route::new("so.tilo.mail", "room", ""),
                reply,
            }),
        }
    }

    pub(crate) fn trace() -> SessionTrace {
        let screen = || {
            vec![
                node(
                    "t:to",
                    "text_input",
                    "To",
                    Some("Ada Lovelace"),
                    Sensitivity::Personal,
                ),
                node(
                    "t:subject",
                    "text_input",
                    "Subject",
                    Some("Quarterly numbers"),
                    Sensitivity::Public,
                ),
                node("t:send", "button", "Send", None, Sensitivity::Public),
            ]
        };
        SessionTrace {
            origin: Origin::Explicit,
            started_unix_ms: 0,
            duration: Offset(9_000_000),
            environment: session_trace::Environment::default(),
            snapshots: Vec::new(),
            timeline: vec![
                keyframe(500, screen()),
                presence(1_000, interaction(1, InteractionPhase::Applying, "Attach")),
                presence(1_500, interaction(1, InteractionPhase::Committed, "Attach")),
                keyframe(2_500, screen()),
                presence(
                    3_000,
                    interaction(
                        2,
                        InteractionPhase::Failed {
                            code: None,
                            message: "Couldn't send".into(),
                        },
                        "Send",
                    ),
                ),
            ],
            tags: Vec::new(),
            layer: session_trace::SessionLayer::Logic,
            redactions: session_trace::Redactions::default(),
            truncated: false,
        }
    }

    fn review() -> Review {
        Review::from_trace("uuid:1", &trace(), &|_| "Mail".to_string())
    }

    /// The step whose text `node` shows, for tests addressing texts.
    fn step_with(review: &Review, node: &str) -> u32 {
        review
            .steps
            .iter()
            .find(|step| {
                review
                    .texts(step.index)
                    .iter()
                    .any(|text| text.text.node == node)
            })
            .map(|step| step.index)
            .expect("some step shows the text")
    }

    #[test]
    fn the_walk_through_has_the_attempts_and_names_the_failure() {
        let review = review();
        assert!(review.steps.len() >= 2, "{:?}", review.steps);
        assert!(
            review.steps.iter().any(Step::failed),
            "the failed send is a step"
        );
        for step in &review.steps {
            assert!(!review.title(step.index).is_empty());
        }
    }

    #[test]
    fn the_floor_hides_the_declared_name_and_the_person_hides_more() {
        let mut review = review();
        let step = step_with(&review, "t:to");
        let to = review.redacted(step, "t:to").expect("the To field");
        assert!(
            !to.contains("Ada"),
            "a declared personal value is hidden: {to}"
        );
        let step = step_with(&review, "t:subject");
        let span = review.redact_word_at(step, "t:subject", 2).expect("a word");
        assert_eq!(
            review.redacted(step, "t:subject").as_deref(),
            Some("▮▮▮▮▮▮▮▮▮ numbers")
        );
        assert!(
            review
                .hidden_values()
                .iter()
                .any(|value| value == "Quarterly")
        );
        assert!(review.unredact(step, "t:subject", span));
        let step = step_with(&review, "t:to");
        let floor = review.spans(step, "t:to")[0];
        assert!(
            !review.unredact(step, "t:to", floor),
            "the floor's are not the person's to take back"
        );
        assert!(
            review
                .hidden_values()
                .iter()
                .any(|value| value.contains("Ada"))
        );
        assert!(
            !review.located().is_empty(),
            "hidden runs are addressed to their keyframes"
        );
    }

    #[test]
    fn a_cut_step_leaves_the_summary_and_names_its_invocation() {
        let mut review = review();
        let failed = review
            .steps
            .iter()
            .find(|step| step.failed())
            .unwrap()
            .index;
        assert!(review.set_cut(failed, true));
        assert!(!review.set_cut(99, true));
        assert_eq!(review.cut_invocations().len(), 1);
        let summary = review.summary(tilo_reference::SessionLayer::Logic);
        assert_eq!(
            summary.steps,
            u32::try_from(review.steps.len() - 1).unwrap()
        );
    }

    #[test]
    fn a_step_that_showed_nothing_new_shows_and_hides_what_was_on_screen() {
        let mut review = review();
        let failed = review
            .steps
            .iter()
            .find(|step| step.failed())
            .unwrap()
            .index;
        let shown: Vec<String> = review
            .shown_texts(failed)
            .iter()
            .map(|text| text.text.node.clone())
            .collect();
        assert!(shown.iter().any(|node| node == "t:subject"), "{shown:?}");
        review
            .hide(failed, "t:subject")
            .expect("hidden from the failed step");
        assert!(
            review
                .hidden_values()
                .iter()
                .any(|value| value == "Quarterly numbers"),
            "{:?}",
            review.hidden_values()
        );
    }

    #[test]
    fn a_word_is_found_on_character_boundaries() {
        assert_eq!(word_at("café noir", 1), Some(Span { start: 0, end: 5 }));
        assert_eq!(word_at("café noir", 7), Some(Span { start: 6, end: 10 }));
        assert_eq!(word_at("a  b", 2), None);
        assert_eq!(
            apply_spans("café noir", &[Span { start: 0, end: 5 }]),
            "▮▮▮▮ noir"
        );
    }

    #[test]
    fn a_protected_app_is_named_once_and_has_no_step() {
        let mut trace = trace();
        for room in ["room-a", "room-b"] {
            trace
                .environment
                .protected
                .push(session_trace::ProtectedApp {
                    app_id: "so.tilo.passwords".into(),
                    room: room.into(),
                });
        }
        let review = Review::from_trace("uuid:1", &trace, &|app| {
            if app == "so.tilo.passwords" {
                "Passwords".to_string()
            } else {
                "Mail".to_string()
            }
        });
        assert_eq!(
            review.protected,
            vec![("so.tilo.passwords".to_string(), "Passwords".to_string())]
        );
        assert!(
            review
                .steps
                .iter()
                .all(|step| step.app_id != "so.tilo.passwords")
        );
        assert!(
            !review
                .summary(session_trace::SessionLayer::Logic)
                .apps
                .contains(&"so.tilo.passwords".to_string())
        );
    }
}
