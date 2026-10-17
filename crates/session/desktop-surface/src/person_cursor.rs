//! Semantic controls attached to real, certified remote person pointers.
//! Hosts supply only pointers painted in the current workspace. This module
//! never invents cursor locations or treats display labels as identities.

use std::collections::HashMap;
use ui::widget::{Button, Decorator, Widget};
use ui::{LocalState, Theme};
use vello::kurbo::{Affine, Point, Rect};

#[derive(Clone, Debug, PartialEq)]
pub struct PersonCursor {
    pub workspace: String,
    /// Canonical lowercase Ed25519 user-root key, never a device endpoint.
    pub person_key: String,
    pub label: String,
    pub marker: Rect,
}

impl PersonCursor {
    pub fn action_target(&self) -> String {
        format!("participant:{}:chat", self.person_key)
    }

    pub fn valid(&self) -> bool {
        uuid::Uuid::parse_str(&self.workspace).is_ok_and(|id| !id.is_nil())
            && self.person_key.len() == 64
            && self
                .person_key
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            && self.marker.is_finite()
            && self.marker.width() > 0.0
            && self.marker.height() > 0.0
    }
}

struct CursorControl {
    source: PersonCursor,
    selected: LocalState<PersonCursor>,
    button: Button,
    origin: Point,
    pointer_position: Option<Point>,
    message_label: String,
}

impl Decorator for CursorControl {
    type Inner = Button;
    fn inner(&self) -> &Button {
        &self.button
    }
    fn inner_mut(&mut self) -> &mut Button {
        &mut self.button
    }
    fn name(&self) -> &'static str {
        "PersonCursorChat"
    }
    // The real cursor is already painted. Only the shared message button's
    // hover/focus face is revealed, not another cursor or an always-on badge.
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::PresenceCtx,
    ) {
        let visible = self.button.is_enabled()
            && (self
                .button
                .cursor_shape(
                    self.pointer()
                        .unwrap_or(Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY)),
                )
                .is_some()
                || self.button.is_focused());
        if visible {
            self.button.draw(scene, transform, text, theme, presence);
        } else {
            // Advance the shared control's hover/theme cache even while its
            // face is hidden; otherwise a hover exit requests frames forever.
            let mut hidden = vello::Scene::new();
            self.button
                .draw(&mut hidden, transform, text, theme, presence);
        }
    }
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.button.accessibility().map(|node| {
            node.with_name(format!("{} — {}", self.message_label, self.source.label))
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

impl CursorControl {
    fn pointer(&self) -> Option<Point> {
        self.pointer_position
    }
}

/// A platform Button for every current person cursor: focus, keyboard and
/// accessibility all invoke the same host-revalidated intention.
pub struct PersonCursors {
    // Frame node maps retain raw widget addresses while the pointer animates.
    #[expect(
        clippy::vec_box,
        reason = "cursor movement must preserve registered widget addresses"
    )]
    controls: Vec<Box<CursorControl>>,
    // Keep raw node-map registrations valid until the host clears its layout.
    #[expect(
        clippy::vec_box,
        reason = "retired registrations must keep their addresses"
    )]
    retired: Vec<Box<CursorControl>>,
    nodes: Vec<taffy::NodeId>,
    activation: LocalState<Option<PersonCursor>>,
    dirty: bool,
    suppressed: bool,
}

impl Default for PersonCursors {
    fn default() -> Self {
        Self {
            controls: Vec::new(),
            retired: Vec::new(),
            nodes: Vec::new(),
            activation: LocalState::new(None),
            dirty: false,
            suppressed: false,
        }
    }
}

