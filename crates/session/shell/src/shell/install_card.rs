//! The card a tab shows while the app another device has open in it is not
//! installed here but can be: the window's name, why it is not open, and one
//! button that installs the app into this tab. Pressing it is the consent;
//! nothing installs without it.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::Theme;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::{
    AppKind, AppModel, AppWidget, EmptyState, EventCtx, IconDescriptor, ModelPath, Widget,
};

use crate::shell::state::TabId;

pub struct InstallCard {
    tab: TabId,
    title: String,
    card: EmptyState,
}

impl InstallCard {
    /// The card for `tab`, whose window is called `title` and whose app the
    /// catalogue offers as `display_name`.
    pub fn new(tab: TabId, title: &str, display_name: &str) -> Self {
        let card = EmptyState::new(title)
            .with_message(crate::shell::i18n::t("peer-window-not-installed"))
            .with_action(crate::shell::i18n::t_args(
                "peer-window-install",
                &[("app", display_name.to_owned().into())],
            ))
            .with_action_id(format!("install-app:{tab}"));
        Self {
            tab,
            title: title.to_owned(),
            card,
        }
    }

    /// Hand a pressed install button to the shell's next reconciliation.
    fn forward_install(&mut self) {
        if self.card.take_action() {
            crate::shell::peer_windows::request_install(self.tab);
        }
    }

    #[cfg(test)]
    pub(crate) fn press_for_test(&mut self) {
        let mut state = automerge::AutoCommit::new();
        let path = automerge::ROOT;
        self.perform_action(&ui::a11y::Action::Activate, &mut state, &path);
    }
}

impl Widget for InstallCard {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.card.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.card.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.card
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.card.handle_event(event, ctx);
        self.forward_install();
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.card.cursor_shape(point)
    }

    fn is_focusable(&self) -> bool {
        self.card.is_focusable()
    }

    fn set_focus(&mut self, focused: bool) {
        self.card.set_focus(focused);
    }

    fn is_focused(&self) -> bool {
        self.card.is_focused()
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let performed = self.card.perform_action(action, state, path);
        self.forward_install();
        performed
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let performed = self
            .card
            .perform_semantic_action(target, action, state, path);
        self.forward_install();
        performed
    }

    fn name(&self) -> &'static str {
        "InstallCard"
    }

    fn get_rect(&self) -> Rect {
        self.card.get_rect()
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.card.accessibility()
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        self.card.synthetic_accessibility_rect(path)
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        self.card.focus_synthetic_accessibility(path)
    }
}

impl AppWidget for InstallCard {
    fn uses_host_record(&self) -> bool {
        false
    }

    fn app_kind(&self) -> AppKind {
        AppKind::Settings
    }

    fn app_title(&self) -> &str {
        &self.title
    }

    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("󰏗")
    }

    fn permission_hint(&self) -> Option<AppKind> {
        None
    }
}
