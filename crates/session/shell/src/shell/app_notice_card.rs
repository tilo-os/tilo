//! App status cards share the toolkit `EmptyState`: install a missing peer app
//! or explicitly reopen a failed saved window in its existing tab.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::Theme;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::{AppKind, AppWidget, EmptyState, EventCtx, IconDescriptor, ModelPath, Widget};

use crate::shell::state::TabId;

pub struct AppNoticeCard {
    tab: TabId,
    title: String,
    retry_app: Option<String>,
    card: EmptyState,
}

impl AppNoticeCard {
    /// The card for `tab`, whose window is called `title` and whose app the
    /// catalogue offers as `display_name`.
    pub fn install(tab: TabId, title: &str, display_name: &str) -> Self {
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
            retry_app: None,
            card,
        }
    }

    /// Reopen the exact saved window; this card owns no document itself.
    pub fn retry(tab: TabId, app_id: &str, title: &str, detail: &str) -> Self {
        Self {
            tab,
            title: title.to_owned(),
            retry_app: Some(app_id.to_owned()),
            card: EmptyState::new(title)
                .with_message(detail)
                .with_action(crate::shell::i18n::t("shell-window-retry"))
                .with_action_id(format!("reopen-window:{tab}")),
        }
    }

    /// Hand an explicit card action to the shell's next reconciliation.
    fn forward_action(&mut self) {
        if self.card.take_action() {
            if let Some(app_id) = &self.retry_app {
                crate::shell::peer_windows::request_retry(self.tab, app_id);
            } else {
                crate::shell::peer_windows::request_install(self.tab);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn press_for_test(&mut self) {
        let mut state = automerge::AutoCommit::new();
        let path = automerge::ROOT;
        self.perform_action(
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut state),
            &path,
        );
    }
}

impl Widget for AppNoticeCard {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.card.build_layout(taffy, theme, node_map);
        // The wrapper is the action owner; the shared face measures and paints.
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        // This is widget layout measurement, not TextRenderer pixel typography.
        Widget::measure(&mut self.card, known, available, text_renderer)
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
        self.forward_action();
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
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let performed = self.card.perform_action(action, state, path);
        self.forward_action();
        performed
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let performed = self
            .card
            .perform_semantic_action(target, action, state, path);
        self.forward_action();
        performed
    }

    fn test_id(&self) -> Option<&str> {
        self.card.test_id()
    }

    fn name(&self) -> &'static str {
        "AppNoticeCard"
    }

    fn get_rect(&self) -> Rect {
        self.card.get_rect()
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.card
            .accessibility()
            .map(|node| node.with_semantic_key(format!("app-notice:{}", self.tab)))
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        self.card.synthetic_accessibility_rect(path)
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        self.card.focus_synthetic_accessibility(path)
    }
}

impl AppWidget for AppNoticeCard {
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
