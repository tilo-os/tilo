//! Transient, semantic action presence.
//!
//! These updates describe an invocation's resource and lifecycle. They do not
//! write the pointer, caret, selection, or typing stores: resource actions do
//! not synthesize pointer input.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "interaction layout is bounded f64 scene geometry and checked Duration arithmetic"
)]

use super::PresenceCtx;
use super::written::written_under;
use crate::ColorCompat;
use crate::paint::PaintCtx;
use crate::permissions::MemberId;
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use std::collections::HashMap;
use tilo_action_types::InvocationId;
use tilo_actions::{Acting, InteractionPhase, SemanticInteraction};
use tilo_sort::CompactSort as _;
use vello::{
    kurbo::{Circle, Point, Rect, Stroke},
    peniko::Color,
};

/// A live phase that has not been restated stops occupying the app surface.
/// The durable Task Center remains the place to diagnose a stalled run.
pub const ACTIVE_INTERACTION_EXPIRY: Duration = Duration::from_secs(15);
/// A terminal edge remains perceptible even when an immediate action starts
/// and completes between two rendered frames.
pub const TERMINAL_INTERACTION_DWELL: Duration = Duration::from_millis(3_500);
/// A painted marker may continue from this agent's recent position on the
/// same surface. Older positions are no longer a truthful visual continuity.
const AGENT_MARKER_CONTINUITY: Duration = Duration::from_secs(8);
/// How long an agent's cursor stays on one target before it moves on: its
/// approach and a moment's rest, so a person sees where it went before the
/// next action takes it elsewhere. A faster agent's next target waits for it,
/// and only its newest waits: the targets in between are skipped, never
/// strobed through.
pub const AGENT_TARGET_DWELL: Duration = Duration::from_millis(900);
/// The shortest time between two arrival flashes of one agent's cursor: a
/// burst of phases and edits flashes once.
const AGENT_PULSE_QUIET: Duration = Duration::from_millis(700);
const EXPIRY_EPSILON: Duration = Duration::from_nanos(1);
/// How much more of an agent's would-click wash stands the moment its step was taken, fading over
/// [`crate::motion::Attention::FLASH`]: the place a person should look at, once.
const LOCUS_FLASH: f32 = 0.22;

#[derive(Debug, Clone)]
pub(super) struct InteractionPresenceEntry {
    interaction: SemanticInteraction,
    /// When the invocation began to show: its arrival, or, for an agent
    /// whose cursor is still resting on its previous target, once that rest
    /// ends ([`AGENT_TARGET_DWELL`]). Phase restatements must not restart its
    /// approach from the edge or make an old invocation look newest.
    first_received_at: Instant,
    /// Arrival of this phase.
    received_at: Instant,
    /// The first frame that drew this phase: its dwell counts from there. A page drawing a frame
    /// every few seconds would otherwise let it come and go between two frames, and its person
    /// would never see it. Until a frame draws it, it waits, for at most
    /// [`ACTIVE_INTERACTION_EXPIRY`] after it arrived.
    drawn_at: std::cell::Cell<Option<Instant>>,
    /// A widget drew its place ([`PresenceCtx::note_placed`]); a frame that keeps that drawing
    /// (cached, or culled outside its damage) still shows it, so this holds across frames.
    placed: std::cell::Cell<bool>,
    /// When its cursor last flashed for an arrival; `None` when a flash of
    /// the same agent just went off ([`AGENT_PULSE_QUIET`]).
    pulsed_at: Option<Instant>,
    /// A later action of the same agent on the same target took its place,
    /// or skipped it while it waited: it shows no more, but is remembered
    /// until it would have expired so its later phases do not bring it back.
    superseded: bool,
}

/// Local continuity for the marker which was actually painted for one agent.
/// It is deliberately separate from pointer/caret presence.
#[derive(Debug, Clone)]
pub(super) struct AgentMarkerContinuity {
    invocation: InvocationId,
    surface: String,
    origin: Point,
    last_tip: Point,
    painted_at: Instant,
}

/// Where an agent's action marker rests: the `locus` it acts on, the
/// `control` it would press there, and the `ink` around them — the text its
/// tag must never cover.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkerPlace {
    pub locus: Rect,
    pub control: Rect,
    pub ink: Vec<Rect>,
}

/// Geometry and phase of one paint-only agent action marker.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentActionMarker {
    pub invocation: InvocationId,
    pub tip: Point,
    pub target: Point,
    /// Actual cursor and agent-tile paint bounds, clipped to this viewport.
    pub bounds: Rect,
    /// Zero at first observation and one when the marker has reached its
    /// action locus. Reduced motion always reports one.
    pub approach: f64,
    /// A short phase-change emphasis, from one down to zero.
    pub pulse: f64,
}

impl PresenceCtx {
    pub(super) fn remove_interactions_for_actor(&mut self, member: &MemberId) {
        self.interactions
            .retain(|_, entry| entry.interaction.actor.member_id() != Some(member));
        self.interaction_markers
            .remove(&tilo_actions::Actor::Member(member.clone()));
        self.remove_written_by(member);
    }

    /// The colour an interaction by `acting` is drawn in: its member's, or for an automation the
    /// member's it runs for.
    fn acting_color(&self, acting: &Acting, theme: &Theme) -> Color {
        match acting.members().first() {
            Some(member) => super::render::color_for(self, member, theme),
            None => theme.tokens.colors.presence.of_rgb(super::identity_rgb(
                crate::tokens::PresenceHue::of_identity(acting.by.id()),
            )),
        }
    }

    /// Apply one broker-normalized interaction update.
    ///
    /// Returns `Ok(true)` only when visible state advanced. Replays, older
    /// sequences, identity/target substitution within an invocation, and any
    /// update after a terminal edge are ignored. Validation errors remain
    /// explicit so a trust-boundary caller can log the malformed frame.
    pub fn apply_interaction(
        &mut self,
        interaction: SemanticInteraction,
    ) -> Result<bool, tilo_actions::InteractionViolation> {
        self.apply_interaction_at(interaction, crate::time::now())
    }

    /// [`Self::apply_interaction`] as of `now`.
    pub fn apply_interaction_at(
        &mut self,
        interaction: SemanticInteraction,
        now: Instant,
    ) -> Result<bool, tilo_actions::InteractionViolation> {
        interaction.validate()?;
        if let Some(current) = self.interactions.get(&interaction.invocation) {
            let same_identity = current.interaction.actor == interaction.actor
                && current.interaction.action == interaction.action
                && same_or_made_in(
                    (
                        &current.interaction.resource.id,
                        &current.interaction.resource.kind,
                    ),
                    &interaction.resource,
                )
                && current.interaction.group == interaction.group;
            if !same_identity
                || interaction.sequence <= current.interaction.sequence
                || current.interaction.phase.is_terminal()
            {
                return Ok(false);
            }
        }

        #[cfg(feature = "model")]
        if let Some(member) = interaction
            .actor
            .member_id()
            .filter(|member| !member.is_person())
        {
            let person = interaction.actor.acted_for_person().map(MemberId::person);
            self.set_acting_for(member.clone(), person);
        }
        let placed = self.placed(interaction.invocation);
        let (first_received_at, superseded, pulsed) =
            match self.interactions.get(&interaction.invocation) {
                // What a write put in a text is named once it applied: it shows then, as a write
                // named from its start does.
                Some(entry) if writes_text(&interaction) && !writes_text(&entry.interaction) => (
                    entry.first_received_at.min(now),
                    entry.superseded,
                    entry.pulsed_at,
                ),
                Some(entry) => (entry.first_received_at, entry.superseded, entry.pulsed_at),
                None if !interaction.actor.is_person() => {
                    let (starts_at, pulsed) = self.pace_agent(&interaction, now);
                    // The characters a write put in a text are there when its
                    // phase arrives. The rest on the agent's last target still
                    // takes the targets in between off the rail and keeps the
                    // cursor's flash, but it is the cursor's rest, not the
                    // write's: a mark that waited for it named text the person
                    // could already read a second late.
                    let starts_at = if writes_text(&interaction) {
                        now
                    } else {
                        starts_at
                    };
                    (starts_at, false, pulsed)
                }
                None => (now, false, None),
            };
        // A phase within the flash its invocation set off carries that flash
        // on.
        let pulsed_at = self.pulse_of(&interaction, now).or(pulsed);
        self.supersede_on_its_items(&interaction);
        self.note_written(&interaction);
        self.interactions.insert(
            interaction.invocation,
            InteractionPresenceEntry {
                interaction,
                first_received_at,
                received_at: now,
                drawn_at: std::cell::Cell::new(None),
                placed: std::cell::Cell::new(placed),
                pulsed_at,
                superseded,
            },
        );
        Ok(!superseded)
    }

    /// When an agent's new invocation `interaction` begins to show, so its
    /// cursor can be followed: a further action on the target its cursor
    /// rests on takes that action's place, and the cursor stays; one
    /// elsewhere waits until the cursor has rested there
    /// ([`AGENT_TARGET_DWELL`]), taking the place of any that waited before
    /// it. With it, the flash of the action it takes the place of, which it
    /// carries on.
    fn pace_agent(
        &mut self,
        interaction: &SemanticInteraction,
        now: Instant,
    ) -> (Instant, Option<Instant>) {
        let actor = &interaction.actor.by;
        let mut resting: Option<Instant> = None;
        let mut pulsed = None;
        for entry in self.interactions.values_mut().filter(|entry| {
            !entry.superseded
                && entry.alive_at(now)
                && !entry.interaction.actor.is_person()
                && entry.interaction.actor.by == *actor
        }) {
            let waiting = now < entry.first_received_at;
            let same_target = entry.interaction.resource.id == interaction.resource.id
                && entry.interaction.action.id == interaction.action.id;
            if same_target && !waiting {
                entry.superseded = true;
                // The cursor stays where it rests, and its flash goes on.
                pulsed = pulsed.max(entry.pulsed_at);
                continue;
            }
            if waiting {
                entry.superseded = true;
            } else {
                resting = Some(resting.map_or(entry.first_received_at, |at| {
                    at.max(entry.first_received_at)
                }));
            }
        }
        let starts_at = resting
            .and_then(|at| at.checked_add(AGENT_TARGET_DWELL))
            .map_or(now, |rest_ends| rest_ends.max(now));
        (starts_at, pulsed)
    }

