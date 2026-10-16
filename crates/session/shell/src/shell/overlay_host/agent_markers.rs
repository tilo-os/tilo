//! Interactive semantics for compositor-validated, actually painted markers.
//! Geometry is descriptive; activation is returned to the compositor for a
//! fresh source/room/member check before an action session opens.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ui::widget::{Button, Decorator, Widget};
use ui::{LocalState, PresenceCtx, Theme};
use vello::kurbo::{Affine, Point, Rect};

pub(crate) const TARGET_PREFIX: &str = "agent-marker:";
const MAX_AGE: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub(crate) struct AccessibleAgentMarker {
    pub connection: u64,
    pub surface: u64,
    pub app_id: String,
    pub room: String,
    pub agent: String,
    pub interaction: tilo_actions::SemanticInteraction,
    pub marker: Rect,
    pub locus: Rect,
    pub protected: Rect,
    pub label: String,
    pub description: String,
    pub published_at: Instant,
}

impl AccessibleAgentMarker {
    pub(crate) fn action_target(&self) -> String {
        format!(
            "{TARGET_PREFIX}{}:{}:{}:{}",
            self.connection, self.surface, self.interaction.actor.id, self.interaction.invocation.0
        )
    }

    pub(crate) fn is_fresh(&self, now: Instant) -> bool {
        self.published_at
            .checked_add(MAX_AGE)
            .is_some_and(|deadline| now < deadline)
    }

    fn same_publication(&self, other: &Self) -> bool {
        self.action_target() == other.action_target()
            && self.app_id == other.app_id
            && self.room == other.room
            && self.agent == other.agent
            && self.interaction == other.interaction
            && self.marker == other.marker
            && self.locus == other.locus
            && self.protected == other.protected
    }

    fn valid(&self) -> bool {
        !self.room.is_empty()
            && !self.app_id.is_empty()
            && !self.agent.is_empty()
            && self.interaction.validate().is_ok()
            && self.interaction.actor.kind == tilo_actions::ActorKind::Agent
            && [self.marker, self.locus, self.protected]
                .iter()
                .all(|rect| rect.is_finite() && rect.width() > 0.0 && rect.height() > 0.0)
    }
}

/// Button supplies ordinary focus, keyboard, accessibility and hit routing.
/// Its pixels are the source's real marker, already painted in that frame;
/// this decorator does not paint a second cursor or invented activity.
struct MarkerControl {
    source: AccessibleAgentMarker,
    activation_source: LocalState<AccessibleAgentMarker>,
    button: Button,
    origin: Point,
}

impl Decorator for MarkerControl {
    type Inner = Button;
    fn inner(&self) -> &Button {
        &self.button
    }
    fn inner_mut(&mut self) -> &mut Button {
        &mut self.button
    }
    fn name(&self) -> &'static str {
        "AgentActionMarker"
    }
    fn draw(
        &mut self,
        _: &mut vello::Scene,
        _: Affine,
        _: &mut ui::text_render::TextRenderer,
        _: &Theme,
        _: &PresenceCtx,
    ) {
    }
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.button.accessibility().map(|node| {
            node.with_name(self.source.label.clone())
                .with_description(self.source.description.clone())
                .with_action_target(self.source.action_target())
        })
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let node = self.button.build_layout(tree, theme, nodes);
        if let Ok(style) = tree.style(node) {
            let mut style = style.clone();
            style.position = taffy::Position::Absolute;
            style.size = taffy::Size {
                width: taffy::Dimension::length(ui::num::f32_of_f64(self.source.marker.width())),
                height: taffy::Dimension::length(ui::num::f32_of_f64(self.source.marker.height())),
            };
            style.min_size = style.size;
            style.inset.left = taffy::LengthPercentageAuto::length(ui::num::f32_of_f64(
                self.source.marker.x0 - self.origin.x,
            ));
            style.inset.top = taffy::LengthPercentageAuto::length(ui::num::f32_of_f64(
                self.source.marker.y0 - self.origin.y,
            ));
            let _ = tree.set_style(node, style);
        }
        node
    }
}

pub(crate) struct AgentMarkers {
    controls: Vec<MarkerControl>,
    nodes: Vec<taffy::NodeId>,
    activation: LocalState<Option<AccessibleAgentMarker>>,
    dirty: bool,
}

impl Default for AgentMarkers {
    fn default() -> Self {
        Self {
            controls: Vec::new(),
            nodes: Vec::new(),
            activation: LocalState::new(None),
            dirty: false,
        }
    }
}

