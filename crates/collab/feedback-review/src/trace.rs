//! From a recorded session to review requests, and from reviewed proposals
//! back to the recording.
//!
//! [`review_input`] reads a [`SessionTrace`]: the semantic interaction
//! lifecycle gives the steps a person took, inspect keyframes give what the
//! screen said. A keyframe node declared `Secret` is dropped by its type
//! before any text is read, so it cannot reach a review request. The result
//! is one or more [`ReviewBatch`]es, each a request inside the contract's
//! bounds; a long recording, a step with many texts or a very long text is
//! split rather than cut, so nothing recorded goes unreviewed.
//!
//! [`ReviewBatch::locate`] maps a batch's proposals back onto the recording:
//! the keyframe on the timeline, the node's stable id, which of its fields
//! and the byte span there. That is what building the redacted duplicate
//! needs. A value that changed while the step lasted is redacted in every
//! keyframe that shows it, including the part of it already typed.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use session_trace::{Event, Offset, PresenceChange, Route, SessionTrace};
use tilo_actions::{InteractionPhase, InvocationId, Sensitivity};
use tilo_reference::inspect::{Content, InspectReply, InspectedNode};

use crate::{
    Confidence, DeclaredSensitivity, MAX_LABEL_CHARS, MAX_STEPS, MAX_TEXT_CHARS,
    MAX_TEXTS_PER_STEP, MAX_TOTAL_TEXT_CHARS, RedactionBasis, RedactionCategory, ReviewProposals,
    ReviewStep, ReviewText, ReviewTraceRequest, Span, StepKind, StepOutcome, TextOrigin,
};

/// Roles whose appearance is a step of its own: something the app told the
/// person.
const ANNOUNCING_ROLES: &[&str] = &["alert", "alert_dialog", "dialog", "status", "banner"];

/// Which field of a node a text was read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeField {
    Value,
    Label,
}

/// One step of the recording, which may span several review steps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceStep {
    /// When it began.
    pub at: Offset,
    /// The route whose app it happened in, when known.
    pub route: Option<Route>,
    /// The semantic invocation behind it, for an invoked action.
    pub invocation: Option<InvocationId>,
}

/// A proposal, addressed to one field of one node in one keyframe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceRedaction {
    /// Index of the inspect keyframe in [`SessionTrace::timeline`].
    pub keyframe: usize,
    pub route: Route,
    /// The node's stable semantic id.
    pub node: String,
    pub field: NodeField,
    /// Byte span in that field's text in that keyframe.
    pub span: Span,
    pub category: RedactionCategory,
    pub confidence: Confidence,
    pub basis: RedactionBasis,
}

/// Where a text of the recording was shown.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shown {
    keyframe: usize,
    text: String,
}

/// One review text's origin.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TextSource {
    route: Route,
    node: String,
    field: NodeField,
    /// Byte offset of this review text inside the node's latest text.
    offset: usize,
    latest: String,
    shown: Vec<Shown>,
}

/// One bounded review request and how to map its answer back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewBatch {
    pub request: ReviewTraceRequest,
    /// For each request step, the index into [`TraceReview::steps`].
    pub trace_steps: Vec<usize>,
    /// Per request step, per review node id, where the text came from.
    sources: Vec<BTreeMap<String, TextSource>>,
}

/// A recording prepared for review.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TraceReview {
    pub steps: Vec<TraceStep>,
    pub batches: Vec<ReviewBatch>,
    /// Timeline indices of inspect keyframes that could not be read. Nothing
    /// in them was reviewed, so a redacted duplicate must leave them out.
    pub unreadable_keyframes: Vec<usize>,
}

#[derive(Clone, Debug)]
struct DraftText {
    route: Route,
    node: String,
    field: NodeField,
    sensitivity: DeclaredSensitivity,
    latest: String,
    shown: Vec<Shown>,
}

#[derive(Clone, Debug)]
struct Draft {
    step: TraceStep,
    kind: StepKind,
    outcome: StepOutcome,
    app: String,
    action: String,
    resource_kind: String,
    texts: BTreeMap<(Route, String, NodeField), DraftText>,
}