    /// An agent's step on an item takes the place of its earlier finished steps there: the item
    /// says what it did last ("renamed"), not what it did before ("created").
    fn supersede_on_its_items(&mut self, interaction: &SemanticInteraction) {
        let items = anchored_items(interaction);
        if items.is_empty() || interaction.actor.is_person() {
            return;
        }
        for entry in self.interactions.values_mut().filter(|entry| {
            entry.interaction.invocation != interaction.invocation
                && entry.interaction.actor.by == interaction.actor.by
                && entry.interaction.phase.is_terminal()
        }) {
            if anchored_items(&entry.interaction)
                .iter()
                .any(|item| items.contains(item))
            {
                entry.superseded = true;
            }
        }
    }

    /// When `interaction`'s arrival flashes its actor's cursor: now, unless
    /// a flash of the same agent went off within [`AGENT_PULSE_QUIET`].
    fn pulse_of(&self, interaction: &SemanticInteraction, now: Instant) -> Option<Instant> {
        let quiet = self.interactions.values().any(|entry| {
            !entry.interaction.actor.is_person()
                && entry.interaction.actor.by == interaction.actor.by
                && entry.pulsed_at.is_some_and(|at| {
                    now.saturating_duration_since(at.max(entry.first_received_at))
                        < AGENT_PULSE_QUIET
                })
        });
        (!quiet).then_some(now)
    }

    /// Remove interactions whose visible dwell has ended. Returns whether a
    /// repaint is warranted.
    pub fn prune_interactions(&mut self, now: Instant) -> bool {
        let before = self.interactions.len();
        self.interactions.retain(|_, entry| entry.alive_at(now));
        let interactions = &self.interactions;
        self.interaction_markers.retain(|_, marker| {
            interactions.contains_key(&marker.invocation)
                || now.saturating_duration_since(marker.painted_at) <= AGENT_MARKER_CONTINUITY
        });
        before != self.interactions.len()
    }

    /// Say that a widget drew `invocation`'s exact place, the characters a write landed in: its
    /// mark stands there from then on, however many later frames keep that drawing.
    pub fn note_placed(&self, invocation: InvocationId) {
        if let Some(entry) = self.interactions.get(&invocation) {
            entry.placed.set(true);
        }
    }

    /// Name each step on an item by what its app publishes for that item now, through
    /// `label_of`: a rename, or a note made a moment ago, reads by its current name rather
    /// than the one the step began with.
    pub fn relabel_items(&mut self, label_of: &dyn Fn(&str) -> Option<String>) {
        for entry in self.interactions.values_mut() {
            let id = tilo_actions::logical_resource_id(&entry.interaction.resource.id);
            if let Some(label) = acted_item(&entry.interaction)
                .and_then(|_| label_of(&id))
                .filter(|label| !label.trim().is_empty())
            {
                entry.interaction.resource.label = label;
            }
        }
    }

    /// Whether a widget drew `invocation`'s place ([`Self::note_placed`]).
    pub fn placed(&self, invocation: InvocationId) -> bool {
        self.interactions
            .get(&invocation)
            .is_some_and(|entry| entry.placed.get())
    }

    /// A frame drew `invocation`'s current phase at `now`: its dwell counts from the first such
    /// frame.
    pub fn note_drawn(&self, invocation: InvocationId, now: Instant) {
        if let Some(entry) = self.interactions.get(&invocation)
            && entry.drawn_at.get().is_none()
        {
            entry.drawn_at.set(Some(now));
        }
    }

    /// All currently visible interactions in deterministic display order.
    /// The host uses this read-only view to resolve generic semantic-control
    /// anchors against the current accessibility tree.
    pub fn visible_interactions(&self, now: Instant) -> Vec<SemanticInteraction> {
        self.visible_entries(now)
            .into_iter()
            .map(|entry| entry.interaction.clone())
            .collect()
    }

    /// The newest visible invocation for each agent. A host may resolve these
    /// against its current action loci and paint at most one moving marker per
    /// agent; people and older agent invocations retain the ordinary outline.
    pub fn agent_marker_interactions(&self, now: Instant) -> Vec<SemanticInteraction> {
        let mut newest = HashMap::<&tilo_actions::Actor, &InteractionPresenceEntry>::new();
        for entry in self
            .interactions
            .values()
            .filter(|entry| entry.visible_at(now) && !entry.interaction.actor.is_person())
        {
            let slot = newest.entry(&entry.interaction.actor.by).or_insert(entry);
            if marker_entry_is_newer(entry, slot) {
                *slot = entry;
            }
        }
        let mut entries = newest.into_values().collect::<Vec<_>>();
        entries.compact_sort_by(|left, right| {
            left.interaction
                .actor
                .by
                .actor_bytes()
                .cmp(&right.interaction.actor.by.actor_bytes())
        });
        entries
            .into_iter()
            .map(|entry| entry.interaction.clone())
            .collect()
    }

    /// Whether another frame is needed to retire an active dwell.
    pub fn has_visible_interactions(&self, now: Instant) -> bool {
        self.interactions
            .values()
            .any(|entry| entry.visible_at(now))
    }

    /// Whether an exact marker painted on a previous frame still needs a
    /// smooth frame. Long active-action dwell never keeps the renderer awake.
    pub fn interaction_animation_demand(&self, now: Instant, theme: &Theme) -> bool {
        !theme.reduce_motion
            && self.interaction_markers.values().any(|marker| {
                self.interactions
                    .get(&marker.invocation)
                    .is_some_and(|entry| entry.visible_at(now) && entry.animating_at(now, theme))
            })
    }

    /// Earliest discrete wake at which visible semantic activity expires, its
    /// cursor begins to move on it, or a write's mark steps down its arrival: each
    /// one is a frame the host asks for, not only an instant to sleep to. A
    /// deadline pass on its own presents no presence-only change. Continuous
    /// motion is intentionally omitted: [`Self::interaction_animation_demand`]
    /// drives those frames.
    pub fn interaction_next_wake_deadline(&self, now: Instant) -> Option<Instant> {
        self.interactions
            .values()
            .flat_map(|entry| entry.next_wakes(now))
            .flatten()
            .chain(self.written_next_wake(now))
            .map(|deadline| deadline.max(now))
            .min()
    }

    /// How strong the wash under an agent's would-click outline stands at `now`: the outline's own
    /// strength, raised by [`LOCUS_FLASH`] the moment its step was taken and fading over the
    /// arrival flash, so a person sees where the agent would press at a glance. `None` for a step
    /// that is not visible here.
    pub fn agent_locus_tint(
        &self,
        interaction: &SemanticInteraction,
        now: Instant,
        theme: &Theme,
    ) -> Option<f32> {
        let entry = self.interactions.get(&interaction.invocation)?;
        if !entry.visible_at(now) || interaction.actor.is_person() {
            return None;
        }
        let pulsed_at = entry.pulsed_at.map(|at| at.max(entry.first_received_at));
        Some(locus_tint(pulsed_at, now, theme))
    }

