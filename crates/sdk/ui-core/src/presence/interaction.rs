//! Transient, semantic action presence.
//!
//! These updates describe an invocation's resource and lifecycle. They do not
//! write the pointer, caret, selection, or typing stores: resource actions do
//! not synthesize pointer input.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "interaction layout is bounded f64 scene geometry and checked Duration arithmetic"
)]

use super::{ParticipantKind, PresenceCtx};
use crate::ColorCompat;
use crate::paint::PaintCtx;
use crate::permissions::UserId;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use crate::tokens::TextRole;
use std::collections::HashMap;
use tilo_actions::{InteractionPhase, InvocationId, SemanticInteraction};
use tilo_sort::CompactSort as _;
use vello::{
    Scene,
    kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke},
    peniko::{Color, Fill},
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
const MAX_VISIBLE_CARDS: usize = 3;
const CARD_WIDTH: f64 = 300.0;
const CARD_HEIGHT: f64 = 52.0;
const CARD_GAP: f64 = 6.0;
const CARD_MARGIN: f64 = 12.0;

#[derive(Debug, Clone)]
pub(super) struct InteractionPresenceEntry {
    interaction: SemanticInteraction,
    /// When the invocation began to show: its arrival, or, for an agent
    /// whose cursor is still resting on its previous target, once that rest
    /// ends ([`AGENT_TARGET_DWELL`]). Phase restatements must not restart its
    /// approach from the edge or make an old invocation look newest.
    first_received_at: Instant,
    /// Arrival of this phase. Drives its visible dwell.
    received_at: Instant,
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

/// Geometry of one generic fallback card. Public so app hosts and tests can
/// reserve the same non-overlapping activity rail without copying constants.
#[derive(Debug, Clone, PartialEq)]
pub struct InteractionCardPlacement {
    pub invocation: InvocationId,
    pub rect: Rect,
    /// Count folded into this row instead of painting more cards over content.
    pub additional: usize,
}

impl PresenceCtx {
    pub(super) fn remove_interactions_for_actor(&mut self, actor_id: &str) {
        self.interactions
            .retain(|_, entry| entry.interaction.actor.id != actor_id);
        self.interaction_markers
            .remove(&UserId(actor_id.to_owned()));
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

    pub(crate) fn apply_interaction_at(
        &mut self,
        interaction: SemanticInteraction,
        now: Instant,
    ) -> Result<bool, tilo_actions::InteractionViolation> {
        interaction.validate()?;
        if let Some(current) = self.interactions.get(&interaction.invocation) {
            let same_identity = current.interaction.actor == interaction.actor
                && current.interaction.action == interaction.action
                && current.interaction.resource == interaction.resource
                && current.interaction.group == interaction.group;
            if !same_identity
                || interaction.sequence <= current.interaction.sequence
                || current.interaction.phase.is_terminal()
            {
                return Ok(false);
            }
        }

        let user = UserId(interaction.actor.id.clone());
        self.set_participant_kind(&user, ParticipantKind::from(interaction.actor.kind));
        self.set_acting_for(&user, interaction.actor.for_person.clone().map(UserId));
        // Software the workspace's roster does not know (a Shortcut's run) is
        // named by the run it works in.
        if interaction.actor.kind.acts_for_someone()
            && !self.names.contains_key(&user)
            && let Some(group) = &interaction.group
        {
            self.set_display_name(user.clone(), group.label.clone());
        }
        let (first_received_at, superseded, pulsed) =
            match self.interactions.get(&interaction.invocation) {
                Some(entry) => (entry.first_received_at, entry.superseded, entry.pulsed_at),
                None if interaction.actor.kind.acts_for_someone() => {
                    let (starts_at, pulsed) = self.pace_agent(&interaction, now);
                    (starts_at, false, pulsed)
                }
                None => (now, false, None),
            };
        // A phase within the flash its invocation set off carries that flash
        // on.
        let pulsed_at = self.pulse_of(&interaction, now).or(pulsed);
        self.interactions.insert(
            interaction.invocation,
            InteractionPresenceEntry {
                interaction,
                first_received_at,
                received_at: now,
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
        let actor = &interaction.actor.id;
        let mut resting: Option<Instant> = None;
        let mut pulsed = None;
        for entry in self.interactions.values_mut().filter(|entry| {
            !entry.superseded
                && entry.alive_at(now)
                && entry.interaction.actor.kind.acts_for_someone()
                && entry.interaction.actor.id == *actor
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

    /// When `interaction`'s arrival flashes its actor's cursor: now, unless
    /// a flash of the same agent went off within [`AGENT_PULSE_QUIET`].
    fn pulse_of(&self, interaction: &SemanticInteraction, now: Instant) -> Option<Instant> {
        let quiet = self.interactions.values().any(|entry| {
            entry.interaction.actor.kind.acts_for_someone()
                && entry.interaction.actor.id == interaction.actor.id
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

    /// Current, non-expired interactions for a resource. App-native renderers
    /// use this to project text ranges, cells, objects, rows, and timelines.
    pub fn interactions_for_resource(
        &self,
        resource_id: &str,
        now: Instant,
    ) -> Vec<&SemanticInteraction> {
        let mut entries = self
            .interactions
            .values()
            .filter(|entry| entry.visible_at(now) && entry.interaction.resource.id == resource_id)
            .collect::<Vec<_>>();
        sort_entries(&mut entries);
        entries
            .into_iter()
            .map(|entry| &entry.interaction)
            .collect()
    }

    /// Say that this frame paints `invocation`'s exact place: the characters
    /// a write landed in, or the row it names. The activity rail then leaves
    /// the invocation out, so one action is not named twice on one frame.
    pub fn note_shown(&self, invocation: InvocationId) {
        if self.interactions.contains_key(&invocation) {
            self.shown.borrow_mut().insert(invocation);
        }
    }

    /// What widgets reported shown since the last frame; taken by the pass
    /// that would otherwise put them in the rail.
    pub(crate) fn take_shown(&self) -> Vec<InvocationId> {
        self.shown.borrow_mut().drain().collect()
    }

    /// How strongly an interaction still shows, from when its current phase
    /// arrived: what the text it wrote fades by. `None` when it is no longer
    /// shown, or its mark has faded out.
    pub fn interaction_fade(&self, invocation: InvocationId, now: Instant) -> Option<f32> {
        let entry = self.interactions.get(&invocation)?;
        if !entry.visible_at(now) {
            return None;
        }
        crate::motion::Trace::alpha(now.saturating_duration_since(entry.received_at))
    }

    /// All currently visible interactions in deterministic display order.
    /// The host uses this read-only view to resolve generic semantic-control
    /// anchors against the current accessibility tree before falling back to
    /// the activity rail.
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
        let mut newest = HashMap::<&str, &InteractionPresenceEntry>::new();
        for entry in self.interactions.values().filter(|entry| {
            entry.visible_at(now) && entry.interaction.actor.kind.acts_for_someone()
        }) {
            let slot = newest
                .entry(entry.interaction.actor.id.as_str())
                .or_insert(entry);
            if marker_entry_is_newer(entry, slot) {
                *slot = entry;
            }
        }
        let mut entries = newest.into_values().collect::<Vec<_>>();
        entries.compact_sort_by(|left, right| {
            left.interaction.actor.id.cmp(&right.interaction.actor.id)
        });
        entries
            .into_iter()
            .map(|entry| entry.interaction.clone())
            .collect()
    }

    /// Visible activity excluding invocations already projected onto exact
    /// action loci. This keeps a resolved marker/outline from also appearing
    /// as a generic fallback card.
    pub fn visible_interactions_excluding(
        &self,
        now: Instant,
        resolved: &[InvocationId],
    ) -> Vec<SemanticInteraction> {
        self.visible_entries_excluding(now, resolved)
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

    /// Earliest discrete wake at which visible semantic activity expires.
    /// Motion is intentionally omitted: [`Self::interaction_animation_demand`]
    /// drives those short frame sequences.
    pub fn interaction_next_wake_deadline(&self, now: Instant) -> Option<Instant> {
        self.interactions
            .values()
            .map(|entry| {
                // A cursor waiting for its rest on the last target to end
                // moves on then.
                if now < entry.first_received_at && !entry.superseded {
                    entry.first_received_at
                } else {
                    let expiry = entry.expiry_deadline();
                    expiry.checked_add(EXPIRY_EPSILON).unwrap_or(expiry)
                }
            })
            .map(|deadline| deadline.max(now))
            .min()
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
        if !interaction.actor.kind.acts_for_someone()
            || !valid_marker_rect(locus)
            || !valid_marker_rect(viewport)
        {
            return None;
        }
        let entry = self.interactions.get(&interaction.invocation)?;
        if !entry.visible_at(now)
            || &entry.interaction != interaction
            || self
                .newest_agent_entry(&interaction.actor.id, now)
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
        let user = UserId(interaction.actor.id.clone());
        // The tip rests on the control the invocation is bound to: what the
        // agent would press, fully inside the frame.
        let control = control.intersect(viewport);
        let target = if valid_marker_rect(control) {
            control.center()
        } else {
            visible_locus.center()
        };
        let previous = self.interaction_markers.get(&user);
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
        let actor_color = super::render::color_for(self, &user, paint.theme);
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
        // The agent wears the nametag it wears wherever it points, in its
        // colour, saying what it is doing here: one tag for the agent and its
        // action, clear of the text around it.
        let words = self.interaction_status_label(interaction);
        let (bounds, tag) = super::render::draw_agent_nametag(
            paint.scene,
            paint.transform,
            paint.text,
            paint.theme,
            (tip, viewport),
            (visible_locus, &place.ink),
            &words,
            actor_color,
        );
        self.note_caret_tag(super::CaretTag {
            user: user.clone(),
            label: words,
            rect: paint.transform.transform_rect_bbox(tag),
        });
        let marker = AgentActionMarker {
            invocation: interaction.invocation,
            tip,
            target,
            bounds,
            approach,
            pulse,
        };
        self.interaction_markers.insert(
            user,
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
        let user = UserId(interaction.actor.id.clone());
        let actor = actor_label(self, &user, interaction.actor.kind.acts_for_someone());
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
    pub fn interaction_status_label(&self, interaction: &SemanticInteraction) -> String {
        self.interaction_status_label_doing(interaction, &doing_phrase(&interaction.action.label))
    }

    /// [`Self::interaction_status_label`] where the place says what the action
    /// is `doing` there better than its own label does: a field names what is
    /// edited in it.
    pub fn interaction_status_label_doing(
        &self,
        interaction: &SemanticInteraction,
        doing: &str,
    ) -> String {
        let user = UserId(interaction.actor.id.clone());
        // The adjacent shared identity mark already distinguishes an Agent.
        // Do not append another role to names such as "Release Notes Agent".
        let actor = self
            .names
            .get(&user)
            .and_then(|name| crate::history_source::human_label(name, &user.0))
            .map_or_else(
                || {
                    crate::i18n::widget_format(
                        if interaction.actor.kind.acts_for_someone() {
                            "interaction-agent"
                        } else {
                            "interaction-collaborator"
                        },
                        &[],
                    )
                },
                str::to_owned,
            );
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

    /// Lay out the generic app-level fallback without overlap. At most three
    /// rows are shown; excess activity is summarized on the last row.
    pub fn interaction_card_layout(
        &self,
        viewport: Rect,
        now: Instant,
    ) -> Vec<InteractionCardPlacement> {
        self.interaction_card_layout_excluding(viewport, now, &[])
    }

    /// Lay out fallback cards without duplicating invocations whose exact
    /// action locus the host has resolved for this frame.
    pub fn interaction_card_layout_excluding(
        &self,
        viewport: Rect,
        now: Instant,
        resolved: &[InvocationId],
    ) -> Vec<InteractionCardPlacement> {
        let entries = self.visible_entries_excluding(now, resolved);
        if entries.is_empty() || viewport.width() < 96.0 || viewport.height() < 32.0 {
            return Vec::new();
        }
        let vertical_room = (viewport.height() - CARD_MARGIN * 2.0).max(0.0);
        let fit = ((vertical_room + CARD_GAP) / (CARD_HEIGHT + CARD_GAP)).floor() as usize;
        let visible = entries.len().min(MAX_VISIBLE_CARDS).min(fit);
        if visible == 0 {
            return Vec::new();
        }
        let width = CARD_WIDTH.min((viewport.width() - CARD_MARGIN * 2.0).max(80.0));
        let mut out = Vec::with_capacity(visible);
        for (index, entry) in entries.iter().copied().take(visible).enumerate() {
            let y = viewport.y0 + CARD_MARGIN + index as f64 * (CARD_HEIGHT + CARD_GAP);
            out.push(InteractionCardPlacement {
                invocation: entry.interaction.invocation,
                rect: Rect::new(
                    viewport.x1 - CARD_MARGIN - width,
                    y,
                    viewport.x1 - CARD_MARGIN,
                    y + CARD_HEIGHT,
                ),
                additional: if index + 1 == visible {
                    entries.len().saturating_sub(visible)
                } else {
                    0
                },
            });
        }
        out
    }

    /// Draw generic activity only for work not already projected onto an
    /// exact locus in this frame.
    pub fn draw_interactions_excluding(
        &self,
        scene: &mut Scene,
        transform: Affine,
        viewport: Rect,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        now: Instant,
        resolved: &[InvocationId],
    ) {
        let entries = self.visible_entries_excluding(now, resolved);
        let placements = self.interaction_card_layout_excluding(viewport, now, resolved);
        for (entry, placement) in entries.iter().zip(&placements) {
            self.draw_interaction_card(scene, transform, text_renderer, theme, entry, placement);
        }
    }

    fn visible_entries(&self, now: Instant) -> Vec<&InteractionPresenceEntry> {
        self.visible_entries_excluding(now, &[])
    }

    fn newest_agent_entry(
        &self,
        actor_id: &str,
        now: Instant,
    ) -> Option<&InteractionPresenceEntry> {
        self.interactions
            .values()
            .filter(|entry| {
                entry.visible_at(now)
                    && entry.interaction.actor.kind.acts_for_someone()
                    && entry.interaction.actor.id == actor_id
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

    fn visible_entries_excluding(
        &self,
        now: Instant,
        excluded: &[InvocationId],
    ) -> Vec<&InteractionPresenceEntry> {
        let mut entries = self
            .interactions
            .values()
            .filter(|entry| {
                entry.visible_at(now) && !excluded.contains(&entry.interaction.invocation)
            })
            .collect::<Vec<_>>();
        sort_entries(&mut entries);
        entries
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "card widths are bounded UI pixels; f32 text shaping precision is sufficient"
    )]
    fn draw_interaction_card(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        entry: &InteractionPresenceEntry,
        placement: &InteractionCardPlacement,
    ) {
        let interaction = &entry.interaction;
        let rect = placement.rect;
        let shape = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.control));
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
            None,
            &shape,
        );
        let user = UserId(interaction.actor.id.clone());
        let actor_color = super::render::color_for(self, &user, theme);
        let phase_color = phase_color(&interaction.phase, theme, actor_color);
        let stroke = if matches!(interaction.phase, InteractionPhase::Locating) {
            Stroke::new(1.5).with_dashes(0.0, [4.0, 3.0])
        } else {
            Stroke::new(1.5)
        };
        scene.stroke(&stroke, transform, phase_color, None, &shape);
        let bar = Rect::new(rect.x0, rect.y0 + 7.0, rect.x0 + 3.0, rect.y1 - 7.0);
        scene.fill(Fill::NonZero, transform, phase_color, None, &bar);

        let is_agent = interaction.actor.kind.acts_for_someone();
        let glyph_center = Point::new(rect.x0 + 15.0, rect.y0 + 16.0);
        let glyph_outline = theme.on_surface(actor_color).ink;
        if is_agent {
            crate::agent_identity::draw_agent_identity_mark(
                scene,
                transform,
                Rect::from_center_size(glyph_center, (10.0, 10.0)),
                actor_color,
                theme,
                crate::agent_identity::AgentIdentityStyle::Solid,
            );
        } else {
            let dot = vello::kurbo::Circle::new(glyph_center, 4.5);
            scene.fill(Fill::NonZero, transform, actor_color, None, &dot);
            scene.stroke(&Stroke::new(1.2), transform, glyph_outline, None, &dot);
        }
        let actor_label = actor_label(self, &user, is_agent);
        let style = theme.text(TextRole::Caption);
        let first = tr.elide_to_width_style(&actor_label, (rect.width() - 40.0) as f32, &style);
        tr.draw_style(
            scene,
            &first,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((rect.x0 + 27.0, rect.y0 + 8.0)),
            None,
            &style.clone().with_weight(TextWeight::Medium),
        );

        // What it is doing and where; the phase is the mark's fade, not words.
        let mut detail = format!(
            "{} · {}",
            doing_phrase(&interaction.action.label),
            interaction.resource.label
        );
        if placement.additional > 0 {
            use std::fmt::Write as _;
            let _ = write!(detail, " · +{} more", placement.additional);
        }
        let detail = tr.elide_to_width_style(&detail, (rect.width() - 24.0) as f32, &style);
        tr.draw_style(
            scene,
            &detail,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((rect.x0 + 12.0, rect.y0 + 29.0)),
            None,
            &style,
        );
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
        let age = now.saturating_duration_since(self.received_at);
        if self.interaction.phase.is_terminal() {
            age <= TERMINAL_INTERACTION_DWELL
        } else {
            age <= ACTIVE_INTERACTION_EXPIRY
        }
    }

    fn expiry_deadline(&self) -> Instant {
        let dwell = if self.interaction.phase.is_terminal() {
            TERMINAL_INTERACTION_DWELL
        } else {
            ACTIVE_INTERACTION_EXPIRY
        };
        self.received_at
            .checked_add(dwell)
            .unwrap_or(self.received_at)
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

fn marker_entry_is_newer(
    candidate: &InteractionPresenceEntry,
    current: &InteractionPresenceEntry,
) -> bool {
    candidate.first_received_at > current.first_received_at
        || (candidate.first_received_at == current.first_received_at
            && candidate.interaction.invocation.0.as_bytes()
                > current.interaction.invocation.0.as_bytes())
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

/// What an actor is doing, in the app's own words: its action's label as a
/// present-tense verb — "checking off", "writing" — because a mark reads as
/// something happening now, not as a state printed beside a name.
fn doing_phrase(label: &str) -> String {
    let mut words = label.split_whitespace();
    let Some(first) = words.next() else {
        return crate::i18n::widget_format("interaction-working", &[]);
    };
    // "New note" names what the action makes, not a verb: the actor is creating it, never
    // "newing" it.
    let start = participle(if first.eq_ignore_ascii_case("new") {
        "create"
    } else {
        first
    });
    words.fold(start, |mut phrase, word| {
        phrase.push(' ');
        phrase.push_str(word);
        phrase
    })
}

/// The -ing form of a verb as an app words it. Verbs that double their last
/// consonant are listed: no rule derives "setting" from "set" without also
/// turning "visit" into "visitting".
fn participle(verb: &str) -> String {
    const DOUBLING: &[&str] = &[
        "begin", "commit", "cut", "drop", "embed", "fit", "forget", "get", "hit", "log", "map",
        "permit", "pin", "plan", "prefer", "put", "quit", "refer", "run", "scan", "set", "ship",
        "shop", "sit", "skip", "stop", "submit", "tag", "trim", "wrap",
    ];
    let verb = verb.trim_end_matches(['.', ',', ':', ';', '!', '?']);
    let lower = verb.to_lowercase();
    if let Some(stem) = lower.strip_suffix("ie") {
        return format!("{stem}ying");
    }
    if let Some(stem) = lower.strip_suffix('e')
        && !stem.is_empty()
        && !stem.ends_with(['e', 'y', 'o'])
    {
        return format!("{stem}ing");
    }
    if let Some(last) = lower.chars().last()
        && DOUBLING.contains(&lower.as_str())
    {
        return format!("{lower}{last}ing");
    }
    format!("{lower}ing")
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

fn phase_color(phase: &InteractionPhase, theme: &Theme, actor: Color) -> Color {
    match phase {
        InteractionPhase::Waiting { .. } => theme.tokens.colors.semantic.warning,
        InteractionPhase::Committed => theme.tokens.colors.semantic.success,
        InteractionPhase::Failed { .. } => theme.tokens.colors.semantic.danger,
        InteractionPhase::Cancelled { .. } => theme.tokens.colors.text_dim,
        InteractionPhase::Locating
        | InteractionPhase::Applying
        | InteractionPhase::Progress { .. } => actor,
    }
}

fn actor_label(presence: &PresenceCtx, user: &UserId, is_agent: bool) -> String {
    let roster_name = presence
        .names
        .get(user)
        .and_then(|name| crate::history_source::human_label(name, &user.0));
    // A roster that knows a participant only by its key names it by that
    // key; a raw hex id is no name, so the card says what kind of
    // participant acts instead.
    match (is_agent, roster_name) {
        (true, Some(name)) if presence.is_agent(user) => presence.kind_label(user, name),
        (true, Some(name)) => crate::history_source::agent_display_label(name),
        (true, None) => crate::i18n::widget_format("interaction-agent", &[]),
        (false, Some(name)) => name.to_string(),
        (false, None) => crate::i18n::widget_format("interaction-collaborator", &[]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_render::TextRenderer;
    use tilo_actions::{
        ActionResource, ActorKind, INTERACTION_VERSION, InteractionAction, InteractionActor,
    };
    use uuid::Uuid;

    fn interaction(
        invocation: u128,
        sequence: u64,
        phase: InteractionPhase,
    ) -> SemanticInteraction {
        SemanticInteraction {
            version: INTERACTION_VERSION,
            invocation: InvocationId(Uuid::from_u128(invocation)),
            sequence,
            actor: InteractionActor {
                id: "quill".into(),
                kind: ActorKind::Agent,
                for_person: None,
            },
            phase,
            action: InteractionAction {
                id: "rename".into(),
                label: "Rename".into(),
                app: None,
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

    /// An agent or a Shortcut acting for a person is named with that person
    /// and gets the software mark and marker a person's activity never does.
    #[test]
    fn an_actor_is_named_with_the_person_it_acts_for() {
        let at = crate::time::now();
        let mut presence = PresenceCtx::empty();
        presence.set_display_name(UserId("ada".into()), "Ada");
        presence.set_display_name(UserId("quill".into()), "Claude");
        let mut agent = interaction(30, 1, InteractionPhase::Applying);
        agent.actor.for_person = Some("ada".into());
        assert!(presence.apply_interaction_at(agent.clone(), at).unwrap());
        let claude = UserId("quill".into());
        assert_eq!(presence.display_name(&claude), "Claude, for Ada");
        assert_eq!(presence.acting_for(&claude), Some(&UserId("ada".into())));
        assert!(presence.is_agent(&claude));
        assert_eq!(presence.agent_marker_interactions(at), vec![agent]);

        let mut shortcut = interaction(31, 1, InteractionPhase::Applying);
        shortcut.actor = InteractionActor::new("shortcut:morning", ActorKind::Shortcut);
        shortcut.group = Some(tilo_actions::WorkGroup {
            id: "run-7".into(),
            label: "Morning".into(),
            kind: "shortcut".into(),
        });
        assert!(presence.apply_interaction_at(shortcut, at).unwrap());
        let morning = UserId("shortcut:morning".into());
        assert_eq!(
            presence.display_name(&morning),
            "Morning · Shortcut",
            "named by its run when the roster does not know it"
        );
        assert_eq!(presence.agent_marker_interactions(at).len(), 2);

        let mut person = interaction(32, 1, InteractionPhase::Applying);
        person.actor = InteractionActor::new("bob", ActorKind::Person);
        presence.set_display_name(UserId("bob".into()), "Bob");
        assert!(presence.apply_interaction_at(person, at).unwrap());
        assert_eq!(presence.display_name(&UserId("bob".into())), "Bob");
        assert_eq!(
            presence.agent_marker_interactions(at).len(),
            2,
            "a collaborator's own action keeps the plain outline"
        );
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

    #[test]
    fn interaction_updates_are_monotonic_bound_to_their_actor_and_expire() {
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
            assert_eq!(
                presence.interactions_for_resource("file:draft", at)[0].sequence,
                3
            );
        }
        // phase: an_invocation_cannot_substitute_its_actor_or_target
        {
            let at = crate::time::now();
            let mut presence = PresenceCtx::empty();
            presence
                .apply_interaction_at(interaction(2, 1, InteractionPhase::Applying), at)
                .unwrap();
            let mut forged = interaction(2, 2, InteractionPhase::Committed);
            forged.actor.id = "someone-else".into();
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
            terminal_presence
                .apply_interaction_at(interaction(4, 1, InteractionPhase::Committed), at)
                .unwrap();
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

    #[test]
    fn interactions_paint_only_their_addressed_surface() {
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
            files.apply_remote_presence("broker-agent".into(), key.clone(), payload.clone());
            assert!(!files.has_visible_interactions(at));

            let mut writer = PresenceCtx::empty();
            writer.set_surface_id("org.tilo.Writer");
            writer.apply_remote_presence("broker-agent".into(), key, payload);
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
        // phase: fallback_cards_do_not_overlap_and_fold_excess_activity
        {
            let at = crate::time::now();
            let mut presence = PresenceCtx::empty();
            for id in 1..=5 {
                let mut someone = interaction(id, 1, InteractionPhase::Applying);
                someone.actor.id = format!("agent-{id}");
                presence.apply_interaction_at(someone, at).unwrap();
            }
            let cards = presence.interaction_card_layout(Rect::new(0.0, 0.0, 800.0, 600.0), at);
            assert_eq!(cards.len(), 3);
            assert_eq!(cards[2].additional, 2);
            for pair in cards.windows(2) {
                assert!(pair[0].rect.intersect(pair[1].rect).is_zero_area());
            }
        }
    }

    #[test]
    fn agent_action_markers_are_truthful_bounded_paint_only_motion() {
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
        assert_eq!(first.target, Point::new(600.0, 300.0));
        assert!(locus.contains(first.target));
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
        assert_eq!(settled.tip, Point::new(600.0, 300.0));
        assert_eq!(settled.approach, 1.0);
        assert_eq!(settled.pulse, 0.0);
        assert!(!presence.interaction_animation_demand(settled_at, &theme));
        assert!(presence.pointers.is_empty());
        assert!(presence.carets.is_empty());
        assert!(presence.selections.is_empty());
        assert!(presence.remote_cursors.is_empty());
    }

    #[test]
    fn agent_action_markers_choose_one_newest_action_and_preserve_recent_continuity() {
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
        assert_eq!(reduced_frame.tip, Point::new(450.0, 330.0));
        assert_eq!(reduced_frame.pulse, 0.0);
        assert!(!presence.interaction_animation_demand(second_at, &reduced));
    }

    #[test]
    fn agent_marker_continuity_never_crosses_surface_viewport_or_long_travel_bounds() {
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
                Rect::new(40.0, 90.0, 100.0, 130.0),
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

    fn on(target: &str, invocation: u128, phase: InteractionPhase) -> SemanticInteraction {
        let mut action = interaction(invocation, 1, phase);
        action.resource.id = target.into();
        action
    }

    /// A fast agent never strobes: a burst on one target is one action at
    /// rest there, a new target waits until the cursor rested on the last
    /// one, and only the newest of the targets that waited is gone to.
    #[test]
    fn a_fast_agents_actions_are_paced_so_a_person_can_follow_them() {
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

    #[test]
    fn resolved_interactions_are_excluded_from_fallback_and_expire_on_a_precise_wake() {
        let at = crate::time::now();
        let mut presence = PresenceCtx::empty();
        let resolved = interaction(60, 1, InteractionPhase::Applying);
        let mut unresolved = interaction(61, 1, InteractionPhase::Applying);
        unresolved.actor.id = "another-agent".into();
        presence.apply_interaction_at(resolved.clone(), at).unwrap();
        presence
            .apply_interaction_at(unresolved.clone(), at)
            .unwrap();
        let remaining = presence.visible_interactions_excluding(at, &[resolved.invocation]);
        assert_eq!(remaining, vec![unresolved]);
        let cards = presence.interaction_card_layout_excluding(
            Rect::new(0.0, 0.0, 800.0, 600.0),
            at,
            &[resolved.invocation],
        );
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].invocation, InvocationId(Uuid::from_u128(61)));
        assert_eq!(
            presence.interaction_next_wake_deadline(at),
            Some(at + ACTIVE_INTERACTION_EXPIRY + EXPIRY_EPSILON)
        );
    }

    #[test]
    fn an_action_reads_as_what_its_actor_is_doing() {
        for (label, doing) in [
            ("Edit", "editing"),
            ("Insert", "inserting"),
            ("Write", "writing"),
            ("Check off", "checking off"),
            ("Open", "opening"),
            ("Set status", "setting status"),
            ("Stop", "stopping"),
            ("Apply", "applying"),
            ("Tag", "tagging"),
            ("Rename the note", "renaming the note"),
            ("New note", "creating note"),
            ("New sub-note", "creating sub-note"),
        ] {
            assert_eq!(doing_phrase(label), doing, "{label}");
        }
        assert_eq!(doing_phrase("  "), "working");
    }

    #[test]
    fn a_place_a_widget_painted_leaves_the_rail() {
        let at = crate::time::now();
        let mut presence = PresenceCtx::empty();
        let event = interaction(70, 1, InteractionPhase::Applying);
        presence.apply_interaction_at(event.clone(), at).unwrap();
        assert_eq!(
            presence.visible_entries_excluding(at, &[]).len(),
            1,
            "an unwritten place is named by the rail"
        );
        presence.note_shown(event.invocation);
        let shown = presence.take_shown();
        assert_eq!(shown, vec![event.invocation]);
        assert!(
            presence.visible_entries_excluding(at, &shown).is_empty(),
            "the characters are the mark; the rail leaves them out"
        );
        assert!(presence.take_shown().is_empty(), "shown is one frame's");
        presence.note_shown(InvocationId(Uuid::from_u128(999)));
        assert!(
            presence.take_shown().is_empty(),
            "an invocation this surface never saw cannot silence a rail entry"
        );
    }

    #[test]
    fn interactions_are_labelled_and_narrated_as_actions() {
        // The exact-control badge names the action and lifecycle first, so
        // elision does not leave only an actor with an ambiguous "Applying".
        {
            let mut presence = PresenceCtx::empty();
            presence
                .names
                .insert(UserId("quill".into()), "Release Notes Agent".into());
            assert_eq!(
                presence.interaction_status_label(&interaction(11, 1, InteractionPhase::Applying)),
                "Release Notes Agent: renaming"
            );
            presence.names.clear();
            assert_eq!(
                presence.interaction_status_label(&interaction(11, 1, InteractionPhase::Applying)),
                "Agent: renaming"
            );
        }
        // phase: agent_identity_is_textual_and_does_not_fall_back_to_a_raw_id
        {
            let user = UserId("agent:opaque-runtime-id".into());
            let mut presence = PresenceCtx::empty();
            assert_eq!(actor_label(&presence, &user, true), "Agent");
            presence.names.insert(user.clone(), "Quill".into());
            assert_eq!(actor_label(&presence, &user, true), "Quill · Agent");
            assert_eq!(actor_label(&presence, &user, false), "Quill");
        }
        // A roster that knows a participant only by its key hands that key
        // back as its name; the card names the kind of participant instead.
        {
            let key = "a4b05b334073f7dc3c31652f1b0143be8b7f5e0c2d9a1f6b3e4c5d6a7b8c9d0e";
            let user = UserId(key.into());
            let mut presence = PresenceCtx::empty();
            presence.set_display_name(user.clone(), key);
            assert_eq!(actor_label(&presence, &user, true), "Agent");
            assert_eq!(actor_label(&presence, &user, false), "Collaborator");
            let mut keyed = interaction(12, 1, InteractionPhase::Committed);
            keyed.actor = InteractionActor::new(key, ActorKind::Person);
            assert_eq!(
                presence.interaction_status_label(&keyed),
                "Collaborator: renaming"
            );
            let agent = UserId("quill".into());
            presence.set_display_name(agent.clone(), "Claude");
            presence.set_participant_kind(&agent, ParticipantKind::Agent);
            presence.set_acting_for(&agent, Some(user));
            assert_eq!(
                presence.display_name(&agent),
                "Claude · Agent",
                "an unnamed person it acts for is left out, not shown as a key"
            );
        }
        // phase: accessibility_narrates_real_action_state_without_pointer_language
        {
            let mut presence = PresenceCtx::empty();
            presence
                .names
                .insert(UserId("quill".into()), "Quill".into());
            let applying = interaction(11, 1, InteractionPhase::Applying);
            assert_eq!(
                presence.interaction_announcement(&applying).as_deref(),
                Some("Quill · Agent, Applying: Rename on Draft.md")
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
}