impl AgentMarkers {
    pub(crate) fn publish(&mut self, connection: u64, markers: Vec<AccessibleAgentMarker>) {
        if markers.is_empty()
            && !self
                .controls
                .iter()
                .any(|control| control.source.connection == connection)
        {
            self.retain(|source| source.is_fresh(Instant::now()));
            return;
        }
        let pending = self.activation.get();
        let (mut previous, retained): (Vec<_>, Vec<_>) = std::mem::take(&mut self.controls)
            .into_iter()
            .partition(|control| control.source.connection == connection);
        self.controls = retained;
        self.retain(|source| source.connection != connection && source.is_fresh(Instant::now()));
        for source in markers
            .into_iter()
            .filter(|source| source.connection == connection && source.valid())
            .take(16)
        {
            if let Some(index) = previous.iter().position(|control| {
                control.source.is_fresh(Instant::now())
                    && control.source.action_target() == source.action_target()
                    && control.source.app_id == source.app_id
                    && control.source.room == source.room
                    && control.source.agent == source.agent
            }) {
                // Preserve the shared Button's held gesture while the painted
                // marker animates. Its callback reads the latest publication.
                let mut control = previous.remove(index);
                control.activation_source.set(source.clone());
                control.source = source;
                self.controls.push(control);
            } else {
                let activation = self.activation.clone();
                let activation_source = LocalState::new(source.clone());
                let selected = activation_source.clone();
                let mut button = Button::new(&source.action_target(), &source.label)
                    .with_accessible_name(&source.description)
                    .on_click(move || activation.set(Some(selected.get())));
                button.set_enabled(false);
                self.controls.push(MarkerControl {
                    source,
                    activation_source,
                    button,
                    origin: Point::ZERO,
                });
            }
        }
        if let Some(pending) = pending
            && self
                .controls
                .iter()
                .any(|control| control.source.same_publication(&pending))
        {
            self.activation.set(Some(pending));
        }
        self.dirty = true;
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&AccessibleAgentMarker) -> bool) -> bool {
        let before = self.controls.len();
        self.controls.retain(|control| keep(&control.source));
        let changed = before != self.controls.len();
        self.dirty |= changed;
        if let Some(pending) = self.activation.get()
            && !self
                .controls
                .iter()
                .any(|control| control.source.same_publication(&pending))
        {
            self.activation.set(None);
        }
        changed
    }

    pub(crate) fn refresh(&mut self, presence: &PresenceCtx, suppressed: bool) {
        for control in &mut self.controls {
            control.button.set_enabled(
                !suppressed
                    && control.source.is_fresh(Instant::now())
                    && presence.participant_kind(&ui::permissions::UserId(
                        control.source.interaction.actor.id.clone(),
                    )) == ui::presence::ParticipantKind::Agent,
            );
        }
    }

    pub(crate) fn pointer_at(&self, point: (f64, f64)) -> bool {
        self.controls.iter().any(|control| {
            control.source.marker.contains(Point::new(point.0, point.1))
                && control.source.is_fresh(Instant::now())
                && Widget::is_enabled(control)
        })
    }

    pub(crate) fn take_activation(&self) -> Option<AccessibleAgentMarker> {
        let activation = self.activation.get();
        self.activation.set(None);
        activation.filter(|source| {
            source.is_fresh(Instant::now())
                && self.controls.iter().any(|control| {
                    control.source.same_publication(source) && Widget::is_enabled(control)
                })
        })
    }

    pub(crate) fn build(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
        origin: Point,
    ) -> Vec<taffy::NodeId> {
        self.nodes = self
            .controls
            .iter_mut()
            .map(|control| {
                control.origin = origin;
                Widget::build_layout(control, tree, theme, nodes)
            })
            .collect();
        self.nodes.clone()
    }

    pub(crate) fn actionable(&self, target: &str) -> bool {
        self.controls.iter().any(|control| {
            control.source.action_target() == target
                && control.source.is_fresh(Instant::now())
                && Widget::is_enabled(control)
        })
    }

    pub(crate) fn semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        edits: &mut ui::widget::Edits<'_>,
        path: &ui::widget::ModelPath,
    ) -> bool {
        if !self.actionable(target) {
            return true;
        }
        if let Some(control) = self
            .controls
            .iter_mut()
            .find(|control| control.source.action_target() == target)
        {
            Widget::perform_action(control, action, &[], edits, path);
        }
        true
    }

    pub(crate) fn apply(&mut self, tree: &taffy::TaffyTree<()>, origin: Point) {
        for (control, node) in self.controls.iter_mut().zip(&self.nodes) {
            Widget::apply_layout(control, tree, *node, origin);
        }
    }

    pub(crate) fn children(&self) -> impl Iterator<Item = &dyn Widget> {
        self.controls.iter().map(|control| control as &dyn Widget)
    }

    pub(crate) fn children_mut(&mut self) -> impl Iterator<Item = &mut dyn Widget> {
        self.controls
            .iter_mut()
            .map(|control| control as &mut dyn Widget)
    }

    pub(crate) fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.controls
            .iter()
            .filter_map(|control| control.source.published_at.checked_add(MAX_AGE))
            .min()
    }

    pub(crate) fn route_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        for (control, node) in self.controls.iter_mut().zip(&self.nodes) {
            if !control.source.is_fresh(Instant::now()) {
                continue;
            }
            Widget::handle_event(control, event, &mut ctx.reborrow(ctx.path, *node));
            if *ctx.handled {
                return;
            }
        }
    }
}