    /// Paint a cursor-shaped collaboration marker moving toward one exact,
    /// current action locus. This is visual narration only: it does not write
    /// pointer, focus, caret, selection, hover, or input state.
    ///
    /// Returns `None` for people, expired/stale invocations, invalid geometry,
    /// or an older invocation when the same agent has newer visible work.
    pub fn draw_agent_action_marker(
        &mut self,
        paint: &mut PaintCtx<'_>,
        interaction: &SemanticInteraction,
        place: &MarkerPlace,
        viewport: Rect,
        surface: &str,
        now: Instant,
    ) -> Option<AgentActionMarker> {
        let (locus, control) = (place.locus, place.control);
        if interaction.actor.is_person()
            || !valid_marker_rect(locus)
            || !valid_marker_rect(viewport)
        {
            return None;
        }
        let entry = self.interactions.get(&interaction.invocation)?;
        if !entry.visible_at(now)
            || &entry.interaction != interaction
            || self
                .newest_agent_entry(&interaction.actor.by, now)
                .is_none_or(|newest| newest.interaction.invocation != interaction.invocation)
        {
            return None;
        }
        let visible_locus = locus.intersect(viewport);
        if visible_locus.is_zero_area() {
            return None;
        }
        let first_received_at = entry.first_received_at;
        let pulsed_at = entry.pulsed_at.map(|at| at.max(first_received_at));
        let actor = interaction.actor.by.clone();
        let shown = interaction
            .actor
            .members()
            .first()
            .map(|member| (*member).clone());
        // The tip rests beside the control the invocation is bound to: what
        // the agent would press stays readable, framed by its outline.
        let control = control.intersect(viewport);
        let target = beside(
            if valid_marker_rect(control) {
                control
            } else {
                visible_locus
            },
            place,
            viewport,
        );
        let previous = self.interaction_markers.get(&actor);
        let cached_origin = previous.and_then(|previous| {
            if previous.surface != surface
                || !point_in_rect(viewport, previous.last_tip)
                || now.saturating_duration_since(previous.painted_at) > AGENT_MARKER_CONTINUITY
            {
                return None;
            }
            if previous.invocation == interaction.invocation {
                point_in_rect(viewport, previous.origin).then_some(previous.origin)
            } else {
                Some(previous.last_tip)
            }
        });
        // A continuity cache is a hint, never a license for a marker to sweep
        // across the whole surface after a layout jump. The nearest edge can
        // reach any in-viewport target within half the viewport diagonal.
        let max_travel = viewport_diagonal(viewport) / 2.0;
        let origin = cached_origin
            .filter(|origin| squared_distance(*origin, target).sqrt() <= max_travel)
            .unwrap_or_else(|| nearest_viewport_edge(viewport, target));

        // The agent's pointer travels to what it acts on, and its arrival
        // flashes (`motion::Follow::APPROACH`, `motion::Attention::FLASH`).
        let motion = paint.theme.motion();
        let source = crate::tokens::MotionSource::Pointer;
        let approach = crate::motion::Follow::APPROACH.progress_after(
            crate::motion::Direction::Enter,
            motion,
            source,
            now.saturating_duration_since(first_received_at),
        );
        let tip = point_lerp(origin, target, approach);
        let pulse = match pulsed_at {
            Some(pulsed_at) if !paint.theme.reduce_motion => {
                1.0 - crate::motion::Attention::FLASH.progress_after(
                    crate::motion::Direction::Exit,
                    motion,
                    source,
                    now.saturating_duration_since(pulsed_at),
                )
            }
            _ => 0.0,
        };
        let actor_color = self.acting_color(&interaction.actor, paint.theme);
        if pulse > 0.0 {
            let radius = 8.0 + 8.0 * (1.0 - pulse);
            paint.scene.stroke(
                &Stroke::new(1.5),
                paint.transform,
                actor_color.with_alpha_factor((pulse * 0.72) as f32),
                None,
                &Circle::new(target, radius),
            );
        }
        // The agent wears the nametag it wears wherever it points, in its colour: its name alone,
        // one tag for the agent, clear of the text around it. What it is doing is said by the
        // caret it writes with, the card of the action it took, and its line among who is here —
        // not by a second sentence on the tag, which a long one pushes out of the window.
        let words = actor_label(self, interaction);
        // A caret it writes with may wear its tag already (an agent writing in a document): the
        // pointer then goes without, one tag for one agent, at its caret. The caret's tag names
        // the agent ("Claude Code") where this one would say what it does; a second tag
        // beside it, or floating where the pointer is, says the same agent twice.
        // Its caret is where it acts then, so no pointer is drawn either: a pointer of its own
        // stood on the very words the caret had just written, or apart from them where it last
        // pointed. A caret anywhere else (an agent writing in one field while it presses
        // a control in another) is no mark on *this* place: the place it would press keeps its own
        // tag, or a bare outline is all a person sees of the step.
        let worn = self.presence_marks().iter().any(|mark| {
            Some(&mark.user) == shown.as_ref()
                && super::caret_tag::overlaps(mark.rect, visible_locus)
        });
        let bounds = if worn {
            Rect::from_origin_size(tip, (0.0, 0.0))
        } else {
            let (bounds, tag) = super::render::draw_agent_nametag(
                paint.scene,
                paint.transform,
                paint.text,
                paint.theme,
                (tip, viewport),
                &place.ink,
                &words,
                actor_color,
            );
            if let Some(user) = shown {
                self.note_caret_tag(super::CaretTag {
                    user,
                    label: words,
                    rect: paint.transform.transform_rect_bbox(tag),
                });
            }
            bounds
        };
        let marker = AgentActionMarker {
            invocation: interaction.invocation,
            tip,
            target,
            bounds,
            approach,
            pulse,
        };
        self.interaction_markers.insert(
            actor,
            AgentMarkerContinuity {
                invocation: interaction.invocation,
                surface: surface.to_owned(),
                origin,
                last_tip: tip,
                painted_at: now,
            },
        );
        Some(marker)
    }

    /// Concise live-region copy for a semantic action phase. `Locating` is
    /// intentionally silent: it is often immediately followed by `Applying`,
    /// and narrating both turns a fast action into accessibility chatter.
    pub fn interaction_announcement(&self, interaction: &SemanticInteraction) -> Option<String> {
        if matches!(interaction.phase, InteractionPhase::Locating) {
            return None;
        }
        let actor = actor_label(self, interaction);
        let state = match &interaction.phase {
            InteractionPhase::Waiting {
                reason: Some(reason),
            } => format!("Waiting: {reason}"),
            InteractionPhase::Failed { message, .. } => format!("Failed: {message}"),
            InteractionPhase::Cancelled {
                message: Some(message),
            } => format!("Cancelled: {message}"),
            phase => phase_label(phase),
        };
        Some(format!(
            "{actor}, {state}: {} on {}",
            interaction.action.label, interaction.resource.label
        ))
    }

    /// An exact-locus label keeps the lifecycle visible when the generic rail
    /// is suppressed. Failure must not look like a successful pointer action.
    ///
    /// Drawn on the item it acted on (a row), the label says what it did there ("Helper: moved"):
    /// the row already names the item, by its name now, where the step's own line would repeat
    /// the name the item had when the step began.
    pub fn interaction_status_label(&self, interaction: &SemanticInteraction) -> String {
        if matches!(
            interaction.anchor,
            tilo_actions::InteractionAnchor::ListItems { .. }
        ) || written_under(interaction).is_some()
        {
            return self.interaction_status_label_written(interaction);
        }
        self.interaction_status_label_doing(interaction, &step_line(interaction))
    }

    /// [`Self::interaction_status_label_doing`] for the characters a write put
    /// in a text: while they stand there, the chip says what the action did in
    /// the past tense — the words its app declares ("renamed", "replaced"),
    /// else the kind of step it took ("wrote") — never the gerund of the
    /// action's menu label, which names the place it acts on and reads as a
    /// replacement even where it only inserted. A phase only a person can
    /// settle — waiting, failed, cancelled — keeps the action's own words,
    /// because then no text of its stands to name.
    pub fn interaction_status_label_written(&self, interaction: &SemanticInteraction) -> String {
        let doing = match interaction.phase {
            InteractionPhase::Waiting { .. }
            | InteractionPhase::Failed { .. }
            | InteractionPhase::Cancelled { .. } => step_line(interaction),
            _ => written_word(interaction),
        };
        self.interaction_status_label_doing(interaction, &doing)
    }

    /// [`Self::interaction_status_label`] where the place says what the action
    /// is `doing` there better than its own label does: a field names what is
    /// edited in it.
    pub fn interaction_status_label_doing(
        &self,
        interaction: &SemanticInteraction,
        doing: &str,
    ) -> String {
        let actor = actor_label(self, interaction);
        // The phase shows through the mark's fade; only a phase that needs a
        // person — waiting, failed, cancelled — says so in words, because a
        // failure must not read as an ordinary mark.
        let doing = doing.to_owned();
        match &interaction.phase {
            InteractionPhase::Waiting { .. }
            | InteractionPhase::Failed { .. }
            | InteractionPhase::Cancelled { .. } => crate::i18n::widget_format(
                "interaction-action-status",
                &[
                    ("name", actor.into()),
                    ("doing", doing.into()),
                    ("phase", phase_label(&interaction.phase).into()),
                ],
            ),
            _ => crate::i18n::widget_format(
                "interaction-action-doing",
                &[("name", actor.into()), ("doing", doing.into())],
            ),
        }
    }

    fn newest_agent_entry(
        &self,
        actor: &tilo_actions::Actor,
        now: Instant,
    ) -> Option<&InteractionPresenceEntry> {
        self.interactions
            .values()
            .filter(|entry| {
                entry.visible_at(now)
                    && !entry.interaction.actor.is_person()
                    && entry.interaction.actor.by == *actor
            })
            .max_by(|left, right| {
                left.first_received_at
                    .cmp(&right.first_received_at)
                    .then_with(|| {
                        left.interaction
                            .invocation
                            .0
                            .as_bytes()
                            .cmp(right.interaction.invocation.0.as_bytes())
                    })
            })
    }

    fn visible_entries(&self, now: Instant) -> Vec<&InteractionPresenceEntry> {
        let mut entries = self
            .interactions
            .values()
            .filter(|entry| entry.visible_at(now))
            .collect::<Vec<_>>();
        sort_entries(&mut entries);
        entries
    }
}

impl InteractionPresenceEntry {
    /// Whether it shows at `now`: begun, still within its dwell, and not
    /// taken over by a later action of its agent.
    fn visible_at(&self, now: Instant) -> bool {
        !self.superseded && now >= self.first_received_at && self.alive_at(now)
    }

    /// Whether it is still remembered at `now`: within its dwell, shown or
    /// not.
    fn alive_at(&self, now: Instant) -> bool {
        now <= self.expiry_deadline()
    }

    /// Its dwell after the first frame that drew it, or, while none has, the longest a phase
    /// waits after it arrived.
    fn expiry_deadline(&self) -> Instant {
        let dwell = if self.interaction.phase.is_terminal() {
            TERMINAL_INTERACTION_DWELL
        } else {
            ACTIVE_INTERACTION_EXPIRY
        };
        let (from, dwell) = match self.drawn_at.get() {
            Some(drawn) => (drawn, dwell),
            None => (self.received_at, dwell.max(ACTIVE_INTERACTION_EXPIRY)),
        };
        from.checked_add(dwell).unwrap_or(from)
    }

    /// The discrete wakes this entry asks a host for: when its cursor starts
    /// moving on it, and when its dwell ends.
    fn next_wakes(&self, now: Instant) -> [Option<Instant>; 2] {
        // A cursor waiting for its rest on the last target to end moves on
        // then.
        let beginning =
            (now < self.first_received_at && !self.superseded).then_some(self.first_received_at);
        let expiry = self.expiry_deadline();
        [
            beginning,
            Some(expiry.checked_add(EXPIRY_EPSILON).unwrap_or(expiry)),
        ]
    }