/// A label the contract accepts: control characters become spaces, it is
/// capped at [`MAX_LABEL_CHARS`], and an empty one takes `fallback`.
fn label(value: &str, fallback: &str) -> String {
    let clean = |text: &str| -> String {
        text.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(MAX_LABEL_CHARS)
            .collect::<String>()
            .trim()
            .to_owned()
    };
    let value = clean(value);
    if value.is_empty() {
        let fallback = clean(fallback);
        if fallback.is_empty() {
            "-".to_owned()
        } else {
            fallback
        }
    } else {
        value
    }
}

/// The node's content: its value, or its label when the label is the
/// content. `None` for a secret node, whatever it carries.
fn content(node: &InspectedNode) -> Option<(NodeField, DeclaredSensitivity, &str)> {
    let sensitivity = match node.sensitivity {
        Sensitivity::Secret => return None,
        Sensitivity::Personal => DeclaredSensitivity::Personal,
        Sensitivity::Public => DeclaredSensitivity::Public,
    };
    let field = match node.content {
        Content::Value => NodeField::Value,
        Content::Label => NodeField::Label,
    };
    let text = node.content_text()?;
    (!text.trim().is_empty()).then_some((field, sensitivity, text))
}

fn outcome(phase: &InteractionPhase) -> Option<StepOutcome> {
    match phase {
        InteractionPhase::Committed | InteractionPhase::Cancelled { .. } => Some(StepOutcome::Done),
        InteractionPhase::Failed { .. } => Some(StepOutcome::Failed),
        InteractionPhase::Rejected { .. } => Some(StepOutcome::Rejected),
        _ => None,
    }
}

struct Builder<'a> {
    app_name: &'a dyn Fn(&str) -> String,
    drafts: Vec<Draft>,
    invocations: HashMap<InvocationId, usize>,
    /// The announcing nodes each route showed in its previous keyframe.
    announced: BTreeMap<Route, BTreeSet<String>>,
    /// The focused node's field name, per route, from its latest keyframe.
    focused: BTreeMap<Route, String>,
    unreadable: Vec<usize>,
}