impl super::ShellOverlayHost {
    pub(crate) fn certified_marker_agent(
        &self,
        room: &str,
        actor: &str,
    ) -> Option<ui::collaboration::CertifiedAgentIdentity> {
        self.shortcut_triggers
            .broker()?
            .certified_agent_identity(room, actor)
    }

    pub(crate) fn marker_action_session_source(
        &self,
    ) -> Option<crate::shell::certified_action_session::ActionSessionSource> {
        self.shortcut_triggers
            .broker()
            .map(crate::shell::certified_action_session::ActionSessionSource::new)
    }

    pub(crate) fn publish_accessible_agent_markers(
        &mut self,
        connection: u64,
        markers: Vec<AccessibleAgentMarker>,
    ) {
        self.agent_markers.publish(connection, markers);
    }
    pub(crate) fn retire_accessible_agent_markers(&mut self, connection: u64) {
        self.agent_markers
            .retain(|source| source.connection != connection);
    }
    pub(crate) fn retain_accessible_agent_markers(
        &mut self,
        keep: impl FnMut(&AccessibleAgentMarker) -> bool,
    ) -> bool {
        self.agent_markers.retain(keep)
    }
    pub(crate) fn take_accessible_agent_marker_activation(&self) -> Option<AccessibleAgentMarker> {
        self.agent_markers.take_activation()
    }
    pub(crate) fn accessible_agent_marker_request_is_inert(&self, target: &str) -> bool {
        target.starts_with(TARGET_PREFIX)
            && (self.agent_quick_composer.is_open() || !self.agent_markers.actionable(target))
    }
    pub(crate) fn open_agent_quick_composer(
        &mut self,
        owner: crate::shell::agent_quick_composer::ComposerOwner,
        label: &str,
        origin: crate::shell::agent_quick_composer::AgentQuickComposerOrigin,
    ) -> bool {
        let Some(broker) = self.shortcut_triggers.broker() else {
            return false;
        };
        if !self.agent_actions.entry(owner.clone()).or_default().open(
            crate::shell::certified_action_session::ActionSessionSource::new(broker),
            owner.clone(),
        ) {
            return false;
        }
        let viewport = self.overlay_area();
        self.agent_quick_composer
            .open(owner, label, origin, viewport);
        true
    }
    pub(crate) fn agent_marker_pointer_at(&self, point: (f64, f64)) -> bool {
        !self.agent_quick_composer.is_open() && self.agent_markers.pointer_at(point)
    }

