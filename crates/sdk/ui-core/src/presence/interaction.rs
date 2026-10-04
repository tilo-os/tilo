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
use crate::permissions::UserId;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use crate::tokens::TextRole;
use tilo_actions::{InteractionActorKind, InteractionPhase, InvocationId, SemanticInteraction};
use vello::{
    Scene,
    kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke},
    peniko::{Color, Fill},
};

/// A live phase that has not been restated stops occupying the app surface.
/// The durable Task Center remains the place to diagnose a stalled run.
pub const ACTIVE_INTERACTION_EXPIRY: Duration = Duration::from_secs(15);
/// A terminal edge remains perceptible even when an immediate action starts
/// and completes between two rendered frames.
pub const TERMINAL_INTERACTION_DWELL: Duration = Duration::from_millis(3_500);
const MAX_VISIBLE_CARDS: usize = 3;
const CARD_WIDTH: f64 = 300.0;
const CARD_HEIGHT: f64 = 52.0;
const CARD_GAP: f64 = 6.0;
const CARD_MARGIN: f64 = 12.0;

#[derive(Debug, Clone)]
pub(super) struct InteractionPresenceEntry {
    interaction: SemanticInteraction,
    received_at: Instant,
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

    fn apply_interaction_at(
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
        self.set_participant_kind(
            &user,
            match interaction.actor.kind {
                InteractionActorKind::Person => ParticipantKind::Person,
                InteractionActorKind::Agent => ParticipantKind::Agent,
            },
        );
        self.interactions.insert(
            interaction.invocation,
            InteractionPresenceEntry {
                interaction,
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

    /// Concise live-region copy for a semantic action phase. `Locating` is
    /// intentionally silent: it is often immediately followed by `Applying`,
    /// and narrating both turns a fast action into accessibility chatter.
    pub fn interaction_announcement(&self, interaction: &SemanticInteraction) -> Option<String> {
        if matches!(interaction.phase, InteractionPhase::Locating) {
            return None;
        }
        let user = UserId(interaction.actor.id.clone());
        let actor = actor_label(
            self,
            &user,
            interaction.actor.kind == InteractionActorKind::Agent,
        );
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

    /// Lay out the generic app-level fallback without overlap. At most three
    /// rows are shown; excess activity is summarized on the last row.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "the result is bounded to three cards and viewport/text dimensions are UI-sized"
    )]
    pub fn interaction_card_layout(
        &self,
        viewport: Rect,
        now: Instant,
    ) -> Vec<InteractionCardPlacement> {
        let entries = self.visible_entries(now);
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
        for (index, entry) in entries.into_iter().take(visible).enumerate() {
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
                    self.interactions
                        .values()
                        .filter(|candidate| candidate.visible_at(now))
                        .count()
                        .saturating_sub(visible)
                } else {
                    0
                },
            });
        }
        out
    }

    /// Draw a compact generic fallback for interactions an application has
    /// not yet projected onto domain geometry. This is an activity rail, not
    /// a cursor: it never moves focus or implies keyboard/mouse navigation.
    pub fn draw_interactions(
        &self,
        scene: &mut Scene,
        transform: Affine,
        viewport: Rect,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        now: Instant,
    ) {
        let entries = self.visible_entries(now);
        let placements = self.interaction_card_layout(viewport, now);
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
        let mut entries = self
            .interactions
            .values()
            .filter(|entry| entry.visible_at(now))
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

        let is_agent = interaction.actor.kind == InteractionActorKind::Agent;
        let glyph_center = Point::new(rect.x0 + 15.0, rect.y0 + 16.0);
        let glyph_outline = theme.on_surface(actor_color).ink;
        draw_actor_glyph(
            scene,
            transform,
            glyph_center,
            actor_color,
            glyph_outline,
            is_agent,
        );
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
        InteractionPhase::Locating => "Inspecting".into(),
        InteractionPhase::Applying => "Applying".into(),
        InteractionPhase::Progress {
            label,
            completed,
            total,
        } => match (completed, total) {
            (Some(done), Some(total)) if *total > 0 => {
                format!("{label} · {done}/{total}")
            }
            _ => label.clone(),
        },
        InteractionPhase::Waiting { .. } => "Waiting".into(),
        InteractionPhase::Committed => "Committed".into(),
        InteractionPhase::Failed { .. } => "Failed".into(),
        InteractionPhase::Cancelled { .. } => "Cancelled".into(),
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

fn draw_actor_glyph(
    scene: &mut Scene,
    transform: Affine,
    center: Point,
    color: Color,
    outline: Color,
    is_agent: bool,
) {
    if is_agent {
        let radius = 5.0;
        let mut diamond = BezPath::new();
        diamond.move_to((center.x, center.y - radius));
        diamond.line_to((center.x + radius, center.y));
        diamond.line_to((center.x, center.y + radius));
        diamond.line_to((center.x - radius, center.y));
        diamond.close_path();
        scene.fill(Fill::NonZero, transform, color, None, &diamond);
        scene.stroke(&Stroke::new(1.2), transform, outline, None, &diamond);
    } else {
        let dot = vello::kurbo::Circle::new(center, 4.5);
        scene.fill(Fill::NonZero, transform, color, None, &dot);
        scene.stroke(&Stroke::new(1.2), transform, outline, None, &dot);
    }
}

fn actor_label(presence: &PresenceCtx, user: &UserId, is_agent: bool) -> String {
    let roster_name = presence
        .names
        .get(user)
        .map(String::as_str)
        .filter(|name| !name.trim().is_empty());
    match (is_agent, roster_name) {
        (true, Some(name)) => format!("{name} · AGENT"),
        (true, None) => "Agent".to_string(),
        (false, Some(name)) => name.to_string(),
        (false, None) => "Collaborator".to_string(),
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
    use tilo_actions::{ActionResource, INTERACTION_VERSION, InteractionAction, InteractionActor};
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
                kind: InteractionActorKind::Agent,
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
    fn interactions_are_labelled_and_narrated_as_actions() {
        // phase: agent_identity_is_textual_and_does_not_fall_back_to_a_raw_id
        {
            let user = UserId("agent:opaque-runtime-id".into());
            let mut presence = PresenceCtx::empty();
            assert_eq!(actor_label(&presence, &user, true), "Agent");
            presence.names.insert(user.clone(), "Quill".into());
            assert_eq!(actor_label(&presence, &user, true), "Quill · AGENT");
            assert_eq!(actor_label(&presence, &user, false), "Quill");
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
                Some("Quill · AGENT, Applying: Rename on Draft.md")
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