    fn animating_at(&self, now: Instant, theme: &Theme) -> bool {
        let motion = theme.motion();
        let source = crate::tokens::MotionSource::Pointer;
        let approach = crate::motion::Follow::APPROACH.settle_time(
            crate::motion::Direction::Enter,
            motion,
            source,
        );
        let flash = crate::motion::Attention::FLASH.settle_time(
            crate::motion::Direction::Exit,
            motion,
            source,
        );
        now.saturating_duration_since(self.first_received_at) < approach
            || self.pulsed_at.is_some_and(|at| {
                now.saturating_duration_since(at.max(self.first_received_at)) < flash
            })
    }
}

/// Whether `interaction` wrote characters into a text: the mark it leaves is its text range, not a
/// place a cursor travels to.
fn writes_text(interaction: &SemanticInteraction) -> bool {
    matches!(
        interaction.anchor,
        tilo_actions::InteractionAnchor::Text { .. }
    )
}

/// The strength of the wash over a control an agent would press: the outline's own
/// ([`super::render::OUTLINE_FILL`]) at rest, raised by [`LOCUS_FLASH`] the moment its step was
/// taken and fading over the arrival flash. A person who asked for less motion keeps the steady
/// wash and never sees the flash.
fn locus_tint(pulsed_at: Option<Instant>, now: Instant, theme: &Theme) -> f32 {
    let flash = pulsed_at
        .filter(|_| !theme.reduce_motion)
        .map_or(0.0, |pulsed_at| {
            1.0 - crate::motion::Attention::FLASH.progress_after(
                crate::motion::Direction::Exit,
                theme.motion(),
                crate::tokens::MotionSource::Pointer,
                now.saturating_duration_since(pulsed_at),
            )
        });
    super::render::OUTLINE_FILL + LOCUS_FLASH * flash as f32
}

fn marker_entry_is_newer(
    candidate: &InteractionPresenceEntry,
    current: &InteractionPresenceEntry,
) -> bool {
    candidate.first_received_at > current.first_received_at
        || (candidate.first_received_at == current.first_received_at
            && candidate.interaction.invocation.0.as_bytes()
                > current.interaction.invocation.0.as_bytes())
}

/// Where an agent's mark rests by the `control` it would press in its place: level with it, just
/// outside it on the side facing the nearer edge of its `place`, where a row's words never are (a
/// leading checkbox's left, a trailing switch's right), else on the other side. A side whose mark
/// would cover `ink` (a neighbouring button, a line of words), or that leaves the frame, is passed
/// over. Only where neither side is clear does the mark take the first that fits the frame, or the
/// control's inside right edge.
fn beside(control: Rect, place: &MarkerPlace, viewport: Rect) -> Point {
    // The mark's ring and halo reach this far from its centre, and a gap keeps it off the outline.
    let radius = super::render::RING_R + 3.0;
    let reach = radius + 2.0;
    let (left, right) = (control.x0 - reach, control.x1 + reach);
    let sides = if control.center().x < place.locus.center().x - f64::EPSILON {
        [left, right]
    } else {
        [right, left]
    };
    let y = control.center().y;
    let framed = |x: f64| x - radius >= viewport.x0 && x + radius <= viewport.x1;
    let clear = |x: f64| {
        let ring = Rect::new(x - radius, y - radius, x + radius, y + radius);
        // What holds the control (its row) is where it stands, not words beside it.
        !place
            .ink
            .iter()
            .any(|ink| !ink.contains_rect(control) && super::caret_tag::overlaps(*ink, ring))
    };
    let x = sides
        .into_iter()
        .find(|x| framed(*x) && clear(*x))
        .or_else(|| sides.into_iter().find(|x| framed(*x)))
        .unwrap_or(control.x1 - reach);
    Point::new(x, y)
}

fn valid_marker_rect(rect: Rect) -> bool {
    [rect.x0, rect.y0, rect.x1, rect.y1]
        .into_iter()
        .all(f64::is_finite)
        && rect.width() > 0.0
        && rect.height() > 0.0
}

fn point_in_rect(rect: Rect, point: Point) -> bool {
    point.x >= rect.x0 && point.x <= rect.x1 && point.y >= rect.y0 && point.y <= rect.y1
}

fn viewport_diagonal(viewport: Rect) -> f64 {
    viewport.width().hypot(viewport.height())
}

fn nearest_viewport_edge(viewport: Rect, target: Point) -> Point {
    let clamped_x = target.x.clamp(viewport.x0, viewport.x1);
    let clamped_y = target.y.clamp(viewport.y0, viewport.y1);
    let candidates = [
        Point::new(viewport.x0, clamped_y),
        Point::new(viewport.x1, clamped_y),
        Point::new(clamped_x, viewport.y0),
        Point::new(clamped_x, viewport.y1),
    ];
    candidates
        .into_iter()
        .min_by(|left, right| {
            squared_distance(*left, target).total_cmp(&squared_distance(*right, target))
        })
        .unwrap_or(target)
}

fn squared_distance(left: Point, right: Point) -> f64 {
    (left.x - right.x).powi(2) + (left.y - right.y).powi(2)
}

fn point_lerp(from: Point, to: Point, progress: f64) -> Point {
    Point::new(
        from.x + (to.x - from.x) * progress,
        from.y + (to.y - from.y) * progress,
    )
}

fn sort_entries(entries: &mut [&InteractionPresenceEntry]) {
    entries.compact_sort_by(|left, right| {
        phase_priority(&right.interaction.phase)
            .cmp(&phase_priority(&left.interaction.phase))
            .then_with(|| right.received_at.cmp(&left.received_at))
            .then_with(|| {
                left.interaction
                    .invocation
                    .0
                    .as_bytes()
                    .cmp(right.interaction.invocation.0.as_bytes())
            })
    });
}

const fn phase_priority(phase: &InteractionPhase) -> u8 {
    match phase {
        InteractionPhase::Waiting { .. } | InteractionPhase::Failed { .. } => 3,
        InteractionPhase::Applying | InteractionPhase::Progress { .. } => 2,
        InteractionPhase::Locating => 1,
        InteractionPhase::Committed | InteractionPhase::Cancelled { .. } => 0,
    }
}

/// What an actor is doing, in the window's language: the kind of step its
/// action declares, on the record or item it acts in — "Writing in Trip plan",
/// "Schreibt in Trip plan". One line for every surface, so a mark reads as
/// something happening now rather than as a state printed beside a name.
fn step_line(interaction: &SemanticInteraction) -> String {
    crate::step::line(&tilo_actions::Step::of_interaction(interaction))
}

/// What the characters a write left say about it, in the window's language:
/// the past-tense words its action declares ("renamed", "replaced"), else the
/// kind of step it took ("wrote"), on no record because the characters stand
/// where it landed.
fn written_word(interaction: &SemanticInteraction) -> String {
    interaction.action.did.clone().unwrap_or_else(|| {
        crate::i18n::widget_format(
            tilo_actions::step::kind(interaction.action.step, &interaction.anchor).did_message(),
            &[],
        )
    })
}

fn phase_label(phase: &InteractionPhase) -> String {
    match phase {
        InteractionPhase::Locating => crate::i18n::widget_format("interaction-inspecting", &[]),
        InteractionPhase::Applying => crate::i18n::widget_format("interaction-applying", &[]),
        InteractionPhase::Progress {
            label,
            completed,
            total,
        } => match (completed, total) {
            (Some(done), Some(total)) if *total > 0 => crate::i18n::widget_format(
                "interaction-progress",
                &[
                    ("label", label.clone().into()),
                    ("done", done.to_string().into()),
                    ("total", total.to_string().into()),
                ],
            ),
            _ => label.clone(),
        },
        InteractionPhase::Waiting { .. } => crate::i18n::widget_format("interaction-waiting", &[]),
        InteractionPhase::Committed => crate::i18n::widget_format("interaction-committed", &[]),
        InteractionPhase::Failed { .. } => crate::i18n::widget_format("interaction-failed", &[]),
        InteractionPhase::Cancelled { .. } => {
            crate::i18n::widget_format("interaction-cancelled", &[])
        }
    }
}

/// The item a step acts on, where its resource is one (`<kind>/<field>/<item>`): what a row
/// showing that item names. A step that made an item names it once it landed.
pub(crate) fn acted_item(interaction: &SemanticInteraction) -> Option<String> {
    let id = tilo_actions::logical_resource_id(&interaction.resource.id);
    tilo_actions::stable_item_address(&id)?;
    id.rsplit_once('/').map(|(_, item)| item.to_owned())
}

