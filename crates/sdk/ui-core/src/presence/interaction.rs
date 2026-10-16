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
use vello::{
    Scene,
    kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Stroke},
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
const EXPIRY_EPSILON: Duration = Duration::from_nanos(1);
const MAX_VISIBLE_CARDS: usize = 3;
const CARD_WIDTH: f64 = 300.0;
const CARD_HEIGHT: f64 = 52.0;
const CARD_GAP: f64 = 6.0;
const CARD_MARGIN: f64 = 12.0;

#[derive(Debug, Clone)]
pub(super) struct InteractionPresenceEntry {
    interaction: SemanticInteraction,
    /// Arrival of the invocation itself. Phase restatements must not restart
    /// its approach from the edge or make an old invocation look newest.
    first_received_at: Instant,
    /// Arrival of this phase. Drives its brief pulse and visible dwell.
    received_at: Instant,
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
    ///
    /// # Errors
    ///
    /// Returns the interaction contract's validation error when the incoming
    /// update is malformed or exceeds its wire bounds.
    pub fn apply_interaction(
        &mut self,
        interaction: SemanticInteraction,
    ) -> Result<bool, tilo_actions::InteractionViolation> {
        self.apply_interaction_at(interaction, crate::time::now())
    }

    /// Observe semantic work without claiming its resource belongs to this
    /// rendering surface. The shell People roster uses this after the room
    /// broker has authenticated and routed the event; app canvases continue
    /// to use `apply_remote_presence`, whose resource-key check prevents an
    /// interaction from painting in unrelated applications.
    ///
    /// # Errors
    ///
    /// Returns the interaction contract's validation error when the incoming
    /// update is malformed or exceeds its wire bounds.
    pub fn observe_interaction(
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
        // Software the room's roster does not know (a Shortcut's run) is
        // named by the run it works in.
        if interaction.actor.kind.acts_for_someone()
            && !self.names.contains_key(&user)
            && let Some(group) = &interaction.group
        {
            self.set_display_name(user.clone(), group.label.clone());
        }
        let first_received_at = self
            .interactions
            .get(&interaction.invocation)
            .map_or(now, |entry| entry.first_received_at);
        self.interactions.insert(
            interaction.invocation,
            InteractionPresenceEntry {
                interaction,
                first_received_at,
                received_at: now,
            },
        );
        Ok(true)
    }

