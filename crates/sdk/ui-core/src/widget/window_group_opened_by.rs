//! The agent's mark in the window's bar ([`super::super::tabs`]'s status chip)
//! as a stop of the window's keyboard order: laid out right after the bar, so
//! Tab reaches it after the tab list and before the app. Focus, the focus
//! ring the bar paints on the mark, the accessibility node and the press all
//! name the same control, so a keyboard-only person reaches what a pointer
//! does.

use crate::a11y::AccessNode;
use crate::widget::Widget;
use taffy::{NodeId, TaffyTree};
use vello::kurbo::Rect;

/// The window's keyboard stop on the agent's mark: where the bar painted it
/// and how assistive technology reads it, whether the keyboard is on it, and
/// a press waiting for the window.
#[derive(Debug, Default)]
pub(crate) struct OpenedByStop {
    /// Where the bar painted the mark, and its node; nothing while the bar
    /// shows none.
    shown: Option<(Rect, AccessNode)>,
    focused: bool,
    pressed: bool,
    /// The keyboard arrived or left: the window paints again.
    dirty: bool,
}

impl OpenedByStop {
    /// Stand on the mark where the bar last painted it, or on nothing.
    pub(super) fn place(&mut self, shown: Option<(Rect, AccessNode)>) {
        self.shown = shown;
    }

    /// Whether the keyboard is on the mark.
    pub(super) fn focused(&self) -> bool {
        self.focused
    }

    /// Take the press the keyboard or assistive technology made on it.
    pub(super) fn take_press(&mut self) -> bool {
        std::mem::take(&mut self.pressed)
    }

    pub(super) fn needs_repaint(&self) -> bool {
        self.dirty
    }

    pub(super) fn mark_painted(&mut self) {
        self.dirty = false;
    }
}

impl Widget for OpenedByStop {
    /// The window places it where the bar painted the mark: as a leaf of its
    /// own it takes no room of the layout's.
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &crate::theme::Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use crate::widget::TaffyTreeExt;
        let style = taffy::Style {
            position: taffy::Position::Absolute,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn get_rect(&self) -> Rect {
        self.shown.as_ref().map_or(Rect::ZERO, |(rect, _)| *rect)
    }

    fn is_focusable(&self) -> bool {
        self.shown.is_some()
    }

    fn set_focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.dirty = true;
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let (_, node) = self.shown.as_ref()?;
        Some(
            node.clone().with_state(
                crate::a11y::AccessState::new()
                    .focusable(true)
                    .focused(self.focused),
            ),
        )
    }

    /// Enter or Space on it, or an assistive technology activating its node,
    /// opens the run as a pointer's press does.
    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _root: &crate::widget::ModelPath,
    ) -> bool {
        if self.shown.is_none() || !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        self.pressed = true;
        self.dirty = true;
        true
    }

    fn name(&self) -> &'static str {
        "OpenedByStop"
    }
}

impl super::WindowGroup {
    /// Mark tab `id` as opened for its person by the agent run `run`, or
    /// clear the mark: the bar names the agent while the tab is in front.
    /// `true` when that changed what the window shows.
    pub fn set_tab_opened_by(
        &mut self,
        id: u64,
        run: Option<tilo_desktop_model::ownership::OpenedBy>,
    ) -> bool {
        self.bar.set_opened_by(id, run)
    }

    /// A press on the agent's mark in the bar, once: the tab and the run
    /// to open in History.
    pub fn take_opened_by_press(&mut self) -> Option<(u64, String)> {
        self.bar.take_opened_by_press()
    }

    /// Which of the window's laid out children the active tab's content is:
    /// after the bar and the keyboard's stop on its agent's mark while the
    /// bar shows, else the first.
    pub(super) fn tile_child_index(&self) -> usize {
        if self.navigation_bar_visible() { 2 } else { 0 }
    }

    /// The keyboard's stop on the agent's mark as a node of the window's,
    /// right after the bar's (`nodes`' first, when `bar` shows): the stop
    /// after the bar's tab list.
    pub(super) fn build_opened_by_stop(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &crate::theme::Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        nodes: &mut Vec<NodeId>,
        bar: bool,
    ) {
        if bar {
            let stop = self.opened_by.build_layout(taffy, theme, node_map);
            nodes.insert(1, stop);
        }
    }

    /// Whether what is typed is the agent's mark's: the keyboard stands on
    /// it, so Enter and Space press it and the app under it hears nothing.
    pub(super) fn opened_by_takes_keys(&self, event: &winit::event::WindowEvent) -> bool {
        self.opened_by.focused()
            && matches!(
                event,
                winit::event::WindowEvent::KeyboardInput { .. } | winit::event::WindowEvent::Ime(_)
            )
    }

    /// What the bar says over everything while the pointer rests on it or
    /// the keyboard is on it: who holds the tab under the pointer, and in
    /// which run the agent opened the window. Each keeps to the part of the
    /// screen the window shows in. Then the keyboard's stop stands where the
    /// agent's mark was painted.
    pub(super) fn draw_bar_tips(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        tr: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        let band = self.bar.get_rect().inflate(0.0, 6.0);
        let room = crate::widget::tooltip::on_screen(presence, band);
        self.bar.draw_holder_chip(scene, transform, tr, theme, room);
        let room = crate::widget::tooltip::on_screen(presence, self.get_rect());
        self.bar
            .draw_opened_by_tip(scene, transform, tr, theme, room);
        self.opened_by.place(self.bar.opened_by_stop());
    }
}