impl Builder<'_> {
    fn push(&mut self, draft: Draft) -> usize {
        self.drafts.push(draft);
        self.drafts.len().saturating_sub(1)
    }

    fn draft(&self, at: Offset, route: Option<&Route>, kind: StepKind, action: &str) -> Draft {
        let app = route.map_or_else(String::new, |route| (self.app_name)(&route.app_id));
        let app_id = route.map_or("", |route| route.app_id.as_str());
        Draft {
            step: TraceStep {
                at,
                route: route.cloned(),
                invocation: None,
            },
            kind,
            outcome: StepOutcome::Done,
            app: label(&app, app_id),
            action: label(action, &app),
            resource_kind: label(route.map_or("", |route| route.record_id.as_str()), app_id),
            texts: BTreeMap::new(),
        }
    }

    fn interaction(&mut self, at: Offset, interaction: &tilo_actions::SemanticInteraction) {
        let index = if let Some(index) = self.invocations.get(&interaction.invocation) {
            *index
        } else {
            let app = interaction.action.app.as_ref();
            let app_label = app.map_or_else(String::new, |app| {
                if app.label.trim().is_empty() {
                    (self.app_name)(&app.id)
                } else {
                    app.label.clone()
                }
            });
            let draft = Draft {
                step: TraceStep {
                    at,
                    route: None,
                    invocation: Some(interaction.invocation),
                },
                kind: StepKind::Invoked,
                outcome: StepOutcome::Done,
                app: label(&app_label, app.map_or("", |app| app.id.as_str())),
                action: label(&interaction.action.label, &interaction.action.id),
                resource_kind: label(&interaction.resource.kind, &interaction.resource.id),
                texts: BTreeMap::new(),
            };
            let index = self.push(draft);
            self.invocations.insert(interaction.invocation, index);
            index
        };
        if let (Some(outcome), Some(draft)) =
            (outcome(&interaction.phase), self.drafts.get_mut(index))
        {
            draft.outcome = outcome;
        }
    }

    fn edited(&mut self, at: Offset, route: &Route) {
        let current = self.drafts.last();
        let same = current.is_some_and(|draft| {
            draft.kind == StepKind::Edited && draft.step.route.as_ref() == Some(route)
        });
        if !same {
            let field = self.focused.get(route).cloned().unwrap_or_default();
            let draft = self.draft(at, Some(route), StepKind::Edited, &field);
            self.push(draft);
        }
    }

    fn keyframe(&mut self, at: Offset, index: usize, route: &Route, answer: &[u8]) {
        let reply = match InspectReply::decode(answer) {
            Ok(reply) if reply.refused.is_none() => reply,
            _ => {
                self.unreadable.push(index);
                return;
            }
        };
        if self.drafts.is_empty() {
            let app = (self.app_name)(&route.app_id);
            let draft = self.draft(at, Some(route), StepKind::Opened, &app);
            self.push(draft);
        }
        // An overlay that was not there before is something the app said.
        let now: BTreeSet<String> = reply
            .nodes
            .iter()
            .filter(|node| ANNOUNCING_ROLES.contains(&node.role.as_str()))
            .map(|node| node.id.clone())
            .collect();
        let before = self
            .announced
            .insert(route.clone(), now.clone())
            .unwrap_or_default();
        for node in reply
            .nodes
            .iter()
            .filter(|node| now.contains(&node.id) && !before.contains(&node.id))
        {
            // Only a public label may name a step; personal content stays text.
            if node.sensitivity == Sensitivity::Public
                && let Some(text) = node.label.as_deref().filter(|text| !text.trim().is_empty())
            {
                let draft = self.draft(at, Some(route), StepKind::Appeared, text);
                self.push(draft);
            }
        }
        if let Some(field) = reply
            .nodes
            .iter()
            .find(|node| {
                node.state.focused
                    && node.content == Content::Value
                    && node.sensitivity == Sensitivity::Public
            })
            .and_then(|node| node.label.clone())
        {
            self.focused.insert(route.clone(), field);
        }
        let Some(draft) = self.drafts.last_mut() else {
            return;
        };
        for node in &reply.nodes {
            let Some((field, sensitivity, text)) = content(node) else {
                continue;
            };
            let entry = draft
                .texts
                .entry((route.clone(), node.id.clone(), field))
                .or_insert_with(|| DraftText {
                    route: route.clone(),
                    node: node.id.clone(),
                    field,
                    sensitivity,
                    latest: String::new(),
                    shown: Vec::new(),
                });
            // A value declared personal once stays personal for the step.
            entry.sensitivity = entry.sensitivity.max(sensitivity);
            text.clone_into(&mut entry.latest);
            entry.shown.push(Shown {
                keyframe: index,
                text: text.to_owned(),
            });
        }
    }
}

/// Byte offsets that cut `text` into pieces of at most [`MAX_TEXT_CHARS`]
/// characters.
fn chunks(text: &str) -> Vec<(usize, usize)> {
    let mut bounds = Vec::new();
    let mut start = 0;
    let mut count = 0;
    for (offset, _) in text.char_indices() {
        if count == MAX_TEXT_CHARS {
            bounds.push((start, offset));
            start = offset;
            count = 0;
        }
        count = count.saturating_add(1);
    }
    bounds.push((start, text.len()));
    bounds
}

/// A review node id for `source`: the stable node id when the contract takes
/// it and it is free in this step, otherwise a short stand-in. The model
/// never sees either.
fn review_node(
    taken: &BTreeMap<String, TextSource>,
    node: &str,
    field: NodeField,
    piece: usize,
) -> String {
    let suffix = match field {
        NodeField::Value => "",
        NodeField::Label => "#label",
    };
    let piece = if piece == 0 {
        String::new()
    } else {
        format!("~{piece}")
    };
    let candidate = format!("{node}{suffix}{piece}");
    let valid = candidate.len() <= crate::MAX_NODE_ID_BYTES
        && !candidate
            .chars()
            .any(|c| c.is_control() || c.is_whitespace());
    if valid && !taken.contains_key(&candidate) {
        return candidate;
    }
    // Of taken.len() + 1 stand-ins at least one is free.
    (0..=taken.len())
        .map(|n| format!("text-{n}"))
        .find(|id| !taken.contains_key(id))
        .unwrap_or_default()
}