    /// Remove interactions whose visible dwell has ended. Returns whether a
    /// repaint is warranted.
    pub fn prune_interactions(&mut self, now: Instant) -> bool {
        let before = self.interactions.len();
        self.interactions.retain(|_, entry| entry.visible_at(now));
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
        entries.sort_by(|left, right| left.interaction.actor.id.cmp(&right.interaction.actor.id));
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

    /// Visible semantic work whose authenticated action catalogue names this
    /// application as its owner. Missing ownership is intentionally excluded:
    /// a compositor must never guess which foreign window may display it.
    pub fn visible_interactions_for_app(
        &self,
        app_id: &str,
        now: Instant,
    ) -> Vec<SemanticInteraction> {
        self.visible_entries(now)
            .into_iter()
            .filter(|entry| entry_belongs_to_app(entry, app_id))
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
            .map(InteractionPresenceEntry::expiry_deadline)
            .map(|deadline| deadline.checked_add(EXPIRY_EPSILON).unwrap_or(deadline))
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
        locus: Rect,
        viewport: Rect,
        surface: &str,
        now: Instant,
    ) -> Option<AgentActionMarker> {
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
        let received_at = entry.received_at;
        let user = UserId(interaction.actor.id.clone());
        // Avoid parking over the control's central label. The tip still
        // belongs to this exact bound control, not an inferred nearby point.
        let inset = f64::from(paint.theme.tokens.spacing.s2);
        let target = Point::new(
            visible_locus.x1 - inset.min(visible_locus.width() / 4.0),
            visible_locus.y1 - inset.min(visible_locus.height() / 4.0),
        );
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

        let durations = paint.theme.tokens.motion.durations;
        let travel = durations.slow().saturating_add(durations.base());
        let approach = if paint.theme.reduce_motion || travel.is_zero() {
            1.0
        } else {
            duration_fraction(now.saturating_duration_since(first_received_at), travel)
        };
        let eased = 1.0 - (1.0 - approach).powi(3);
        let tip = point_lerp(origin, target, eased);
        let pulse = if paint.theme.reduce_motion || durations.base().is_zero() {
            0.0
        } else {
            1.0 - duration_fraction(now.saturating_duration_since(received_at), durations.base())
        };
        let marker = AgentActionMarker {
            invocation: interaction.invocation,
            tip,
            target,
            bounds: agent_marker_bounds(tip, viewport),
            approach,
            pulse,
        };
        let actor_color = super::render::color_for(self, &user, paint.theme);
        draw_agent_marker_geometry(paint, &marker, actor_color, viewport);
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
        crate::i18n::widget_format(
            "interaction-action-status",
            &[
                ("name", actor.into()),
                ("action", interaction.action.label.clone().into()),
                ("phase", phase_label(&interaction.phase).into()),
            ],
        )
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

    /// Draw the generic rail only for semantic work explicitly owned by one
    /// application. Compositors use this for foreign surfaces: an external
    /// client gets collaboration by default, while activity from a different
    /// app can never be painted over its pixels.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "the result is bounded to three cards and viewport dimensions are UI-sized"
    )]
    pub fn draw_interactions_for_app(
        &self,
        scene: &mut Scene,
        transform: Affine,
        viewport: Rect,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        now: Instant,
        app_id: &str,
    ) {
        let entries = self
            .visible_entries(now)
            .into_iter()
            .filter(|entry| entry_belongs_to_app(entry, app_id))
            .collect::<Vec<_>>();
        if entries.is_empty() || viewport.width() < 96.0 || viewport.height() < 32.0 {
            return;
        }
        let vertical_room = (viewport.height() - CARD_MARGIN * 2.0).max(0.0);
        let fit = ((vertical_room + CARD_GAP) / (CARD_HEIGHT + CARD_GAP)).floor() as usize;
        let visible = entries.len().min(MAX_VISIBLE_CARDS).min(fit);
        let width = CARD_WIDTH.min((viewport.width() - CARD_MARGIN * 2.0).max(80.0));
        for (index, entry) in entries.iter().take(visible).enumerate() {
            let y = viewport.y0 + CARD_MARGIN + index as f64 * (CARD_HEIGHT + CARD_GAP);
            let placement = InteractionCardPlacement {
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
            };
            self.draw_interaction_card(scene, transform, text_renderer, theme, entry, &placement);
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

        let mut detail = format!(
            "{} · {} · {}",
            phase_label(&interaction.phase),
            interaction.action.label,
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
    fn visible_at(&self, now: Instant) -> bool {
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
        let durations = theme.tokens.motion.durations;
        now.saturating_duration_since(self.first_received_at)
            < durations.slow().saturating_add(durations.base())
            || now.saturating_duration_since(self.received_at) < durations.base()
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

fn duration_fraction(elapsed: Duration, total: Duration) -> f64 {
    (elapsed.as_secs_f64() / total.as_secs_f64()).clamp(0.0, 1.0)
}

fn point_lerp(from: Point, to: Point, progress: f64) -> Point {
    Point::new(
        from.x + (to.x - from.x) * progress,
        from.y + (to.y - from.y) * progress,
    )
}

fn agent_marker_bounds(tip: Point, viewport: Rect) -> Rect {
    let x = if tip.x + 22.0 > viewport.x1 {
        -1.0
    } else {
        1.0
    };
    let y = if tip.y + 20.0 > viewport.y1 {
        -1.0
    } else {
        1.0
    };
    (Affine::translate((tip.x, tip.y)) * Affine::scale_non_uniform(x, y))
        .transform_rect_bbox(Rect::new(-0.625, -0.625, 21.625, 19.625))
        .intersect(viewport)
}

fn draw_agent_marker_geometry(
    paint: &mut PaintCtx<'_>,
    marker: &AgentActionMarker,
    actor_color: Color,
    viewport: Rect,
) {
    if marker.pulse > 0.0 {
        let radius = 8.0 + 8.0 * (1.0 - marker.pulse);
        paint.scene.stroke(
            &Stroke::new(1.5),
            paint.transform,
            actor_color.with_alpha_factor((marker.pulse * 0.72) as f32),
            None,
            &Circle::new(marker.target, radius),
        );
    }

    // A conventional pointer silhouette communicates approach, while the
    // attached shared agent tile makes it unmistakably semantic presence.
    let tip = marker.tip;
    let direction_x = if tip.x + 22.0 > viewport.x1 {
        -1.0
    } else {
        1.0
    };
    let direction_y = if tip.y + 20.0 > viewport.y1 {
        -1.0
    } else {
        1.0
    };
    let transform = paint.transform
        * Affine::translate((tip.x, tip.y))
        * Affine::scale_non_uniform(direction_x, direction_y);
    let mut cursor = BezPath::new();
    cursor.move_to((0.0, 0.0));
    cursor.line_to((2.2, 17.0));
    cursor.line_to((6.1, 12.1));
    cursor.line_to((10.0, 19.0));
    cursor.line_to((13.0, 17.2));
    cursor.line_to((9.2, 10.5));
    cursor.line_to((15.6, 10.0));
    cursor.close_path();
    paint
        .scene
        .fill(Fill::NonZero, transform, actor_color, None, &cursor);
    paint.scene.stroke(
        &Stroke::new(1.25),
        transform,
        paint.theme.on_surface(actor_color).ink,
        None,
        &cursor,
    );
    crate::agent_identity::draw_agent_identity_mark(
        paint.scene,
        transform,
        Rect::new(10.0, 1.0, 21.0, 12.0),
        actor_color,
        paint.theme,
        crate::agent_identity::AgentIdentityStyle::Solid,
    );
}

fn sort_entries(entries: &mut [&InteractionPresenceEntry]) {
    entries.sort_by(|left, right| {
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

fn entry_belongs_to_app(entry: &InteractionPresenceEntry, app_id: &str) -> bool {
    entry
        .interaction
        .action
        .app
        .as_ref()
        .is_some_and(|app| app.id == app_id)
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
        shortcut.group = Some(tilo_actions::ActionGroup {
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
            locus,
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
        // phase: foreign_app_fallback_is_strictly_scoped_to_catalogue_owner
        {
            let at = crate::time::now();
            let mut presence = PresenceCtx::empty();
            let mut writer = interaction(20, 1, InteractionPhase::Applying);
            writer.action.app = Some(tilo_actions::ActionApp {
                id: "org.tilo.Writer".into(),
                label: "Writer".into(),
            });
            let mut files = interaction(21, 1, InteractionPhase::Applying);
            files.action.app = Some(tilo_actions::ActionApp {
                id: "org.tilo.Files".into(),
                label: "Files".into(),
            });
            let unowned = interaction(22, 1, InteractionPhase::Applying);
            assert!(presence.apply_interaction_at(writer, at).unwrap());
            assert!(presence.apply_interaction_at(files, at).unwrap());
            assert!(presence.apply_interaction_at(unowned, at).unwrap());

            let writer = presence.visible_interactions_for_app("org.tilo.Writer", at);
            assert_eq!(writer.len(), 1);
            assert_eq!(writer[0].action.label, "Rename");
            assert!(
                presence
                    .visible_interactions_for_app("org.tilo.Calendar", at)
                    .is_empty()
            );
        }
        // phase: room_fanout_only_paints_the_addressed_app_surface
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
                presence
                    .apply_interaction_at(interaction(id, 1, InteractionPhase::Applying), at)
                    .unwrap();
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
        assert_eq!(first.tip, Point::new(viewport.x1, 312.0));
        assert_eq!(first.target, Point::new(632.0, 312.0));
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
        assert!(advanced.approach > 0.45 && advanced.approach < 0.55);
        assert!(advanced.tip.x < first.tip.x);
        assert!(advanced.pulse > 0.99, "the new phase gets one short pulse");

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
        assert_eq!(settled.tip, Point::new(632.0, 312.0));
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
        assert_eq!(reduced_frame.tip, Point::new(492.0, 352.0));
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

    #[test]
    fn resolved_interactions_are_excluded_from_fallback_and_expire_on_a_precise_wake() {
        let at = crate::time::now();
        let mut presence = PresenceCtx::empty();
        let resolved = interaction(60, 1, InteractionPhase::Applying);
        let unresolved = interaction(61, 1, InteractionPhase::Applying);
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
                "Rename (Applying) - Release Notes Agent"
            );
            presence.names.clear();
            assert_eq!(
                presence.interaction_status_label(&interaction(11, 1, InteractionPhase::Applying)),
                "Rename (Applying) - Agent"
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
                "Rename (Committed) - Collaborator"
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