impl PersonCursors {
    pub fn publish(&mut self, cursors: Vec<PersonCursor>, message_label: &str) {
        let previous: Vec<_> = self
            .controls
            .iter()
            .map(|control| control.source.clone())
            .collect();
        let mut old = std::mem::take(&mut self.controls);
        for source in cursors.into_iter().filter(PersonCursor::valid).take(128) {
            if self.controls.iter().any(|control| {
                control.source.workspace == source.workspace
                    && control.source.person_key == source.person_key
            }) {
                continue;
            }
            if let Some(index) = old.iter().position(|control| {
                control.source.workspace == source.workspace
                    && control.source.person_key == source.person_key
            }) {
                let mut control = old.remove(index);
                if control.source != source {
                    control.selected.set(source.clone());
                    control.source = source;
                }
                control.message_label = message_label.into();
                self.controls.push(control);
            } else {
                let activation = self.activation.clone();
                let selected = LocalState::new(source.clone());
                let clicked = selected.clone();
                let label = format!("{} — {}", message_label, source.label);
                let button = Button::icon_only(
                    &source.action_target(),
                    ui::icons::Icon::MessageSquare,
                    &label,
                )
                .on_click(move || activation.set(Some(clicked.get())));
                self.controls.push(Box::new(CursorControl {
                    source,
                    selected,
                    button,
                    origin: Point::ZERO,
                    pointer_position: None,
                    message_label: message_label.into(),
                }));
            }
        }
        for control in &mut old {
            control.button.set_enabled(false);
        }
        self.retired.extend(old);
        self.dirty |= previous
            != self
                .controls
                .iter()
                .map(|control| control.source.clone())
                .collect::<Vec<_>>();
        self.refresh(self.suppressed);
    }
    pub fn refresh(&mut self, suppressed: bool) {
        self.suppressed = suppressed;
        for control in &mut self.controls {
            control.button.set_enabled(!suppressed);
        }
    }
    pub fn actionable(&self, target: &str) -> bool {
        !self.suppressed
            && self
                .controls
                .iter()
                .any(|control| control.source.action_target() == target)
    }
    pub fn pointer_at(&self, point: Point) -> bool {
        !self.suppressed
            && self.controls.iter().any(|control| {
                control.button.is_enabled() && control.button.cursor_shape(point).is_some()
            })
    }
    pub fn take_activation(&self) -> Option<PersonCursor> {
        let selected = self.activation.get();
        if selected.is_some() {
            self.activation.set(None);
        }
        selected.filter(|source| {
            !self.suppressed
                && self
                    .controls
                    .iter()
                    .any(|control| &control.source == source)
        })
    }
    pub fn semantic_action(
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
    pub fn build(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
        origin: Point,
    ) -> Vec<taffy::NodeId> {
        // The host has discarded the preceding layout's node map.
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
    pub fn apply(&mut self, tree: &taffy::TaffyTree<()>, origin: Point) {
        for (control, node) in self.controls.iter_mut().zip(&self.nodes) {
            Widget::apply_layout(control.as_mut(), tree, *node, origin);
        }
    }
    pub fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::PresenceCtx,
    ) {
        for control in &mut self.controls {
            Widget::draw(control.as_mut(), scene, transform, text, theme, presence);
        }
    }
    pub fn route_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        for (control, node) in self.controls.iter_mut().zip(&self.nodes) {
            if let winit::event::WindowEvent::CursorMoved { position, .. } = event {
                control.pointer_position = Some(ctx.logical(position));
            }
            if matches!(event, winit::event::WindowEvent::CursorLeft { .. }) {
                control.pointer_position = None;
            }
            Widget::handle_event(control.as_mut(), event, &mut ctx.reborrow(ctx.path, *node));
            if *ctx.handled {
                return;
            }
        }
    }
    pub fn children(&self) -> impl Iterator<Item = &dyn Widget> {
        self.controls
            .iter()
            .map(|control| control.as_ref() as &dyn Widget)
    }
    pub fn children_mut(&mut self) -> impl Iterator<Item = &mut dyn Widget> {
        self.controls
            .iter_mut()
            .map(|control| control.as_mut() as &mut dyn Widget)
    }
    pub fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cursor(workspace: &str, label: &str) -> PersonCursor {
        PersonCursor {
            workspace: workspace.into(),
            person_key: "ab".repeat(32),
            label: label.into(),
            marker: Rect::new(10.0, 20.0, 42.0, 52.0),
        }
    }
    #[test]
    fn semantics_follow_key_not_name_or_pointer_and_departures_retire_controls() {
        let room = "11111111-1111-4111-8111-111111111111";
        let source = cursor(room, "Ada");
        let target = source.action_target();
        let mut cursors = PersonCursors::default();
        cursors.publish(vec![source.clone()], "Message");
        assert!(cursors.actionable(&target));
        let mut doc = automerge::AutoCommit::new();
        assert!(cursors.semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert_eq!(
            cursors.take_activation(),
            Some(source.clone()),
            "semantic activation uses the shared Button callback"
        );
        cursors.refresh(true);
        assert!(cursors.semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert!(
            cursors.take_activation().is_none(),
            "modal suppression cannot queue a later activation"
        );
        cursors.refresh(false);
        let mut renamed = cursor(room, "Other label");
        renamed.marker = Rect::new(30.0, 40.0, 62.0, 72.0);
        assert_eq!(target, renamed.action_target());
        cursors.activation.set(Some(source));
        cursors.publish(vec![renamed], "Message");
        assert!(
            cursors.take_activation().is_none(),
            "stale publication cannot activate"
        );
        cursors.publish(Vec::new(), "Message");
        assert!(!cursors.actionable(&target));
        assert_eq!(cursors.children().count(), 0);
    }
    #[test]
    fn publication_preserves_registered_addresses_until_layout_rebuild() {
        let mut cursors = PersonCursors::default();
        let mut source = cursor("11111111-1111-4111-8111-111111111111", "Ada");
        cursors.publish(vec![source.clone()], "Message");
        let address = &raw const *cursors.controls[0];
        source.marker = source.marker + vello::kurbo::Vec2::new(2.0, 3.0);
        cursors.publish(vec![source], "Message");
        assert_eq!(address, &raw const *cursors.controls[0]);
        cursors.publish(Vec::new(), "Message");
        assert_eq!(cursors.retired.len(), 1);
        assert_eq!(address, &raw const *cursors.retired[0]);
        assert!(!cursors.retired[0].button.is_enabled());
        let mut tree = taffy::TaffyTree::new();
        let mut nodes = HashMap::new();
        cursors.build(&mut tree, &Theme::default(), &mut nodes, Point::ZERO);
        assert!(cursors.retired.is_empty());
    }

    #[test]
    fn invalid_room_or_device_handle_cannot_be_a_person_target() {
        let mut source = cursor("not-a-room", "Ada");
        assert!(!source.valid());
        source.workspace = "11111111-1111-4111-8111-111111111111".into();
        source.person_key = "peer@4".into();
        assert!(!source.valid());
    }
}