/// The items `interaction`'s anchor names: the rows it acts on, or the items whose text it
/// wrote (an issue, for its title or a comment under it, `<issue>/<comment>`).
fn anchored_items(interaction: &SemanticInteraction) -> Vec<&str> {
    match &interaction.anchor {
        tilo_actions::InteractionAnchor::ListItems { ids } => {
            ids.iter().map(String::as_str).collect()
        }
        tilo_actions::InteractionAnchor::Text { ranges, .. } => ranges
            .iter()
            .filter_map(|range| range.block.split('/').next())
            .filter(|item| !item.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether `update` names what `current` named: the same resource, or the item a step on the
/// resource made there, which it names once it landed. Its label is the app's to say
/// ([`PresenceCtx::relabel_items`]), so it is no part of what the step acts on.
pub(super) fn same_or_made_in(
    (id, kind): (&str, &str),
    update: &tilo_actions::ActionResource,
) -> bool {
    let made = tilo_actions::stable_item_address(&tilo_actions::logical_resource_id(&update.id));
    (id == update.id && kind == update.kind)
        || (made.is_some_and(|made| {
            made.rsplit_once(':')
                .is_some_and(|(made, _)| made == update.kind)
        }) && update
            .kind
            .strip_prefix(kind)
            .is_some_and(|field| field.starts_with('.')))
}

/// What the roster calls the actor of `interaction`, by the name it holds for them; an automation by the group it runs in. A roster that knows a
/// member only by its key gives no name: a raw id is none, so it says what kind of member acts.
fn actor_label(presence: &PresenceCtx, interaction: &SemanticInteraction) -> String {
    // An agent that named its session is called that: the roster's name for an agent working
    // through a key is the key's device label, which is no name to point at a step with.
    if interaction.actor.is_agent()
        && let Some(named) = interaction
            .group
            .as_ref()
            .and_then(|group| group.session.as_deref())
            .map(str::trim)
            .filter(|named| !named.is_empty())
    {
        return named.to_owned();
    }
    let Some(member) = interaction.actor.member_id() else {
        let name = interaction
            .group
            .as_ref()
            .and_then(|group| group.label.clone());
        return crate::i18n::widget_format(
            "member-shortcut",
            &[("name", name.unwrap_or_default().into())],
        );
    };
    let named = presence.names.get(member);
    match named.and_then(|name| super::human_label(name, member.id())) {
        Some(name) => name.to_owned(),
        None if member.is_person() => crate::i18n::widget_format("interaction-member", &[]),
        None => crate::i18n::widget_format("interaction-agent", &[]),
    }
}

#[cfg(test)]
#[cfg(feature = "model")]
pub(crate) mod tests {
    use super::*;
    use crate::text_render::TextRenderer;
    use tilo_actions::{
        Acting, ActionResource, INTERACTION_VERSION, InteractionAction, InteractionAnchor, StepKind,
    };
    use uuid::Uuid;
    use vello::{Scene, kurbo::Affine};

    pub(crate) fn interaction(
        invocation: u128,
        sequence: u64,
        phase: InteractionPhase,
    ) -> SemanticInteraction {
        SemanticInteraction {
            version: INTERACTION_VERSION,
            invocation: InvocationId(Uuid::from_u128(invocation)),
            sequence,
            actor: Acting::agent("quill", "ada"),
            phase,
            action: InteractionAction {
                id: "rename".into(),
                label: "Rename".into(),
                app: None,
                step: None,
                creates: None,
                did: None,
            },
            resource: ActionResource {
                id: "file:draft".into(),
                kind: "files.entry".into(),
                label: "Draft.md".into(),
            },
            anchor: tilo_actions::InteractionAnchor::Resource,
            group: None,
        }
    }

    fn paint_marker(
        presence: &mut PresenceCtx,
        interaction: &SemanticInteraction,
        locus: Rect,
        viewport: Rect,
        surface: &str,
        now: Instant,
        theme: &Theme,
    ) -> Option<AgentActionMarker> {
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        presence.draw_agent_action_marker(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, theme),
            interaction,
            &MarkerPlace {
                locus,
                control: locus,
                ink: Vec::new(),
            },
            viewport,
            surface,
            now,
        )
    }

    fn on(target: &str, invocation: u128, phase: InteractionPhase) -> SemanticInteraction {
        let mut action = interaction(invocation, 1, phase);
        action.resource.id = target.into();
        action
    }

    /// A step that made a note names it once it landed, by the title its app gives it now.
    #[test]
    fn a_step_on_an_item_reads_by_the_items_name_now() {
        let (mut presence, at) = (PresenceCtx::empty(), crate::time::now());
        let mut made = interaction(7, 1, InteractionPhase::Applying);
        (made.resource.id, made.resource.kind) = ("doc:notebook".into(), "notes.pages".into());
        presence.apply_interaction_at(made.clone(), at).unwrap();
        (made.sequence, made.phase) = (2, InteractionPhase::Committed);
        (made.resource.id, made.resource.kind) =
            ("notes.pages/pages/p2".into(), "notes.pages.pages".into());
        assert!(presence.apply_interaction_at(made.clone(), at).unwrap());
        for title in ["Packing list", "Packing for Monday"] {
            presence.relabel_items(&|id| (id == "notes.pages/pages/p2").then(|| title.into()));
            assert!(step_line(&presence.visible_interactions(at)[0]).ends_with(title));
        }
        // An item of another resource is no landing of this step.
        (made.sequence, made.resource.id) = (3, "tasks.tracker/issues/p2".into());
        assert!(!presence.apply_interaction_at(made, at).unwrap());
    }

    #[test]
    fn an_agents_mark_rests_beside_what_it_would_press_never_on_words() {
        let viewport = Rect::new(0.0, 0.0, 960.0, 600.0);
        let ring = |tip: Point| {
            let r = super::super::render::RING_R + 3.0;
            Rect::new(tip.x - r, tip.y - r, tip.x + r, tip.y + r)
        };
        // A row: its checkbox leads, its words follow it.
        let row = Rect::new(36.0, 80.0, 924.0, 116.0);
        let checkbox = Rect::new(44.0, 90.0, 60.0, 106.0);
        let words = Rect::new(72.0, 88.0, 240.0, 108.0);
        let place = MarkerPlace {
            locus: row,
            control: checkbox,
            ink: vec![row, checkbox],
        };
        let tip = beside(checkbox, &place, viewport);
        assert!(
            tip.x < checkbox.x0,
            "it points at the checkbox from outside the row's words"
        );
        assert!(!super::super::caret_tag::overlaps(ring(tip), words));
        // A field with a button beside it: the mark leaves the button's words alone.
        let field = Rect::new(36.0, 24.0, 808.0, 56.0);
        let button = Rect::new(817.0, 24.0, 924.0, 56.0);
        let place = MarkerPlace {
            locus: field,
            control: field,
            ink: vec![field, button],
        };
        let tip = beside(field, &place, viewport);
        assert!(
            !super::super::caret_tag::overlaps(ring(tip), button),
            "{tip:?}"
        );
        // A lone button keeps its mark just past its right edge.
        let place = MarkerPlace {
            locus: button,
            control: button,
            ink: vec![button],
        };
        assert!(beside(button, &place, viewport).x > button.x1);
    }

    tilo_test_env::scenario! {
        fn agent_action_markers_move_truthfully_and_at_a_followable_pace;

        phase agent_action_markers_are_truthful_bounded_paint_only_motion;
        {
            let at = crate::time::now();
            let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
            let locus = Rect::new(560.0, 280.0, 640.0, 320.0);
            let theme = Theme::light();
            let mut presence = PresenceCtx::empty();
            let locating = interaction(40, 1, InteractionPhase::Locating);
            presence.apply_interaction_at(locating.clone(), at).unwrap();

            // phase: a_first_action_enters_from_the_nearest_viewport_edge
            let first = paint_marker(
                &mut presence,
                &locating,
                locus,
                viewport,
                "writer:1",
                at,
                &theme,
            )
            .unwrap();
            assert_eq!(first.tip, Point::new(viewport.x1, 300.0));
            assert_eq!(
                first.target,
                Point::new(653.5, 300.0),
                "the mark rests beside what it would press, never over it"
            );
            assert!(!locus.contains(first.target));
            assert_eq!(first.approach, 0.0);
            assert!(first.pulse > 0.99);

            // phase: a_phase_update_keeps_the_invocations_first_observation
            let applying_at = at + Duration::from_millis(300);
            let applying = interaction(40, 2, InteractionPhase::Applying);
            presence
                .apply_interaction_at(applying.clone(), applying_at)
                .unwrap();
            let advanced = paint_marker(
                &mut presence,
                &applying,
                locus,
                viewport,
                "writer:1",
                applying_at,
                &theme,
            )
            .unwrap();
            assert!(
                advanced.approach > 0.9 && advanced.approach < 1.0,
                "still on its way from the first observation, not restarted: {}",
                advanced.approach
            );
            assert!(advanced.tip.x < first.tip.x);
            assert!(
                advanced.pulse < first.pulse,
                "a phase in the same burst carries on the one flash, never restarts it: {}",
                advanced.pulse
            );

            // phase: motion_stops_after_the_bounded_approach_and_pulse
            assert!(presence.interaction_animation_demand(applying_at, &theme));
            let settled_at = at + Duration::from_millis(611);
            let settled = paint_marker(
                &mut presence,
                &applying,
                locus,
                viewport,
                "writer:1",
                settled_at,
                &theme,
            )
            .unwrap();
            assert_eq!(settled.tip, Point::new(653.5, 300.0));
            assert_eq!(settled.approach, 1.0);
            assert_eq!(settled.pulse, 0.0);
            assert!(!presence.interaction_animation_demand(settled_at, &theme));
            assert!(presence.pointers.is_empty());
            assert!(presence.carets.is_empty());
            assert!(presence.selections.is_empty());
            assert!(presence.remote_cursors.is_empty());
        }

        phase agent_action_markers_choose_one_newest_action_and_preserve_recent_continuity;
        {
            let at = crate::time::now();
            let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
            let first_locus = Rect::new(80.0, 80.0, 160.0, 120.0);
            let second_locus = Rect::new(400.0, 300.0, 500.0, 360.0);
            let theme = Theme::light();
            let mut presence = PresenceCtx::empty();
            let first = interaction(50, 1, InteractionPhase::Applying);
            presence.apply_interaction_at(first.clone(), at).unwrap();
            let first_settled_at = at + Duration::from_millis(611);
            let first_frame = paint_marker(
                &mut presence,
                &first,
                first_locus,
                viewport,
                "writer:1",
                first_settled_at,
                &theme,
            )
            .unwrap();

            let second_at = at + Duration::from_millis(700);
            let second = interaction(51, 1, InteractionPhase::Locating);
            presence
                .apply_interaction_at(second.clone(), second_at)
                .unwrap();
            assert_eq!(
                presence.agent_marker_interactions(second_at),
                vec![second.clone()]
            );
            assert!(
                paint_marker(
                    &mut presence,
                    &first,
                    first_locus,
                    viewport,
                    "writer:1",
                    second_at,
                    &theme,
                )
                .is_none(),
                "an older invocation cannot paint a second marker for the actor"
            );
            let second_frame = paint_marker(
                &mut presence,
                &second,
                second_locus,
                viewport,
                "writer:1",
                second_at,
                &theme,
            )
            .unwrap();
            assert_eq!(second_frame.tip, first_frame.tip);

            // phase: reduced_motion_snaps_to_the_locus_without_pulsing
            let reduced = theme.clone().with_reduce_motion(true);
            let reduced_frame = paint_marker(
                &mut presence,
                &second,
                second_locus,
                viewport,
                "writer:1",
                second_at,
                &reduced,
            )
            .unwrap();
            assert_eq!(reduced_frame.tip, Point::new(513.5, 330.0));
            assert_eq!(reduced_frame.pulse, 0.0);
            assert!(!presence.interaction_animation_demand(second_at, &reduced));
        }

        phase agent_marker_continuity_never_crosses_surface_viewport_or_long_travel_bounds;
        {
            let at = crate::time::now();
            let theme = Theme::light();
            let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);

            // phase: the_same_invocation_reenters_from_an_edge_on_another_surface
            {
                let mut presence = PresenceCtx::empty();
                let action = interaction(70, 1, InteractionPhase::Locating);
                presence.apply_interaction_at(action.clone(), at).unwrap();
                let _ = paint_marker(
                    &mut presence,
                    &action,
                    Rect::new(80.0, 280.0, 160.0, 320.0),
                    viewport,
                    "writer:1",
                    at,
                    &theme,
                )
                .unwrap();
                let moved = paint_marker(
                    &mut presence,
                    &action,
                    Rect::new(640.0, 280.0, 720.0, 320.0),
                    viewport,
                    "writer:2",
                    at + Duration::from_millis(100),
                    &theme,
                )
                .unwrap();
                assert!(
                    moved.tip.x > moved.target.x,
                    "a cross-surface marker must approach from that surface's nearest edge"
                );
                assert!(presence.pointers.is_empty());
            }

            // phase: a_compact_resize_discards_an_origin_outside_the_new_viewport
            {
                let mut presence = PresenceCtx::empty();
                let action = interaction(71, 1, InteractionPhase::Locating);
                presence.apply_interaction_at(action.clone(), at).unwrap();
                let _ = paint_marker(
                    &mut presence,
                    &action,
                    Rect::new(640.0, 280.0, 720.0, 320.0),
                    viewport,
                    "writer:1",
                    at,
                    &theme,
                )
                .unwrap();
                let compact = Rect::new(0.0, 0.0, 320.0, 240.0);
                let resized = paint_marker(
                    &mut presence,
                    &action,
                    Rect::new(20.0, 90.0, 60.0, 130.0),
                    compact,
                    "writer:1",
                    at + Duration::from_millis(100),
                    &theme,
                )
                .unwrap();
                assert!(
                    resized.tip.x < resized.target.x,
                    "the compact viewport supplies a fresh nearest-edge origin"
                );
                assert!(point_in_rect(compact, resized.tip));
                assert!(presence.carets.is_empty());
                assert!(presence.selections.is_empty());
            }

            // phase: a_recent_but_far_new_invocation_uses_the_nearest_edge
            {
                let mut presence = PresenceCtx::empty();
                let first = interaction(72, 1, InteractionPhase::Applying);
                presence.apply_interaction_at(first.clone(), at).unwrap();
                let _ = paint_marker(
                    &mut presence,
                    &first,
                    Rect::new(60.0, 80.0, 140.0, 120.0),
                    viewport,
                    "writer:1",
                    at + Duration::from_millis(611),
                    &theme,
                )
                .unwrap();
                let next_at = at + Duration::from_millis(700);
                let next = interaction(73, 1, InteractionPhase::Locating);
                presence
                    .apply_interaction_at(next.clone(), next_at)
                    .unwrap();
                let far = paint_marker(
                    &mut presence,
                    &next,
                    Rect::new(660.0, 430.0, 740.0, 470.0),
                    viewport,
                    "writer:1",
                    next_at,
                    &theme,
                )
                .unwrap();
                assert_eq!(far.tip, Point::new(viewport.x1, far.target.y));
                assert!(presence.remote_cursors.is_empty());
            }
        }

        phase a_fast_agents_actions_are_paced_so_a_person_can_follow_them;
        // A fast agent never strobes: a burst on one target is one action at
        // rest there, a new target waits until the cursor rested on the last
        // one, and only the newest of the targets that waited is gone to.
        {
            let at = crate::time::now();
            let mut presence = PresenceCtx::empty();

            // phase: a_burst_on_one_target_is_one_action
            for n in 0..8u128 {
                let when = at + Duration::from_millis(40 * n as u64);
                presence
                    .apply_interaction_at(on("file:draft", 100 + n, InteractionPhase::Committed), when)
                    .unwrap();
            }
            let burst_end = at + Duration::from_millis(280);
            let shown = presence.visible_interactions(burst_end);
            assert_eq!(shown.len(), 1, "eight edits read as one: {shown:?}");
            assert_eq!(shown[0].invocation, InvocationId(Uuid::from_u128(107)));
            let pulses: std::collections::HashSet<_> = presence
                .interactions
                .values()
                .filter_map(|entry| entry.pulsed_at)
                .collect();
            assert_eq!(pulses, [at].into(), "the burst flashes the cursor once");

            // phase: a_new_target_waits_for_the_rest_on_the_last_one
            let elsewhere_at = at + Duration::from_millis(300);
            presence
                .apply_interaction_at(
                    on("file:notes", 200, InteractionPhase::Applying),
                    elsewhere_at,
                )
                .unwrap();
            let waiting = presence.agent_marker_interactions(elsewhere_at);
            assert_eq!(waiting.len(), 1);
            assert_eq!(
                waiting[0].resource.id, "file:draft",
                "the cursor stays where it rests"
            );
            // The rest counts from the burst's last action there.
            let rest_ends = burst_end + AGENT_TARGET_DWELL;
            assert_eq!(
                presence.interaction_next_wake_deadline(elsewhere_at),
                Some(rest_ends),
                "the frame that moves it on is asked for"
            );

            // phase: only_the_newest_waiting_target_is_gone_to
            presence
                .apply_interaction_at(
                    on("file:todo", 201, InteractionPhase::Applying),
                    elsewhere_at + Duration::from_millis(10),
                )
                .unwrap();
            presence
                .apply_interaction_at(
                    {
                        let mut later = on("file:notes", 200, InteractionPhase::Committed);
                        later.sequence = 2;
                        later
                    },
                    rest_ends,
                )
                .unwrap();
            let moved = presence.agent_marker_interactions(rest_ends);
            assert_eq!(moved.len(), 1);
            assert_eq!(
                moved[0].resource.id, "file:todo",
                "the skipped target never shows"
            );
            assert!(
                presence
                    .visible_interactions(rest_ends)
                    .iter()
                    .all(|shown| shown.resource.id != "file:notes"),
                "a later phase of a skipped action does not bring it back"
            );
        }
    }

    tilo_test_env::scenario! {
        fn interactions_update_expire_and_paint_only_where_addressed;

        phase interaction_updates_are_monotonic_bound_to_their_actor_and_expire;
        {
            // phase: invocation_updates_are_monotonic_and_terminal_is_final
            {
                let at = crate::time::now();
                let mut presence = PresenceCtx::empty();
                assert!(
                    presence
                        .apply_interaction_at(interaction(1, 2, InteractionPhase::Applying), at)
                        .unwrap()
                );
                assert!(
                    !presence
                        .apply_interaction_at(interaction(1, 1, InteractionPhase::Locating), at)
                        .unwrap()
                );
                assert!(
                    presence
                        .apply_interaction_at(interaction(1, 3, InteractionPhase::Committed), at)
                        .unwrap()
                );
                assert!(
                    !presence
                        .apply_interaction_at(interaction(1, 4, InteractionPhase::Applying), at)
                        .unwrap()
                );
                assert_eq!(presence.visible_interactions(at)[0].sequence, 3);
            }
            // phase: an_invocation_cannot_substitute_its_actor_or_target
            {
                let at = crate::time::now();
                let mut presence = PresenceCtx::empty();
                presence
                    .apply_interaction_at(interaction(2, 1, InteractionPhase::Applying), at)
                    .unwrap();
                let mut forged = interaction(2, 2, InteractionPhase::Committed);
                forged.actor = tilo_actions::Acting::person("someone-else");
                assert!(!presence.apply_interaction_at(forged, at).unwrap());
                let mut moved = interaction(2, 2, InteractionPhase::Committed);
                moved.resource.id = "file:secret".into();
                assert!(!presence.apply_interaction_at(moved, at).unwrap());
            }
            // phase: active_and_terminal_states_expire_on_their_own_dwell
            {
                let at = crate::time::now();
                let mut presence = PresenceCtx::empty();
                presence
                    .apply_interaction_at(interaction(3, 1, InteractionPhase::Applying), at)
                    .unwrap();
                assert!(presence.has_visible_interactions(at + ACTIVE_INTERACTION_EXPIRY));
                assert!(!presence.has_visible_interactions(
                    at + ACTIVE_INTERACTION_EXPIRY + Duration::from_millis(1)
                ));

                let mut terminal_presence = PresenceCtx::empty();
                let committed = interaction(4, 1, InteractionPhase::Committed);
                terminal_presence
                    .apply_interaction_at(committed.clone(), at)
                    .unwrap();
                terminal_presence.note_drawn(committed.invocation, at);
                assert!(terminal_presence.has_visible_interactions(at + TERMINAL_INTERACTION_DWELL));
                assert!(
                    terminal_presence
                        .prune_interactions(at + TERMINAL_INTERACTION_DWELL + Duration::from_millis(1))
                );
                assert!(!terminal_presence.has_visible_interactions(
                    at + TERMINAL_INTERACTION_DWELL + Duration::from_millis(1)
                ));
            }
        }

        phase interactions_paint_only_their_addressed_surface;
        {
            // phase: workspace_fanout_only_paints_the_addressed_app_surface
            {
                let at = crate::time::now();
                let update = interaction(10, 1, InteractionPhase::Applying);
                let key = crate::presence::PresenceKey::resource(
                    "org.tilo.Writer",
                    &update.resource.id,
                    &update.resource.kind,
                );
                let payload = crate::collaboration::PresencePayload::Interaction(Box::new(update));

                let mut files = PresenceCtx::empty();
                files.set_surface_id("org.tilo.Files");
                files.apply_remote_presence(MemberId::person("broker-agent"), key.clone(), payload.clone());
                assert!(!files.has_visible_interactions(at));

                let mut writer = PresenceCtx::empty();
                writer.set_surface_id("org.tilo.Writer");
                writer.apply_remote_presence(MemberId::person("broker-agent"), key, payload);
                assert!(writer.has_visible_interactions(crate::time::now()));
            }
            // phase: applying_an_agent_interaction_does_not_synthesize_input_presence
            {
                let at = crate::time::now();
                let mut presence = PresenceCtx::empty();
                presence
                    .apply_interaction_at(interaction(9, 1, InteractionPhase::Applying), at)
                    .unwrap();
                assert!(presence.pointers.is_empty());
                assert!(presence.carets.is_empty());
                assert!(presence.selections.is_empty());
                assert!(presence.remote_cursors.is_empty());
            }
        }

    }

    #[test]
    fn an_agents_later_step_on_an_item_takes_its_marks_place() {
        let at = crate::time::now();
        let mut presence = PresenceCtx::empty();
        let mut created = interaction(120, 2, InteractionPhase::Committed);
        created.action.id = "create".into();
        created.anchor = InteractionAnchor::ListItems {
            ids: vec!["7d2b".into()],
        };
        presence.apply_interaction_at(created, at).unwrap();
        let mut renamed = interaction(121, 1, InteractionPhase::Applying);
        presence.apply_interaction_at(renamed.clone(), at).unwrap();
        let shown = |presence: &PresenceCtx| -> Vec<u128> {
            presence
                .visible_interactions(at + Duration::from_secs(1))
                .iter()
                .map(|shown| shown.invocation.0.as_u128())
                .collect()
        };
        assert!(
            shown(&presence).contains(&120),
            "a step naming no item leaves it"
        );
        renamed.sequence = 2;
        renamed.phase = InteractionPhase::Committed;
        renamed.anchor = InteractionAnchor::Text {
            region: "title".into(),
            ranges: vec![tilo_actions::InteractionTextRange {
                block: "7d2b".into(),
                start: 0,
                end: 11,
            }],
        };
        presence.apply_interaction_at(renamed, at).unwrap();
        assert_eq!(shown(&presence), [121], "the item says it was renamed");
    }

    tilo_test_env::scenario! {
        fn a_write_shows_where_it_landed;

        phase a_write_shows_when_it_arrives_not_when_the_cursor_finishes_resting;
        // The characters a write put in a text show when the phase arrives: the
        // rest on the agent's last target still takes the targets in between off
        // the rail, but it is the cursor's rest, not the write's.
        {
            let at = crate::time::now();
            let mut presence = PresenceCtx::empty();
            // The agent's last action rests on another target.
            let mut resting = interaction(96, 1, InteractionPhase::Committed);
            resting.resource.id = "file:todo".into();
            resting.action.id = "tick".into();
            presence.apply_interaction_at(resting, at).unwrap();

            let mut write = interaction(97, 1, InteractionPhase::Applying);
            write.resource.id = "file:notes".into();
            write.action.id = "replace-range".into();
            write.anchor = tilo_actions::InteractionAnchor::Text {
                region: "body".into(),
                ranges: vec![tilo_actions::InteractionTextRange {
                    block: "b1".into(),
                    start: 33,
                    end: 66,
                }],
            };
            presence.apply_interaction_at(write.clone(), at).unwrap();
            assert!(
                presence
                    .visible_interactions(at)
                    .iter()
                    .any(|shown| shown.invocation == write.invocation),
                "the text it wrote is already there"
            );

            // A place the cursor has to travel to still waits for the rest.
            let mut card = interaction(98, 1, InteractionPhase::Applying);
            card.resource.id = "file:third".into();
            card.action.id = "open".into();
            presence.apply_interaction_at(card.clone(), at).unwrap();
            assert!(
                !presence
                    .visible_interactions(at)
                    .iter()
                    .any(|shown| shown.invocation == card.invocation),
                "the cursor rests before it moves on"
            );
            assert!(
                presence
                    .visible_interactions(at + AGENT_TARGET_DWELL)
                    .iter()
                    .any(|shown| shown.invocation == card.invocation)
            );

            // A write that names its text only once it applied (an append to a note) shows then.
            let mut landed = card.clone();
            landed.sequence = 2;
            landed.phase = InteractionPhase::Committed;
            landed.anchor = write.anchor.clone();
            presence.apply_interaction_at(landed, at).unwrap();
            assert!(
                presence
                    .visible_interactions(at)
                    .iter()
                    .any(|shown| shown.invocation == card.invocation),
                "the text it wrote is there once it landed"
            );
        }
    }

    tilo_test_env::scenario! {
        fn an_actor_and_its_work_are_named_and_coloured;

        phase an_actor_is_named_by_its_own_name_and_keeps_whom_it_acts_for;
        // An agent or a Shortcut acting for a person is named by its own name, keeps whom it
        // acts for, and gets the software mark and marker a person's activity never does.
        {
            let at = crate::time::now();
            let mut presence = PresenceCtx::empty();
            presence.set_display_name(MemberId::person("ada"), "Ada");
            presence.set_display_name(MemberId::agent("quill", "ada"), "Claude");
            let mut agent = interaction(30, 1, InteractionPhase::Applying);
            agent.actor.acted_for = vec![tilo_actions::Acting::person("ada")];
            assert!(presence.apply_interaction_at(agent.clone(), at).unwrap());
            let claude = MemberId::agent("quill", "ada");
            assert_eq!(presence.display_name(&claude), "Claude");
            assert_eq!(presence.acting_for(&claude), Some(&MemberId::person("ada")));
            assert!(!claude.is_person());
            assert_eq!(presence.agent_marker_interactions(at), vec![agent]);

            let mut shortcut = interaction(31, 1, InteractionPhase::Applying);
            shortcut.actor = Acting::automation("morning");
            shortcut.group = Some(tilo_actions::WorkGroup {
                id: "run-7".into(),
                label: Some("Morning".into()),
                kind: "shortcut".into(),
                declared_id: None,
                session: None,
            });
            assert_eq!(
                actor_label(&presence, &shortcut),
                "Morning (Shortcut)",
                "named by its run"
            );
            assert!(presence.apply_interaction_at(shortcut, at).unwrap());
            assert_eq!(presence.agent_marker_interactions(at).len(), 2);

            let mut person = interaction(32, 1, InteractionPhase::Applying);
            person.actor = Acting::person("bob");
            presence.set_display_name(MemberId::person("bob"), "Bob");
            assert!(presence.apply_interaction_at(person, at).unwrap());
            assert_eq!(presence.display_name(&MemberId::person("bob")), "Bob");
            assert_eq!(
                presence.agent_marker_interactions(at).len(),
                2,
                "a member's own action keeps the plain outline"
            );
        }

        phase an_agent_writing_at_its_caret_wears_one_tag;
        // An agent writing in a document wears one tag, at its caret: the caret's tag names it, and
        // the marker of the action it is taking there adds no second tag, whatever words that tag
        // would have said.
        {
            let at = crate::time::now();
            let theme = Theme::default();
            let mut presence = PresenceCtx::empty();
            presence.set_display_name(MemberId::agent("quill", "ada"), "Claude Code");
            let agent = interaction(40, 1, InteractionPhase::Applying);
            presence.apply_interaction_at(agent.clone(), at).unwrap();
            let claude = MemberId::agent("quill", "ada");
            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            presence.draw_peer_caret_labeled_in(
                &mut scene,
                &theme,
                &mut text,
                Affine::IDENTITY,
                &claude,
                "Claude Code",
                super::super::LabeledCaret::new(Rect::new(200.0, 300.0, 202.0, 318.0)),
            );
            let locus = Rect::new(100.0, 296.0, 400.0, 322.0);
            let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
            let marker = paint_marker(&mut presence, &agent, locus, viewport, "note", at, &theme)
                .expect("the action is marked");
            assert!(
                marker.bounds.is_zero_area(),
                "and no pointer of its own beside the caret: {:?}",
                marker.bounds
            );
            let tags: Vec<String> = presence
                .caret_tags()
                .into_iter()
                .filter(|tag| tag.user == claude)
                .map(|tag| tag.label)
                .collect();
            assert_eq!(
                tags,
                vec!["Claude Code".to_owned()],
                "one tag, at its caret"
            );
        }

        phase a_tag_elsewhere_leaves_the_would_click_place_labelled;
        // A tag of the agent's access elsewhere — at the caret in the field it just wrote — says
        // nothing about the control it would press next: that place keeps its own tag, or a person
        // seeing the step has only a thin outline to go by.
        {
            let at = crate::time::now();
            let theme = Theme::default();
            let mut presence = PresenceCtx::empty();
            presence.set_display_name(MemberId::agent("quill", "ada"), "Claude Code");
            let agent = interaction(41, 1, InteractionPhase::Applying);
            presence.apply_interaction_at(agent.clone(), at).unwrap();
            let claude = MemberId::agent("quill", "ada");
            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            // Its caret stands in another field, far from the button it acts on.
            presence.draw_peer_caret_labeled_in(
                &mut scene,
                &theme,
                &mut text,
                Affine::IDENTITY,
                &claude,
                "Claude Code",
                super::super::LabeledCaret::new(Rect::new(600.0, 80.0, 602.0, 98.0)),
            );
            let locus = Rect::new(100.0, 296.0, 400.0, 322.0);
            let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
            let marker = paint_marker(&mut presence, &agent, locus, viewport, "note", at, &theme)
                .expect("the action is marked");
            assert!(
                !marker.bounds.is_zero_area(),
                "the place it would press wears its tag: {:?}",
                marker.bounds
            );
            let tags: Vec<String> = presence
                .caret_tags()
                .into_iter()
                .filter(|tag| tag.user == claude)
                .map(|tag| tag.label)
                .collect();
            assert_eq!(tags.len(), 2, "one at its caret, one where it would press");
        }

        phase the_place_an_agent_would_press_is_washed_in_its_colour;
        // The place an agent would press is washed in its colour, so it reads at a glance: light at
        // rest, brighter the moment its step was taken, and steady for a person who asked for less
        // motion.
        {
            let at = crate::time::now();
            let theme = Theme::dark();
            let at_rest = crate::presence::render::OUTLINE_FILL;
            assert!((locus_tint(None, at, &theme) - at_rest).abs() < f32::EPSILON);
            assert!(
                (locus_tint(Some(at), at, &theme) - (at_rest + LOCUS_FLASH)).abs() < 0.01,
                "it flashes as the step is taken"
            );
            let settled = at
                + crate::motion::Attention::FLASH.settle_time(
                    crate::motion::Direction::Exit,
                    theme.motion(),
                    crate::tokens::MotionSource::Pointer,
                );
            assert!(
                (locus_tint(Some(at), settled, &theme) - at_rest).abs() < 0.01,
                "the flash is over, the wash stays"
            );
            let still = theme.with_reduce_motion(true);
            assert!(
                (locus_tint(Some(at), at, &still) - at_rest).abs() < f32::EPSILON,
                "no flash for a person who asked for less motion"
            );
        }

        phase a_step_reads_as_the_kind_of_work_it_declares;
        // A step says the kind of work its action declares, else the part of the
        // record it acts on: never an English verb read out of the label.
        {
            let mut writing = interaction(91, 1, InteractionPhase::Applying);
            writing.resource.label = "Trip plan".into();
            writing.anchor = InteractionAnchor::Text {
                region: "text".into(),
                ranges: Vec::new(),
            };
            assert_eq!(step_line(&writing), "Writing in Trip plan");

            let mut deleting = interaction(92, 1, InteractionPhase::Applying);
            deleting.action.step = Some(StepKind::Delete);
            assert_eq!(step_line(&deleting), "Deleting Draft.md");

            let unclassified = interaction(93, 1, InteractionPhase::Applying);
            assert_eq!(
                step_line(&unclassified),
                "Working in Draft.md",
                "a step whose declaration names no kind says where it works"
            );
        }

        phase interactions_are_labelled_and_narrated_as_actions;
        {
            // The exact-control badge names the action and lifecycle first, so
            // elision does not leave only an actor with an ambiguous "Applying".
            {
                let mut presence = PresenceCtx::empty();
                presence
                    .names
                    .insert(MemberId::agent("quill", "ada"), "Release Notes Agent".into());
                assert_eq!(
                    presence.interaction_status_label(&interaction(11, 1, InteractionPhase::Applying)),
                    "Release Notes Agent: Working in Draft.md"
                );
                // The pointer's tag names the agent alone: what it does is the card's line, and
                // a second sentence on the tag pushes a long one out of the window it points in.
                assert_eq!(
                    actor_label(&presence, &interaction(11, 1, InteractionPhase::Applying)),
                    "Release Notes Agent"
                );
                presence.names.clear();
                assert_eq!(
                    presence.interaction_status_label(&interaction(11, 1, InteractionPhase::Applying)),
                    "Agent: Working in Draft.md"
                );
            }
            // phase: agent_identity_is_textual_and_does_not_fall_back_to_a_raw_id
            {
                let user = MemberId::agent("opaque-runtime-id", "ada");
                let mut agent = interaction(13, 1, InteractionPhase::Applying);
                agent.actor = Acting::agent("opaque-runtime-id", "ada");
                let mut presence = PresenceCtx::empty();
                assert_eq!(actor_label(&presence, &agent), "Agent");
                presence.names.insert(user.clone(), "Quill".into());
                assert_eq!(actor_label(&presence, &agent), "Quill");
                // An agent that named its session is called that, whatever the roster calls the
                // key's device it works through: a key label and an address name no one.
                presence.names.insert(user.clone(), "API key \u{201c}Local stack\u{201d} via localhost:37710".into());
                let mut named = interaction(14, 1, InteractionPhase::Applying);
                named.actor = Acting::agent("opaque-runtime-id", "ada");
                named.group = Some(tilo_actions::WorkGroup {
                    id: "agent-session:1".into(),
                    label: None,
                    kind: "agent-session".into(),
                    declared_id: None,
                    session: Some("Office Hours".into()),
                });
                assert_eq!(actor_label(&presence, &named), "Office Hours");
            }
            // A roster that knows a member only by its key hands that key
            // back as its name; the card names the kind of member instead.
            {
                let key = "a4b05b334073f7dc3c31652f1b0143be8b7f5e0c2d9a1f6b3e4c5d6a7b8c9d0e";
                let user = MemberId::person(key);
                let mut presence = PresenceCtx::empty();
                presence.set_display_name(user.clone(), key);
                let mut keyed = interaction(12, 1, InteractionPhase::Committed);
                keyed.actor = Acting::person(key);
                assert_eq!(actor_label(&presence, &keyed), "Member");
                assert_eq!(
                    presence.interaction_status_label(&keyed),
                    "Member: Working in Draft.md"
                );
                let agent = MemberId::agent("quill", key);
                presence.set_display_name(agent.clone(), "Claude");
                presence.set_acting_for(agent.clone(), Some(user));
                assert_eq!(
                    presence.display_name(&agent),
                    "Claude",
                    "an unnamed person it acts for is left out, not shown as a key"
                );
            }
            // phase: accessibility_narrates_real_action_state_without_pointer_language
            {
                let mut presence = PresenceCtx::empty();
                presence
                    .names
                    .insert(MemberId::agent("quill", "ada"), "Quill".into());
                let applying = interaction(11, 1, InteractionPhase::Applying);
                assert_eq!(
                    presence.interaction_announcement(&applying).as_deref(),
                    Some("Quill, Applying: Rename on Draft.md")
                );
                assert!(
                    presence
                        .interaction_announcement(&interaction(11, 0, InteractionPhase::Locating))
                        .is_none(),
                    "the short locating edge is intentionally not narrated"
                );
                let failed = interaction(
                    11,
                    2,
                    InteractionPhase::Failed {
                        code: Some("conflict".into()),
                        message: "Draft changed".into(),
                    },
                );
                let announcement = presence
                    .interaction_announcement(&failed)
                    .expect("terminal action announcement");
                assert!(announcement.contains("Failed: Draft changed"));
                assert!(!announcement.to_lowercase().contains("cursor"));
                assert!(!announcement.to_lowercase().contains("click"));
            }
        }

        phase a_write_says_what_the_action_did;
        // The chip on the characters a write put in a note says what the action
        // did, not the name of the place the menu acts on: "Helper: renamed"
        // for a rename whose app declares the word, where "rename" alone names
        // an edit of the title and "replace text at a place" named replacement
        // even for an insertion. An action declaring none says the kind of step
        // it took: an insertion writes. A write that failed says the step
        // itself, since none of its text stands.
        {
            let mut presence = PresenceCtx::empty();
            presence
                .names
                .insert(MemberId::agent("helper", "ada"), "Helper".into());
            let mut renamed = interaction(94, 1, InteractionPhase::Applying);
            renamed.actor = Acting::agent("helper", "ada");
            renamed.action = InteractionAction {
                id: "rename".into(),
                label: "Rename".into(),
                app: None,
                step: Some(StepKind::Edit),
                creates: None,
                did: Some("renamed".into()),
            };
            renamed.anchor = tilo_actions::InteractionAnchor::Text {
                region: "title".into(),
                ranges: vec![tilo_actions::InteractionTextRange {
                    block: String::new(),
                    start: 0,
                    end: 13,
                }],
            };
            assert_eq!(
                presence.interaction_status_label_written(&renamed),
                "Helper: renamed"
            );

            let mut inserted = interaction(95, 1, InteractionPhase::Applying);
            inserted.actor = Acting::agent("helper", "ada");
            inserted.action = InteractionAction {
                id: "replace-range".into(),
                label: "Replace text at a place".into(),
                app: None,
                step: None,
                creates: None,
                did: None,
            };
            inserted.anchor = tilo_actions::InteractionAnchor::Text {
                region: "body".into(),
                ranges: vec![tilo_actions::InteractionTextRange {
                    block: "b1".into(),
                    start: 33,
                    end: 66,
                }],
            };
            assert_eq!(
                presence.interaction_status_label_written(&inserted),
                "Helper: wrote"
            );
            let mut failed = inserted.clone();
            failed.sequence = 2;
            failed.phase = InteractionPhase::Failed {
                code: None,
                message: "The note is read-only".into(),
            };
            let name = presence.interaction_status_label_written(&failed);
            assert!(
                name.starts_with("Helper: Writing in Draft.md. Failed"),
                "a failed write says the step, not text that never landed: {name}"
            );
        }
    }
}