struct Batching {
    batches: Vec<ReviewBatch>,
    current: ReviewBatch,
    chars: usize,
}

impl Batching {
    fn empty() -> ReviewBatch {
        ReviewBatch {
            request: ReviewTraceRequest { steps: Vec::new() },
            trace_steps: Vec::new(),
            sources: Vec::new(),
        }
    }

    fn flush(&mut self) {
        if !self.current.request.steps.is_empty() {
            self.batches
                .push(std::mem::replace(&mut self.current, Self::empty()));
        }
        self.chars = 0;
    }

    fn add(&mut self, trace_step: usize, step: ReviewStep, sources: BTreeMap<String, TextSource>) {
        let chars: usize = step
            .texts
            .iter()
            .map(|text| text.text.chars().count())
            .sum();
        if self.current.request.steps.len() >= MAX_STEPS
            || self.chars.saturating_add(chars) > MAX_TOTAL_TEXT_CHARS
        {
            self.flush();
        }
        self.chars = self.chars.saturating_add(chars);
        self.current.request.steps.push(step);
        self.current.trace_steps.push(trace_step);
        self.current.sources.push(sources);
    }
}

/// Prepare `trace` for review. `app_name` turns an app id into the name the
/// person knows it by.
#[must_use]
pub fn review_input(trace: &SessionTrace, app_name: &dyn Fn(&str) -> String) -> TraceReview {
    let mut builder = Builder {
        app_name,
        drafts: Vec::new(),
        invocations: HashMap::new(),
        announced: BTreeMap::new(),
        focused: BTreeMap::new(),
        unreadable: Vec::new(),
    };
    for (index, entry) in trace.timeline.iter().enumerate() {
        match &entry.event {
            Event::Presence(update) => {
                if let PresenceChange::Interaction(interaction) = &update.change {
                    builder.interaction(entry.at, interaction);
                }
            }
            Event::Op(op) => builder.edited(entry.at, &op.route),
            Event::Inspect(frame) => builder.keyframe(entry.at, index, &frame.route, &frame.answer),
        }
    }

    let mut batching = Batching {
        batches: Vec::new(),
        current: Batching::empty(),
        chars: 0,
    };
    let mut steps = Vec::with_capacity(builder.drafts.len());
    for (trace_step, draft) in builder.drafts.into_iter().enumerate() {
        steps.push(draft.step.clone());
        let template = ReviewStep {
            kind: draft.kind,
            outcome: draft.outcome,
            app: draft.app,
            action: draft.action,
            resource_kind: draft.resource_kind,
            texts: Vec::new(),
        };
        let mut step = template.clone();
        let mut sources: BTreeMap<String, TextSource> = BTreeMap::new();
        let mut chars = 0_usize;
        let mut wrote_any = false;
        for text in draft.texts.into_values() {
            for (piece, (start, end)) in chunks(&text.latest).into_iter().enumerate() {
                let Some(part) = text.latest.get(start..end) else {
                    continue;
                };
                if part.trim().is_empty() {
                    continue;
                }
                let part_chars = part.chars().count();
                // A step that would break a bound continues as another
                // request step describing the same recorded step.
                if step.texts.len() >= MAX_TEXTS_PER_STEP
                    || chars.saturating_add(part_chars) > MAX_TOTAL_TEXT_CHARS
                {
                    batching.add(
                        trace_step,
                        std::mem::replace(&mut step, template.clone()),
                        std::mem::take(&mut sources),
                    );
                    chars = 0;
                }
                let node = review_node(&sources, &text.node, text.field, piece);
                step.texts.push(ReviewText {
                    node: node.clone(),
                    sensitivity: text.sensitivity,
                    kind: None,
                    origin: TextOrigin::Semantic,
                    text: part.to_owned(),
                });
                sources.insert(
                    node,
                    TextSource {
                        route: text.route.clone(),
                        node: text.node.clone(),
                        field: text.field,
                        offset: start,
                        latest: text.latest.clone(),
                        shown: text.shown.clone(),
                    },
                );
                chars = chars.saturating_add(part_chars);
                wrote_any = true;
            }
        }
        if !step.texts.is_empty() || !wrote_any {
            batching.add(trace_step, step, sources);
        }
    }
    batching.flush();
    TraceReview {
        steps,
        batches: batching.batches,
        unreadable_keyframes: builder.unreadable,
    }
}

