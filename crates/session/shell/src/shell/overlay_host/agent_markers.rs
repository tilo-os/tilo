//! Interactive semantics for compositor-validated, actually painted markers.
//! Geometry is descriptive; activation is returned to the compositor for a
//! fresh source/room/member check before an action session opens.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ui::widget::{Button, ButtonRole, Decorator, Widget};

use crate::shell::agent_quick_composer::ComposerTask;
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
    pub task: ComposerTask,
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
            && self.task == other.task
            && self.interaction == other.interaction
            && self.marker == other.marker
            && self.locus == other.locus
            && self.protected == other.protected
    }

    fn valid(&self) -> bool {
        !self.room.is_empty()
            && !self.app_id.is_empty()
            && !self.agent.is_empty()
            && !self.task.task_id.is_empty()
            && self.task.run_epoch > 0
            && self
                .interaction
                .paint_origin
                .is_some_and(|origin| !origin.is_nil())
            && self
                .interaction
                .group
                .as_ref()
                .is_some_and(|group| group.kind == "task" && group.id == self.task.task_id)
            && self.interaction.validate().is_ok()
            && self.interaction.actor.kind == tilo_actions::ActorKind::Agent
            && [self.marker, self.locus, self.protected]
                .iter()
                .all(|rect| rect.is_finite() && rect.width() > 0.0 && rect.height() > 0.0)
    }
}

/// Button supplies ordinary focus, keyboard, accessibility and hit routing.
/// The real marker is already painted. Reveal only a shared message control
/// on hover/focus, without an idle animation or a second activity cursor.
struct MarkerControl {
    source: AccessibleAgentMarker,
    activation_source: LocalState<AccessibleAgentMarker>,
    button: Button,
    origin: Point,
    hovered: bool,
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
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.button.is_enabled() && (self.hovered || self.button.is_focused()) {
            self.button.draw(scene, transform, text, theme, presence);
        } else {
            let mut hidden = vello::Scene::new();
            self.button
                .draw(&mut hidden, transform, text, theme, presence);
        }
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        match event {
            winit::event::WindowEvent::CursorMoved { position, .. } => {
                self.hovered = self.button.cursor_shape(ctx.logical(position)).is_some();
            }
            winit::event::WindowEvent::CursorLeft { .. } => self.hovered = false,
            _ => {}
        }
        self.button.handle_event(event, ctx);
    }
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.button.accessibility().map(|node| {
            node.with_name(super::super::i18n::t_args(
                "agent-marker-message",
                &[("task", self.source.task.label.clone().into())],
            ))
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
    #[expect(clippy::vec_box, reason = "node maps retain raw widget addresses")]
    controls: Vec<Box<MarkerControl>>,
    #[expect(
        clippy::vec_box,
        reason = "retired registrations must keep their addresses"
    )]
    retired: Vec<Box<MarkerControl>>,
    nodes: Vec<taffy::NodeId>,
    activation: LocalState<Option<AccessibleAgentMarker>>,
    dirty: bool,
    review: Option<crate::shell::task_review::NativeTaskReview>,
}