    /// Public fixture for the offscreen shots driver: the agent composer
    /// beside a selection, idle, typed, answered or following a run. No
    /// session, agent or task is created.
    /// # Errors
    /// Requires the debug environment gate and a known fixture phase.
    #[cfg(debug_assertions)]
    pub fn seed_agent_composer_for_shots(&mut self, phase: &str) -> Result<(), &'static str> {
        use crate::shell::agent_quick_composer::{
            AgentQuickComposerOrigin, ComposerOwner, ComposerRun, ComposerTask, ComposerTurn,
            RunPhase,
        };
        use crate::shell::i18n::t;
        if std::env::var_os("TILO_UI_DEBUG").is_none() {
            return Err("agent composer shots require TILO_UI_DEBUG and a debug build");
        }
        let viewport = self.overlay_area();
        // Where a selected line of a document sits in the shots desktop.
        let selection = vello::kurbo::Rect::new(
            viewport.x0 + viewport.width() * 0.42,
            viewport.y0 + viewport.height() * 0.34,
            viewport.x0 + viewport.width() * 0.58,
            viewport.y0 + viewport.height() * 0.34 + 44.0,
        );
        self.agent_quick_composer.open(
            ComposerOwner {
                room: "shots-room".into(),
                agent: "scribe".into(),
                person: "shots-person".into(),
                identity_revision: 0,
            },
            "scribe",
            AgentQuickComposerOrigin {
                marker: selection,
                protected: selection,
                context: t("agent-quick-shots-context"),
            },
            viewport,
        );
        self.agent_quick_composer.set_about_selection(true);
        let asked = |key: &str| ComposerTurn {
            agent: false,
            text: t(key),
        };
        let run = |phase| ComposerRun {
            task: ComposerTask {
                task_id: "task:shots".into(),
                run_epoch: 1,
                revision: 0,
                label: t("agent-quick-shots-task"),
            },
            phase,
        };
        let (draft, conversation, followed) = match phase {
            "agent-composer" => (String::new(), Vec::new(), None),
            "agent-composer-typed" => (t("agent-quick-shots-draft"), Vec::new(), None),
            "agent-composer-answer" => (
                String::new(),
                vec![
                    asked("agent-quick-shots-question"),
                    ComposerTurn {
                        agent: true,
                        text: t("agent-quick-shots-answer"),
                    },
                ],
                None,
            ),
            "agent-composer-working" => (
                t("agent-quick-shots-steer"),
                vec![asked("agent-quick-shots-request")],
                Some(run(RunPhase::Working)),
            ),
            _ => return Err("unknown agent composer phase"),
        };
        self.agent_quick_composer
            .stage_for_shots(&draft, conversation, followed);
        Ok(())
    }

    pub(crate) fn agent_quick_composer_open(&self) -> bool {
        self.agent_quick_composer.is_open()
    }
    pub(crate) fn agent_quick_composer_mut(
        &mut self,
    ) -> &mut crate::shell::agent_quick_composer::AgentQuickComposer {
        &mut self.agent_quick_composer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker() -> AccessibleAgentMarker {
        AccessibleAgentMarker {
            connection: 7,
            surface: 11,
            app_id: "so.tilo.chat".into(),
            room: "room".into(),
            agent: "tide".into(),
            interaction: tilo_actions::SemanticInteraction {
                version: tilo_actions::INTERACTION_VERSION,
                invocation: tilo_actions::InvocationId::new(),
                sequence: 1,
                actor: tilo_actions::InteractionActor::new(
                    "opaque-certified-software",
                    tilo_actions::ActorKind::Agent,
                ),
                phase: tilo_actions::InteractionPhase::Applying,
                action: tilo_actions::InteractionAction {
                    id: "rename".into(),
                    label: "Rename".into(),
                    app: None,
                },
                resource: tilo_actions::ActionResource {
                    id: "category".into(),
                    kind: "chat.category".into(),
                    label: "Category".into(),
                },
                anchor: tilo_actions::InteractionAnchor::Resource,
                group: None,
            },
            marker: Rect::new(100.0, 100.0, 132.0, 132.0),
            locus: Rect::new(90.0, 90.0, 210.0, 150.0),
            protected: Rect::new(90.0, 90.0, 210.0, 180.0),
            label: "Tide".into(),
            description: "Renaming Category".into(),
            published_at: Instant::now(),
        }
    }

    #[test]
    fn empty_painted_frames_do_not_rebuild_the_control_tree() {
        let mut markers = AgentMarkers::default();
        markers.publish(7, Vec::new());
        assert!(!markers.take_structure_dirty());
        let source = marker();
        markers.publish(source.connection, vec![source.clone()]);
        assert!(markers.take_structure_dirty());
        markers.publish(8, Vec::new());
        assert!(
            !markers.take_structure_dirty(),
            "another idle source leaves live controls mounted"
        );
        markers.publish(source.connection, Vec::new());
        assert!(
            markers.take_structure_dirty(),
            "removing a live marker still rebuilds"
        );
        markers.publish(source.connection, Vec::new());
        assert!(!markers.take_structure_dirty());
    }

    #[test]
    fn replacing_a_painted_marker_retires_its_pending_activation() {
        let source = marker();
        let target = source.action_target();
        let mut markers = AgentMarkers::default();
        let mut presence = PresenceCtx::default();
        presence.set_participant_kind(
            ui::permissions::UserId(source.interaction.actor.id.clone()),
            ui::presence::ParticipantKind::Agent,
        );
        let mut doc = automerge::AutoCommit::new();
        markers.publish(source.connection, vec![source.clone()]);
        markers.refresh(&presence, false);
        markers.semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        );
        let mut replacement = source.clone();
        replacement.interaction.sequence += 1;
        replacement.marker = Rect::new(140.0, 100.0, 172.0, 132.0);
        markers.publish(source.connection, vec![replacement.clone()]);
        markers.refresh(&presence, false);
        assert!(markers.take_activation().is_none());
        markers.semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        );
        let activated = markers
            .take_activation()
            .expect("the current marker activates");
        assert!(activated.same_publication(&replacement));
        assert!(!activated.same_publication(&source));
        presence.set_participant_kind(
            ui::permissions::UserId(source.interaction.actor.id),
            ui::presence::ParticipantKind::Person,
        );
        markers.refresh(&presence, false);
        markers.semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        );
        assert!(
            markers.take_activation().is_none(),
            "revoked identity cannot activate"
        );
    }

    #[test]
    fn live_frame_restatement_keeps_activation_but_empty_roster_revokes_it() {
        let source = marker();
        let target = source.action_target();
        let mut markers = AgentMarkers::default();
        let mut presence = PresenceCtx::default();
        let user = ui::permissions::UserId(source.interaction.actor.id.clone());
        presence.set_participant_kind(user, ui::presence::ParticipantKind::Agent);
        markers.publish(source.connection, vec![source.clone()]);
        markers.refresh(&presence, false);
        let mut doc = automerge::AutoCommit::new();
        markers.semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        );
        let mut restated = source.clone();
        restated.published_at = Instant::now();
        markers.publish(source.connection, vec![restated]);
        assert!(
            markers.take_activation().is_some(),
            "an identical painted frame doesn't eat the click"
        );
        markers.semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        );
        presence.unregister_peer(&source.interaction.actor.id);
        markers.refresh(&presence, false);
        assert!(markers.take_activation().is_none());
        assert!(!markers.actionable(&target));
    }

    #[test]
    fn a_non_agent_interaction_cannot_publish_an_agent_control() {
        let mut source = marker();
        source.interaction.actor.kind = tilo_actions::ActorKind::App;
        let target = source.action_target();
        let mut markers = AgentMarkers::default();
        let mut presence = PresenceCtx::default();
        presence.set_participant_kind(
            ui::permissions::UserId(source.interaction.actor.id.clone()),
            ui::presence::ParticipantKind::Agent,
        );
        markers.publish(source.connection, vec![source]);
        markers.refresh(&presence, false);
        assert!(!markers.actionable(&target));
        assert_eq!(markers.children().count(), 0);
    }

    #[test]
    fn marker_semantic_activation_requires_roster_kind_and_never_resurrects_expired_geometry() {
        let mut markers = AgentMarkers::default();
        let source = marker();
        let target = source.action_target();
        markers.publish(7, vec![source.clone()]);
        let mut presence = PresenceCtx::default();
        markers.refresh(&presence, false);
        let mut doc = automerge::AutoCommit::new();
        markers.semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        );
        assert!(
            markers.take_activation().is_none(),
            "a software-looking label does not grant Agent identity"
        );
        for kind in [
            ui::presence::ParticipantKind::App,
            ui::presence::ParticipantKind::Shortcut,
        ] {
            presence.set_participant_kind(
                ui::permissions::UserId(source.interaction.actor.id.clone()),
                kind,
            );
            markers.refresh(&presence, false);
            markers.semantic_action(
                &target,
                &ui::a11y::Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            );
            assert!(
                markers.take_activation().is_none(),
                "software drawing style is not certified Agent membership"
            );
        }
        presence.set_participant_kind(
            ui::permissions::UserId(source.interaction.actor.id.clone()),
            ui::presence::ParticipantKind::Agent,
        );
        markers.refresh(&presence, false);
        markers.semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        );
        assert_eq!(
            markers
                .take_activation()
                .expect("the ordinary Button activated")
                .interaction
                .invocation,
            source.interaction.invocation
        );
        markers.refresh(&presence, true);
        markers.semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        );
        assert!(
            markers.take_activation().is_none(),
            "an open composer owns input"
        );
        let expired = AccessibleAgentMarker {
            published_at: Instant::now()
                .checked_sub(MAX_AGE)
                .and_then(|instant| instant.checked_sub(Duration::from_secs(1)))
                .expect("expired marker timestamp must be representable"),
            ..source
        };
        markers.publish(7, vec![expired]);
        markers.refresh(&presence, false);
        markers.semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        );
        assert!(
            markers.take_activation().is_none(),
            "old bounds are never a pointer fallback"
        );
    }
}