/// Where a value redacted at `start..end` of `latest` sits in an earlier
/// state of the same field: wherever it occurs whole, and the part of it
/// inside the text `earlier` shares with `latest` at its start or its end (a
/// value still being typed, or edited around).
fn earlier_spans(
    earlier: &str,
    latest: &str,
    value: &str,
    start: usize,
    end: usize,
) -> Vec<(usize, usize)> {
    let mut spans: Vec<(usize, usize)> = earlier
        .match_indices(value)
        .map(|(at, found)| (at, at.saturating_add(found.len())))
        .collect();
    let prefix = earlier
        .char_indices()
        .zip(latest.chars())
        .find(|((_, a), b)| a != b)
        .map_or_else(|| earlier.len().min(latest.len()), |((at, _), _)| at);
    if start < prefix {
        spans.push((start, end.min(prefix)));
    }
    let suffix = earlier
        .chars()
        .rev()
        .zip(latest.chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum::<usize>()
        .min(earlier.len().saturating_sub(prefix));
    let latest_tail = latest.len().saturating_sub(suffix);
    if end > latest_tail && suffix > 0 {
        let shift = |at: usize| {
            at.saturating_sub(latest_tail)
                .saturating_add(earlier.len().saturating_sub(suffix))
        };
        spans.push((shift(start.max(latest_tail)), shift(end)));
    }
    spans.retain(|(from, to)| from < to && earlier.get(*from..*to).is_some());
    spans
}

impl ReviewBatch {
    /// The distinct values `proposals` hide, longest first: what a replay
    /// must mask wherever else it meets them (document text, presence,
    /// interaction labels).
    #[must_use]
    pub fn hidden_values(&self, proposals: &ReviewProposals) -> Vec<String> {
        let mut values: Vec<String> = proposals
            .redactions
            .iter()
            .filter_map(|proposal| self.request.redacted(proposal))
            .map(str::to_owned)
            .collect();
        values.sort_unstable_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        values.dedup();
        values
    }

    /// Address `proposals` (this batch's review) to the recording: every
    /// keyframe that showed a redacted value, at its span there.
    #[must_use]
    pub fn locate(&self, proposals: &ReviewProposals) -> Vec<TraceRedaction> {
        let mut located = Vec::new();
        for proposal in &proposals.redactions {
            let Some(source) = usize::try_from(proposal.step)
                .ok()
                .and_then(|step| self.sources.get(step))
                .and_then(|sources| sources.get(&proposal.node))
            else {
                continue;
            };
            let (Ok(start), Ok(end)) = (
                usize::try_from(proposal.span.start),
                usize::try_from(proposal.span.end),
            ) else {
                continue;
            };
            let start = source.offset.saturating_add(start);
            let end = source.offset.saturating_add(end);
            let Some(value) = source
                .latest
                .get(start..end)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            for shown in &source.shown {
                let spans: Vec<(usize, usize)> = if shown.text == source.latest {
                    vec![(start, end)]
                } else {
                    earlier_spans(&shown.text, &source.latest, value, start, end)
                };
                for (from, to) in spans {
                    let (Ok(from), Ok(to)) = (u32::try_from(from), u32::try_from(to)) else {
                        continue;
                    };
                    located.push(TraceRedaction {
                        keyframe: shown.keyframe,
                        route: source.route.clone(),
                        node: source.node.clone(),
                        field: source.field,
                        span: Span {
                            start: from,
                            end: to,
                        },
                        category: proposal.category,
                        confidence: proposal.confidence,
                        basis: proposal.basis,
                    });
                }
            }
        }
        located.sort_by(|a, b| {
            (a.keyframe, &a.node, a.field, a.span).cmp(&(b.keyframe, &b.node, b.field, b.span))
        });
        located.dedup_by(|a, b| {
            a.keyframe == b.keyframe && a.node == b.node && a.field == b.field && a.span == b.span
        });
        located
    }
}

#[cfg(test)]
mod tests {
    use session_trace::{
        Environment, InspectKeyframe, Origin, PresenceUpdate, Redactions, SessionLayer, TimedEvent,
    };
    use tilo_actions::{
        ActionApp, ActionResource, InteractionAction, InteractionActor, InteractionActorKind,
        InteractionAnchor, SemanticInteraction,
    };
    use tilo_reference::inspect::{INSPECT_VERSION, Identity, NodeState, Pixels};

    use super::*;
    use crate::{floor, merge};

    fn node(
        id: &str,
        role: &str,
        sensitivity: Sensitivity,
        label: &str,
        value: Option<&str>,
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
            bounds: None,
            visible: None,
            pixels: Pixels::Described,
            state: NodeState::default(),
            children: Vec::new(),
            reference: None,
        }
    }

    fn keyframe(at: u64, nodes: Vec<InspectedNode>) -> TimedEvent {
        let reply = InspectReply {
            version: INSPECT_VERSION,
            target: None,
            nodes,
            truncated: false,
            refused: None,
        };
        TimedEvent {
            at: Offset(at),
            event: Event::Inspect(InspectKeyframe {
                route: Route::new("so.tilo.mail", "room-1", "draft-1"),
                answer: reply.encode().unwrap(),
            }),
        }
    }

    fn send(at: u64, phase: InteractionPhase) -> TimedEvent {
        TimedEvent {
            at: Offset(at),
            event: Event::Presence(PresenceUpdate {
                participant: "person-1".into(),
                room: "room-1".into(),
                scope: Vec::new(),
                change: PresenceChange::Interaction(Box::new(SemanticInteraction {
                    version: tilo_actions::INTERACTION_VERSION,
                    invocation: InvocationId(uuid::Uuid::from_u128(1)),
                    sequence: at,
                    actor: InteractionActor {
                        id: "person-1".into(),
                        kind: InteractionActorKind::Person,
                    },
                    phase,
                    action: InteractionAction {
                        id: "mail.send".into(),
                        label: "Send".into(),
                        app: Some(ActionApp {
                            id: "so.tilo.mail".into(),
                            label: "Mail".into(),
                        }),
                    },
                    resource: ActionResource {
                        id: "mail.draft:1".into(),
                        kind: "mail.draft".into(),
                        label: "Draft".into(),
                    },
                    anchor: InteractionAnchor::Resource,
                    group: None,
                })),
            }),
        }
    }

    fn trace(timeline: Vec<TimedEvent>) -> SessionTrace {
        SessionTrace {
            origin: Origin::Explicit,
            started_unix_ms: 0,
            duration: Offset(10),
            environment: Environment::default(),
            snapshots: Vec::new(),
            timeline,
            tags: Vec::new(),
            layer: SessionLayer::Logic,
            redactions: Redactions::default(),
            truncated: false,
        }
    }

    fn mail() -> SessionTrace {
        let form = |body: &str| {
            vec![
                node(
                    "r:to",
                    "text_input",
                    Sensitivity::Personal,
                    "To",
                    Some("Jana Novák"),
                ),
                node(
                    "r:body",
                    "text_input",
                    Sensitivity::Public,
                    "Message",
                    Some(body),
                ),
                node(
                    "r:password",
                    "text_input",
                    Sensitivity::Secret,
                    "Password",
                    Some("hunter2"),
                ),
                node("b:send", "button", Sensitivity::Public, "Send", None),
            ]
        };
        let mut failed = form("Mail me at jana@example.cz");
        failed.push(node(
            "t:error",
            "alert",
            Sensitivity::Public,
            "Couldn\u{2019}t send",
            None,
        ));
        trace(vec![
            keyframe(1, form("Mail me at jana@exa")),
            keyframe(2, form("Mail me at jana@example.cz")),
            send(3, InteractionPhase::Applying),
            send(
                4,
                InteractionPhase::Failed {
                    code: None,
                    message: "offline".into(),
                },
            ),
            keyframe(5, failed),
        ])
    }

    fn app_name(id: &str) -> String {
        if id == "so.tilo.mail" {
            "Mail".into()
        } else {
            id.into()
        }
    }

    #[test]
    fn steps_come_from_interactions_and_keyframes_and_secrets_never_arrive() {
        let review = review_input(&mail(), &app_name);
        assert_eq!(review.batches.len(), 1);
        let request = &review.batches[0].request;
        assert_eq!(request.validate(), Ok(()));
        let kinds: Vec<(StepKind, StepOutcome, &str)> = request
            .steps
            .iter()
            .map(|step| (step.kind, step.outcome, step.action.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                (StepKind::Opened, StepOutcome::Done, "Mail"),
                (StepKind::Invoked, StepOutcome::Failed, "Send"),
                (
                    StepKind::Appeared,
                    StepOutcome::Done,
                    "Couldn\u{2019}t send"
                ),
            ]
        );
        let encoded = format!("{request:?}");
        assert!(!encoded.contains("hunter2"));
        assert!(
            !encoded.contains("r:password"),
            "a secret node is dropped whole"
        );
        let to = request.text(0, "r:to").unwrap();
        assert_eq!(to.sensitivity, DeclaredSensitivity::Personal);
        assert_eq!(
            request.text(0, "r:body").unwrap().text,
            "Mail me at jana@example.cz"
        );
        assert_eq!(request.text(0, "b:send#label").unwrap().text, "Send");
    }

    #[test]
    fn proposals_map_back_to_every_keyframe_that_showed_the_value() {
        let recorded = mail();
        let review = review_input(&recorded, &app_name);
        let batch = &review.batches[0];
        let proposals = merge(&batch.request, floor(&batch.request), None);
        let located = batch.locate(&proposals);
        let body: Vec<(usize, Span)> = located
            .iter()
            .filter(|redaction| redaction.node == "r:body")
            .map(|redaction| (redaction.keyframe, redaction.span))
            .collect();
        // The first keyframe shows the address half typed; that half is
        // redacted there too.
        assert_eq!(
            body,
            [
                (0, Span { start: 11, end: 19 }),
                (1, Span { start: 11, end: 26 }),
                (4, Span { start: 11, end: 26 }),
            ]
        );
        let to: Vec<usize> = located
            .iter()
            .filter(|redaction| redaction.node == "r:to")
            .map(|redaction| redaction.keyframe)
            .collect();
        assert_eq!(
            to,
            [0, 1, 4],
            "the declared name, in every keyframe that shows it"
        );
        assert!(
            located
                .iter()
                .all(|redaction| redaction.route.app_id == "so.tilo.mail")
        );
        let hidden = batch.hidden_values(&proposals);
        assert_eq!(hidden, ["jana@example.cz", "Jana Novák"]);
    }

    #[test]
    fn a_long_recording_is_split_into_bounded_requests_without_dropping_text() {
        let long = "word ".repeat(1_000);
        let nodes: Vec<InspectedNode> = (0..60)
            .map(|n| node(&format!("r:{n}"), "text", Sensitivity::Public, &long, None))
            .collect();
        let review = review_input(&trace(vec![keyframe(1, nodes)]), &app_name);
        let mut reviewed = 0;
        for batch in &review.batches {
            assert_eq!(batch.request.validate(), Ok(()));
            reviewed += batch
                .request
                .steps
                .iter()
                .flat_map(|step| &step.texts)
                .map(|text| text.text.len())
                .sum::<usize>();
            assert!(batch.trace_steps.iter().all(|step| *step == 0));
        }
        assert_eq!(reviewed, long.len() * 60);
        assert!(review.batches.len() > 1);
    }

    #[test]
    fn an_unreadable_keyframe_is_reported_not_skipped_silently() {
        let mut recorded = mail();
        recorded.timeline.push(TimedEvent {
            at: Offset(6),
            event: Event::Inspect(InspectKeyframe {
                route: Route::new("so.tilo.mail", "room-1", ""),
                answer: b"not an answer".to_vec(),
            }),
        });
        assert_eq!(review_input(&recorded, &app_name).unreadable_keyframes, [5]);
    }
}
