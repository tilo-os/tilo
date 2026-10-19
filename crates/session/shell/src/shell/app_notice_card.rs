//! App status cards share the toolkit `EmptyState`: install a missing peer app,
//! get a saved window's app that this computer doesn't have, or explicitly
//! reopen a failed saved window in its existing tab.

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

/// What a card's primary action asks the shell for.
enum Primary {
    /// Install the peer window's app and open it here.
    Install,
    /// Reopen this saved window in its tab.
    Reopen(String),
    /// Open the Store on this saved window's app.
    Get(String),
}

/// The quiet Close beside a Get card's primary action.
const CLOSE_TARGET: &str = "close";

pub struct AppNoticeCard {
    tab: TabId,
    title: String,
    primary: Primary,
    card: EmptyState,
    /// The quiet Close under the primary action, for a window that can't
    /// be reopened here: painted, hit-tested and announced by these.
    close: Option<(String, ui::widget::PaintedTargets)>,
    /// The Close's hover or press changed since it was last painted.
    close_changed: bool,
}

impl AppNoticeCard {
    /// The card for `tab`, whose window is called `title` and whose app the
    /// catalogue offers as `display_name`.
    /// `detail` replaces the plain "not installed" when there is more to
    /// say: why the last install failed.
    pub fn install(tab: TabId, title: &str, display_name: &str, detail: &str) -> Self {
        let message = if detail.is_empty() {
            crate::shell::i18n::t("peer-window-not-installed")
        } else {
            detail.to_owned()
        };
        let card = EmptyState::new(title)
            .with_message(message)
            .with_action(crate::shell::i18n::t_args(
                "peer-window-install",
                &[("app", display_name.to_owned().into())],
            ))
            .with_action_id(format!("install-app:{tab}"));
        Self {
            tab,
            title: title.to_owned(),
            primary: Primary::Install,
            card,
            close: None,
            close_changed: false,
        }
    }

    /// A saved window whose app, `display_name`, this computer doesn't
    /// have: "Get {App}" opens the Store on it, and a quiet Close closes the
    /// window. Reopening can't work until it is installed, so it is not
    /// offered.
    pub fn get(tab: TabId, app_id: &str, display_name: &str) -> Self {
        let title = crate::shell::i18n::t_args(
            "shell-window-app-missing",
            &[("app", display_name.to_owned().into())],
        );
        let card = EmptyState::new(title.clone())
            .with_action(crate::shell::i18n::t_args(
                "shell-window-get-app",
                &[("app", display_name.to_owned().into())],
            ))
            .with_action_id(format!("get-app:{tab}"));
        Self {
            tab,
            title,
            primary: Primary::Get(app_id.to_owned()),
            card,
            close: Some((
                crate::shell::i18n::t("shell-window-close"),
                ui::widget::PaintedTargets::new(),
            )),
            close_changed: false,
        }
    }

    /// Reopen the exact saved window; this card owns no document itself.
    pub fn retry(tab: TabId, app_id: &str, title: &str, detail: &str) -> Self {
        Self {
            tab,
            title: title.to_owned(),
            primary: Primary::Reopen(app_id.to_owned()),
            card: EmptyState::new(title)
                .with_message(detail)
                .with_action(crate::shell::i18n::t("shell-window-retry"))
                .with_action_id(format!("reopen-window:{tab}")),
            close: None,
            close_changed: false,
        }
    }

    /// Hand an explicit card action to the shell's next reconciliation.
    fn forward_action(&mut self) {
        use crate::shell::peer_windows::{CardRequest, request};
        if self.card.take_action() {
            match &self.primary {
                Primary::Install => crate::shell::peer_windows::request_install(self.tab),
                Primary::Reopen(app_id) => request(CardRequest::Reopen, self.tab, app_id),
                Primary::Get(app_id) => request(CardRequest::Get, self.tab, app_id),
            }
        }
        let closed = self
            .close
            .as_mut()
            .is_some_and(|(_, targets)| !targets.take_activated().is_empty());
        if closed && let Primary::Get(app_id) = &self.primary {
            request(CardRequest::Close, self.tab, app_id);
        }
    }

    /// Where the quiet Close stands: centred under the primary action.
    fn close_rect(&self, text: &mut TextRenderer, theme: &Theme, label: &str) -> Rect {
        let action = self.card.action_rect();
        let style = theme.text(ui::TextRole::Body);
        let (w, h) = text.measure_style(label, None, &style);
        let width = f64::from(w) + 32.0;
        let height = (f64::from(h) + 16.0).max(32.0);
        let x0 = action.center().x - width / 2.0;
        let y0 = action.y1 + f64::from(theme.tokens.spacing.s2);
        Rect::new(x0, y0, x0 + width, y0 + height)
    }

    #[cfg(test)]
    pub(crate) fn press_close_for_test(&mut self) {
        if let Some((_, targets)) = &mut self.close {
            targets.perform_semantic_action(&ui::a11y::Action::Activate, CLOSE_TARGET);
        }
        self.forward_action();
    }

    #[cfg(test)]
    pub(crate) fn press_for_test(&mut self) {
        let mut state = automerge::AutoCommit::new();
        let path = automerge::ROOT;
        self.perform_action(
            &ui::a11y::Action::Activate,
            &[],
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
        self.close_changed = false;
        let Some(label) = self.close.as_ref().map(|(label, _)| label.clone()) else {
            return;
        };
        let rect = self.close_rect(text_renderer, theme, &label);
        let Some((_, targets)) = &mut self.close else {
            return;
        };
        targets.begin_frame();
        if self.card.action_rect().area() > 0.0 {
            targets.target(CLOSE_TARGET, rect, label.clone());
        }
        targets.end_frame();
        let interaction = targets.interaction(CLOSE_TARGET);
        ui::widget::Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ui::widget::ButtonFace::new(rect, &label)
                .role(ui::widget::ButtonRole::Plain)
                .hovered(interaction.hovered)
                .pressed(interaction.pressed),
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.card.handle_event(event, ctx);
        if !*ctx.handled
            && let Some((_, targets)) = &mut self.close
        {
            let mut handled = false;
            self.close_changed |= targets.handle_pointer_event(event, ctx.scale_factor, &mut handled);
            *ctx.handled |= handled;
        }
        self.forward_action();
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self
            .close
            .as_ref()
            .is_some_and(|(_, targets)| targets.hit(point).is_some())
        {
            return Some(winit::window::CursorIcon::Pointer);
        }
        self.card.cursor_shape(point)
    }

    fn needs_redraw(&self) -> bool {
        self.close_changed || self.card.needs_redraw()
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
        key_path: &[u64],
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let close = self
            .close
            .as_mut()
            .is_some_and(|(_, targets)| targets.perform_action(action, key_path));
        let performed = close || self.card.perform_action(action, &[], state, path);
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
        let close = self
            .close
            .as_mut()
            .is_some_and(|(_, targets)| targets.perform_semantic_action(action, target));
        let performed = close
            || self
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
        self.card.accessibility().map(|mut node| {
            if let Some((_, targets)) = &self.close {
                node.children.extend(targets.accessibility_children());
            }
            node.with_semantic_key(format!("app-notice:{}", self.tab))
        })
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
}
