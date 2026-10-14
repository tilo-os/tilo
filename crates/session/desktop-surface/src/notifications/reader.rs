//! A viewer-local reading page using the toolkit's painted scroll region.
use super::history::source_label;
use super::{CenterLayout, Notification};
use crate::i18n::t;
use ui::TextRole;
use ui::a11y::{AccessNode, AccessState, Role};
use ui::num::f32_of_f64;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, ScrollRegion};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::WindowEvent;

#[derive(Debug)]
pub(super) struct NotificationReader {
    pub id: u64,
    region: ScrollRegion,
    theme: Option<Box<Theme>>,
    back_focused: bool,
    /// Focus rings show once the keyboard (or assistive technology) moves
    /// focus, never for a page the pointer opened.
    focus_visible: bool,
}

impl NotificationReader {
    pub fn new(id: u64, focus_visible: bool) -> Self {
        let mut region = ScrollRegion::vertical();
        region.set_focused(true);
        Self {
            id,
            region,
            theme: None,
            back_focused: false,
            focus_visible,
        }
    }

    pub fn accessibility(&self, notification: &Notification, layout: &CenterLayout) -> AccessNode {
        let body = if notification.show_preview {
            notification.body.clone()
        } else {
            t("notification-preview-hidden")
        };
        AccessNode::new(Role::Dialog)
            .with_name(notification.title.clone())
            .with_state(AccessState::new().modal(true))
            .with_bounds(layout.panel())
            .with_children(vec![
                AccessNode::new(Role::Button)
                    .with_name(t("notification-center-back-to-list"))
                    .with_bounds(layout.reader_back())
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.back_focused),
                    ),
                AccessNode::new(Role::Group)
                    .with_name(notification.title.clone())
                    .with_description(body)
                    .with_bounds(layout.reading_viewport())
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(!self.back_focused),
                    ),
            ])
    }

    pub fn focus(&mut self, path: &[usize]) -> bool {
        match path {
            [0] => self.back_focused = true,
            [1] => self.back_focused = false,
            _ => return false,
        }
        self.focus_visible = true;
        self.region.set_focused(!self.back_focused);
        true
    }

    pub fn toggle_focus(&mut self) {
        self.focus_visible = true;
        self.back_focused = !self.back_focused;
        self.region.set_focused(!self.back_focused);
    }

    pub fn key(&mut self, key: &winit::keyboard::Key) -> bool {
        use ui::widget::PrimaryScrollAction;
        use winit::keyboard::{Key, NamedKey};
        if self.back_focused {
            return false;
        }
        let action = match key {
            Key::Named(NamedKey::ArrowDown) => PrimaryScrollAction::Line(1),
            Key::Named(NamedKey::ArrowUp) => PrimaryScrollAction::Line(-1),
            Key::Named(NamedKey::PageDown) => PrimaryScrollAction::Page(1),
            Key::Named(NamedKey::PageUp) => PrimaryScrollAction::Page(-1),
            Key::Named(NamedKey::Home) => PrimaryScrollAction::Start,
            Key::Named(NamedKey::End) => PrimaryScrollAction::End,
            _ => return false,
        };
        matches!(
            self.apply_scroll(action),
            ui::widget::PrimaryScrollResult::Changed(_)
        )
    }

    /// Whether focus is drawn: once the keyboard has moved it.
    #[cfg(test)]
    pub fn focus_visible(&self) -> bool {
        self.focus_visible
    }

    pub fn activate_back(&self) -> bool {
        self.back_focused
    }

    pub fn event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) -> bool {
        let Some(theme) = &self.theme else {
            return false;
        };
        self.region.handle_event(event, ctx, theme)
    }

    pub fn scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        self.region.primary_vertical_scroll()
    }

    pub fn apply_scroll(
        &mut self,
        request: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        self.region.apply_primary_vertical_scroll(request)
    }

    pub fn draw(
        &mut self,
        notification: &Notification,
        layout: &CenterLayout,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        cursor: Option<Point>,
    ) {
        if self.theme.as_deref() != Some(theme) {
            self.theme = Some(Box::new(theme.clone()));
        }
        Button::paint(
            scene,
            transform,
            text,
            theme,
            // "‹ Notifications": the way back names the list it returns to.
            &ButtonFace::new(layout.reader_back(), &t("notification-center-title"))
                .role(ui::widget::ButtonRole::Plain)
                .icon(ui::icons::Icon::ChevronLeft)
                .accessible_name(&t("notification-center-back-to-list"))
                .hovered(cursor.is_some_and(|p| layout.reader_back().contains(p)))
                .focused(self.back_focused && self.focus_visible),
        );
        let viewport = layout.reading_viewport();
        self.region.set_viewport(viewport, theme);
        let gap = f64::from(theme.tokens.spacing.s4);
        let title_style = theme.text(TextRole::Heading);
        let source_style = theme.text(TextRole::Caption);
        let body_style = theme.text(TextRole::Body);
        let source = source_label(notification).unwrap_or_default();
        let hidden = (!notification.show_preview).then(|| t("notification-preview-hidden"));
        let body = hidden.as_deref().unwrap_or(&notification.body);
        // A notice with no source has no source line, nor its step.
        let source_gap = if source.is_empty() { 0.0 } else { gap };
        // The overflow lane changes wrap width. Measure again after resolving
        // it so neither the last line nor the scrollbar clips the document.
        let mut title_h = 0.0;
        let mut source_h = 0.0;
        for _ in 0..2 {
            let width = f32_of_f64((self.region.content_rect().width() - 2.0 * gap).max(1.0));
            title_h = f64::from(
                text.measure_style(&notification.title, Some(width), &title_style)
                    .1,
            );
            source_h = if source.is_empty() {
                0.0
            } else {
                f64::from(text.measure_style(&source, Some(width), &source_style).1)
            };
            let body_h = f64::from(text.measure_style(body, Some(width), &body_style).1);
            self.region.set_content_size(
                viewport.width(),
                title_h + source_h + body_h + 3.0 * gap + source_gap,
            );
        }
        let width = f32_of_f64((self.region.content_rect().width() - 2.0 * gap).max(1.0));
        if self.focus_visible && !self.back_focused {
            ui::widget::paint_focus_ring(
                scene,
                transform,
                viewport,
                f64::from(theme.tokens.radii.control),
                theme,
            );
        }
        self.region
            .paint(scene, transform, theme, |scene, content| {
                text.draw_style(
                    scene,
                    &notification.title,
                    theme.tokens.colors.text_primary,
                    content.pre_translate((gap, gap).into()),
                    Some(width),
                    &title_style,
                );
                text.draw_style(
                    scene,
                    &source,
                    theme.tokens.colors.text_dim,
                    content.pre_translate((gap, 2.0 * gap + title_h).into()),
                    Some(width),
                    &source_style,
                );
                text.draw_style(
                    scene,
                    body,
                    theme.tokens.colors.text_primary,
                    content
                        .pre_translate((gap, 2.0 * gap + source_gap + title_h + source_h).into()),
                    Some(width),
                    &body_style,
                );
            });
    }
}