impl Default for AgentMarkers {
    fn default() -> Self {
        Self {
            controls: Vec::new(),
            retired: Vec::new(),
            nodes: Vec::new(),
            activation: LocalState::new(None),
            dirty: false,
            review: None,
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
                    && control.source.task.task_id == source.task.task_id
                    && control.source.task.run_epoch == source.task.run_epoch
                    && control.source.interaction.paint_origin == source.interaction.paint_origin
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
                let mut button = Button::icon_only(
                    &source.action_target(),
                    ui::icons::Icon::MessageSquare,
                    &super::super::i18n::t("agent-quick-message"),
                )
                .role(ButtonRole::Plain)
                .with_accessible_name(&source.description)
                .on_click(move || activation.set(Some(selected.get())));
                button.set_enabled(false);
                self.controls.push(Box::new(MarkerControl {
                    source,
                    activation_source,
                    button,
                    origin: Point::ZERO,
                    hovered: false,
                }));
            }
        }
        for control in &mut previous {
            control.button.set_enabled(false);
        }
        self.retired.extend(previous);
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
        let mut live = Vec::new();
        for mut control in std::mem::take(&mut self.controls) {
            if keep(&control.source) {
                live.push(control);
            } else {
                control.button.set_enabled(false);
                self.retired.push(control);
            }
        }
        self.controls = live;
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
            control
                .button
                .cursor_shape(Point::new(point.0, point.1))
                .is_some()
                && control.source.is_fresh(Instant::now())
                && Widget::is_enabled(control.as_ref())
        })
    }

    pub(crate) fn take_activation(&self) -> Option<AccessibleAgentMarker> {
        let activation = self.activation.get();
        self.activation.set(None);
        activation.filter(|source| {
            source.is_fresh(Instant::now())
                && self.controls.iter().any(|control| {
                    control.source.same_publication(source) && Widget::is_enabled(control.as_ref())
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
        self.retired.clear();
        self.nodes = self
            .controls
            .iter_mut()
            .map(|control| {
                control.origin = origin;
                Widget::build_layout(control.as_mut(), tree, theme, nodes)
            })
            .collect();
        self.nodes.clone()
    }

    pub(crate) fn actionable(&self, target: &str) -> bool {
        self.controls.iter().any(|control| {
            control.source.action_target() == target
                && control.source.is_fresh(Instant::now())
                && Widget::is_enabled(control.as_ref())
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
            Widget::perform_action(control.as_mut(), action, &[], edits, path);
        }
        true
    }

    pub(crate) fn apply(&mut self, tree: &taffy::TaffyTree<()>, origin: Point) {
        for (control, node) in self.controls.iter_mut().zip(&self.nodes) {
            Widget::apply_layout(control.as_mut(), tree, *node, origin);
        }
    }

    pub(crate) fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for control in &mut self.controls {
            Widget::draw(control.as_mut(), scene, transform, text, theme, presence);
        }
    }

    pub(crate) fn children(&self) -> impl Iterator<Item = &dyn Widget> {
        self.controls
            .iter()
            .map(|control| control.as_ref() as &dyn Widget)
    }

    pub(crate) fn children_mut(&mut self) -> impl Iterator<Item = &mut dyn Widget> {
        self.controls
            .iter_mut()
            .map(|control| control.as_mut() as &mut dyn Widget)
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
            Widget::handle_event(control.as_mut(), event, &mut ctx.reborrow(ctx.path, *node));
            if *ctx.handled {
                return;
            }
        }
    }
}

/// Resolve only the marker's explicit task group in an authorized owner view.
/// Neither a display label nor the agent's other work can supply a target.
fn task_from_view(
    view: &tilo_desktop::activity::TaskReviewSnapshot,
    room: &str,
    person: &str,
    agent: &str,
    task_id: &str,
    original_epoch: Option<u64>,
    now: i64,
) -> Option<ComposerTask> {
    if view.room != room
        || view.actor != person
        || !(0..=2).contains(&now.checked_sub(view.sampled_at_secs)?)
    {
        return None;
    }
    let mut matches = view.tasks.iter().filter(|task| task.id == task_id);
    let task = matches.next()?;
    if matches.next().is_some() || task.route != tilo_desktop_model::WorkspaceTaskRoute::Model {
        return None;
    }
    let control = view.controls.get(&task.id)?;
    // Only a retained broker admission supplies this epoch. The current
    // assignment/status cannot erase old pixels' conversation custody, and
    // cannot supply a replacement run when the original has ended.
    if let Some(epoch) = original_epoch {
        if epoch == 0 || epoch > control.run_epoch {
            return None;
        }
        return Some(ComposerTask {
            task_id: task.id.clone(),
            run_epoch: epoch,
            revision: control
                .directions
                .last()
                .map_or(0, |direction| direction.revision),
            label: task.title.clone(),
        });
    }
    if task.assigned_agent.as_deref() != Some(agent)
        || task.status != tilo_desktop_model::WorkspaceTaskStatus::InProgress
    {
        return None;
    }
    let active = control.active_run.as_ref()?;
    if active.agent_id != agent
        || active.epoch == 0
        || active.epoch != control.run_epoch
        || original_epoch.is_some_and(|epoch| epoch != active.epoch)
    {
        return None;
    }
    Some(ComposerTask {
        task_id: task.id.clone(),
        run_epoch: active.epoch,
        revision: control
            .directions
            .last()
            .map_or(0, |direction| direction.revision),
        label: task.title.clone(),
    })
}

impl super::ShellOverlayHost {
    /// Open a painted marker after the compositor has revalidated its native source.
    pub(crate) fn open_certified_agent_marker(&mut self, marker: AccessibleAgentMarker) -> bool {
        let Some(session) = self.marker_action_session_source() else {
            return false;
        };
        let Some(identity) =
            session.certified_marker_agent(&marker.room, &marker.interaction.actor.id)
        else {
            return false;
        };
        if identity.agent_key != marker.agent {
            return false;
        }
        let Some(identity_revision) = session.identity_revision(&marker.room) else {
            return false;
        };
        drop(session);
        let Some(chrome) = self.chrome.as_ref() else {
            return false;
        };
        let person = chrome.borrow().active_local_user();
        if self
            .certified_marker_task(
                &marker.room,
                &marker.app_id,
                &marker.agent,
                &marker.interaction,
            )
            .as_ref()
            != Some(&marker.task)
        {
            return false;
        }
        let task = marker.task.clone();
        let painted_origin = (marker.app_id.clone(), marker.interaction.clone());
        let opened = self.open_agent_quick_composer(
            crate::shell::agent_quick_composer::ComposerOwner {
                room: marker.room,
                agent: identity.agent_key,
                person,
                identity_revision,
            },
            &marker.label,
            crate::shell::agent_quick_composer::AgentQuickComposerOrigin {
                marker: marker.marker,
                protected: marker.protected,
                context: marker.description,
            },
        );
        if opened {
            self.agent_quick_composer_mut().bind_painted_task(
                task,
                painted_origin.0,
                painted_origin.1,
            );
        }
        opened
    }

    pub(crate) fn attach_agent_marker_task_review(
        &mut self,
        review: crate::shell::task_review::NativeTaskReview,
    ) {
        self.agent_markers.review = Some(review);
    }

    pub(crate) fn certified_marker_task(
        &self,
        room: &str,
        app: &str,
        agent: &str,
        interaction: &tilo_actions::SemanticInteraction,
    ) -> Option<ComposerTask> {
        let (task, epoch) = self.shortcut_triggers.broker()?.certified_painted_task(
            room,
            app,
            agent,
            interaction,
        )?;
        // The owner view may validate the original run, never select a new
        // epoch for old pixels, even on their first appearance after reconnect.
        self.marker_task_at_epoch(room, agent, &task, Some(epoch))
    }

    fn marker_task_at_epoch(
        &self,
        room: &str,
        agent: &str,
        task_id: &str,
        original_epoch: Option<u64>,
    ) -> Option<ComposerTask> {
        let view = self.agent_markers.review.as_ref()?.snapshot(room)?.ok()?;
        let person = self.chrome.as_ref()?.borrow().active_local_user();
        let now = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs(),
        )
        .ok()?;
        task_from_view(&view, room, &person, agent, task_id, original_epoch, now)
    }

    pub(crate) fn validate_painted_composer(&mut self) {
        if self.agent_quick_composer.about_selection() {
            return;
        }
        let Some(owner) = self.agent_quick_composer.owner().cloned() else {
            return;
        };
        let Some((app, interaction)) = self.agent_quick_composer.marker_origin().cloned() else {
            return;
        };
        let valid = self
            .certified_marker_task(&owner.room, &app, &owner.agent, &interaction)
            .is_some_and(|task| {
                self.agent_quick_composer
                    .marker_task()
                    .is_some_and(|bound| {
                        task.task_id == bound.task_id && task.run_epoch == bound.run_epoch
                    })
            });
        if !valid {
            // Keep the retained local editor, but never replace its source or authority.
            self.agent_quick_composer.marker.revoke_conversation();
        }
    }

    pub(crate) fn retain_current_agent_marker_tasks(&mut self) -> bool {
        let current: Vec<_> = self
            .agent_markers
            .controls
            .iter()
            .filter(|control| {
                self.certified_marker_task(
                    &control.source.room,
                    &control.source.app_id,
                    &control.source.agent,
                    &control.source.interaction,
                )
                .is_some_and(|task| {
                    task.task_id == control.source.task.task_id
                        && task.run_epoch == control.source.task.run_epoch
                })
            })
            .map(|control| control.source.action_target())
            .collect();
        self.agent_markers
            .retain(|marker| current.contains(&marker.action_target()))
    }

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
        self.follow_agent_composer_anchor();
    }
    fn follow_agent_composer_anchor(&mut self) {
        if !self.agent_quick_composer.is_open() {
            return;
        }
        let Some(owner) = self.agent_quick_composer.owner() else {
            return;
        };
        let Some(task) = self.agent_quick_composer.marker_task() else {
            return;
        };
        if self
            .chrome
            .as_ref()
            .is_none_or(|chrome| chrome.borrow().active_local_user() != owner.person)
            || self
                .marker_action_session_source()
                .and_then(|source| source.identity_revision(&owner.room))
                != Some(owner.identity_revision)
        {
            return;
        }
        let now = Instant::now();
        let marker = self.agent_markers.controls.iter().find(|control| {
            let source = &control.source;
            source.room == owner.room
                && source.agent == owner.agent
                && &source.task == task
                && source.is_fresh(now)
                && self
                    .certified_marker_task(
                        &source.room,
                        &source.app_id,
                        &source.agent,
                        &source.interaction,
                    )
                    .as_ref()
                    == Some(task)
        });
        if let Some(control) = marker {
            self.agent_quick_composer
                .follow_anchor(control.source.marker, control.source.protected);
        }
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

    /// Mount the production task cursor widget; fixtures are never native admission proof.
    ///
    /// # Errors
    /// Returns an error when the debug gate is closed, the phase is unknown, or the
    /// task cursor fixture cannot be seeded.
    #[cfg(debug_assertions)]
    pub fn seed_task_cursor_for_shots(&mut self, phase: &str) -> Result<(), &'static str> {
        use crate::shell::agent_quick_composer::{
            AgentQuickComposerOrigin, ComposerOwner, ComposerTask,
        };
        use crate::shell::i18n::t;
        if std::env::var_os("TILO_UI_DEBUG").is_none() {
            return Err("task cursor fixtures require the debug gate");
        }
        if !matches!(
            phase,
            "task-cursor-ready" | "task-cursor-ended" | "task-cursor-edge"
        ) {
            return Err("unknown task cursor fixture");
        }
        let viewport = self.overlay_area();
        let size = f64::from(ui::tokens::Metrics::BASE.icon_md);
        let marker = if phase == "task-cursor-edge" {
            Rect::new(
                viewport.x1 - size,
                viewport.y1 - size,
                viewport.x1,
                viewport.y1,
            )
        } else {
            Rect::new(
                viewport.x0 + viewport.width() * 0.25,
                viewport.y0 + viewport.height() * 0.45,
                viewport.x0 + viewport.width() * 0.25 + size,
                viewport.y0 + viewport.height() * 0.45 + size,
            )
        };
        let owner = ComposerOwner {
            room: "shots-room".into(),
            agent: "shots-worker".into(),
            person: "shots-person".into(),
            identity_revision: 0,
        };
        let task = ComposerTask {
            task_id: "task:shots".into(),
            run_epoch: 1,
            revision: 0,
            label: t("agent-quick-shots-task"),
        };
        self.agent_quick_composer.open(
            owner.clone(),
            &task.label,
            AgentQuickComposerOrigin {
                marker,
                protected: marker,
                context: String::new(),
            },
            viewport,
        );
        self.agent_quick_composer.bind_marker_task(task.clone());
        let live = phase != "task-cursor-ended";
        self.agent_quick_composer.project(
            &owner,
            [live, false, live],
            if live { vec![task] } else { Vec::new() },
            live,
        );
        self.agent_quick_composer
            .marker
            .stage_for_shots(&t("agent-quick-shots-steer"));
        if !live {
            self.agent_quick_composer.marker.observe_ended();
        }
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
            task: ComposerTask {
                task_id: "task:exact".into(),
                run_epoch: 1,
                revision: 0,
                label: "Exact task".into(),
            },
            interaction: tilo_actions::SemanticInteraction {
                paint_origin: Some(uuid::Uuid::from_u128(42)),
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
                group: Some(tilo_actions::ActionGroup {
                    id: "task:exact".into(),
                    label: "Untrusted label".into(),
                    kind: "task".into(),
                }),
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
    fn marker_binding_requires_exact_authorized_task_and_active_run() {
        use tilo_desktop_model::{WorkspaceTaskActiveRun, WorkspaceTaskControl};
        let task = serde_json::from_value(serde_json::json!({
            "id":"task:exact", "title":"Authorized title", "created_by":"person",
            "assigned_agent":"tide", "route":"model", "status":"in_progress",
            "created_at":0, "updated_at":0
        }))
        .expect("public task fixture");
        let control = WorkspaceTaskControl {
            run_epoch: 7,
            active_run: Some(WorkspaceTaskActiveRun {
                epoch: 7,
                agent_id: "tide".into(),
                started_at: 1,
                applied_direction_revision: 0,
                approval: None,
            }),
            ..WorkspaceTaskControl::default()
        };
        let mut view = tilo_desktop::activity::TaskReviewSnapshot {
            admissions: std::collections::BTreeMap::default(),
            room: "room".into(),
            actor: "person".into(),
            generation: 1,
            sampled_at_secs: 100,
            writable: true,
            tasks: vec![task],
            controls: std::collections::BTreeMap::from([("task:exact".into(), control)]),
        };
        let resolve =
            |view: &tilo_desktop::activity::TaskReviewSnapshot, room, person, agent, id, now| {
                task_from_view(view, room, person, agent, id, None, now)
            };
        let exact = resolve(&view, "room", "person", "tide", "task:exact", 101).unwrap();
        assert_eq!(exact.run_epoch, 7);
        assert_eq!(exact.label, "Authorized title");
        for (room, person, agent, id, now) in [
            ("other", "person", "tide", "task:exact", 101),
            ("room", "other", "tide", "task:exact", 101),
            ("room", "person", "other", "task:exact", 101),
            ("room", "person", "tide", "task:missing", 101),
            ("room", "person", "tide", "task:exact", 103),
            ("room", "person", "tide", "task:exact", 99),
        ] {
            assert!(resolve(&view, room, person, agent, id, now).is_none());
        }
        view.controls.get_mut("task:exact").unwrap().run_epoch = 8;
        assert!(resolve(&view, "room", "person", "tide", "task:exact", 101).is_none());
        view.controls
            .get_mut("task:exact")
            .unwrap()
            .active_run
            .as_mut()
            .unwrap()
            .epoch = 8;
        assert_eq!(
            task_from_view(&view, "room", "person", "tide", "task:exact", Some(7), 101)
                .unwrap()
                .run_epoch,
            7,
            "old pixels open their historical conversation, never the replacement"
        );
        assert_eq!(
            task_from_view(&view, "room", "person", "tide", "task:exact", Some(8), 101)
                .unwrap()
                .run_epoch,
            8
        );
        view.controls.get_mut("task:exact").unwrap().active_run = None;
        view.tasks[0].assigned_agent = Some("replacement-agent".into());
        view.tasks[0].status = tilo_desktop_model::WorkspaceTaskStatus::Done;
        assert!(resolve(&view, "room", "person", "tide", "task:exact", 101).is_none());
        assert_eq!(
            task_from_view(&view, "room", "person", "tide", "task:exact", Some(7), 101)
                .unwrap()
                .run_epoch,
            7,
            "historical custody does not depend on the current agent assignment"
        );
        assert!(
            task_from_view(&view, "room", "person", "tide", "task:exact", Some(9), 101).is_none()
        );
    }

    #[test]
    fn task_or_run_replacement_retires_a_pending_marker_activation() {
        let source = marker();
        let mut markers = AgentMarkers::default();
        let mut presence = PresenceCtx::default();
        presence.set_participant_kind(
            ui::permissions::UserId(source.interaction.actor.id.clone()),
            ui::presence::ParticipantKind::Agent,
        );
        markers.publish(source.connection, vec![source.clone()]);
        markers.refresh(&presence, false);
        let mut doc = automerge::AutoCommit::new();
        markers.semantic_action(
            &source.action_target(),
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        );
        assert!(markers.activation.get().is_some());
        let mut replacement = source.clone();
        replacement.task.run_epoch += 1;
        replacement.interaction.paint_origin = Some(uuid::Uuid::from_u128(43));
        markers.publish(source.connection, vec![replacement]);
        assert_eq!(
            markers.retired.len(),
            1,
            "a replacement admission must retire the held Button, not update its click source"
        );
        assert_eq!(markers.retired[0].source.task.run_epoch, 1);
        assert_eq!(markers.retired[0].activation_source.get().task.run_epoch, 1);
        markers.refresh(&presence, false);
        assert!(markers.take_activation().is_none());
        let mut mismatched = source.clone();
        mismatched.task.task_id = "task:another".into();
        markers.publish(source.connection, vec![mismatched]);
        assert_eq!(markers.children().count(), 0);
        let mut unbound = source.clone();
        unbound.interaction.group = None;
        markers.publish(source.connection, vec![unbound]);
        assert_eq!(markers.children().count(), 0);
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
