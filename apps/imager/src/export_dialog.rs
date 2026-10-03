//! Read-only embedded-media-profile dialog.
//!
//! The host owns the policy which produces the export text. This component
//! owns the transient modal state and every UI concern around presenting it.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::a11y::AccessNode;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{ModalDialog, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};

const TITLE: &str = "Embedded setup profile";
const MAX_WIDTH: f64 = 640.0;
const MAX_HEIGHT: f64 = 620.0;
const WIDTH_FRACTION: f64 = 0.62;
const COMPACT_WIDTH_FRACTION: f64 = 0.9;
const HEIGHT_FRACTION: f64 = 0.8;

struct ExportConfigBody {
    text: String,
    rect: Rect,
}

impl ExportConfigBody {
    fn new() -> Self {
        Self {
            text: String::new(),
            rect: Rect::ZERO,
        }
    }

    fn set_text(&mut self, text: String) {
        self.text = text;
    }

    fn line_color(theme: &Theme, line: &str) -> vello::peniko::Color {
        let colors = &theme.tokens.colors;
        if line.starts_with('#') {
            colors.neutral[6]
        } else if line.starts_with('[') {
            colors.accent[6]
        } else {
            colors.text_primary
        }
    }
}

impl Widget for ExportConfigBody {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        #[allow(
            clippy::expect_used,
            reason = "a fresh Taffy leaf cannot fail; Widget::build_layout cannot return a result"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .expect("fresh export body leaf");
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let colors = &theme.tokens.colors;
        // ModalDialog's body begins six pixels below the original subtitle
        // baseline. Drawing upward preserves the established card rhythm.
        let compact = self.rect.width() < 360.0;
        let subtitle = if compact {
            "Exact embedded profile; secrets are collected on the device."
        } else {
            "Embedded setup profile (JSON) — these choices are applied by the installer; secrets are collected on the device."
        };
        let text_width = (self.rect.width() - 8.0).max(0.0) as f32;
        text_renderer.draw_text_weighted(
            scene,
            subtitle,
            11.5,
            colors.text_dim,
            transform * Affine::translate((self.rect.x0 + 4.0, self.rect.y0 - 6.0)),
            Some(text_width),
            TextWeight::Regular,
        );

        let (_, subtitle_height) =
            text_renderer.measure_weighted(subtitle, 11.5, Some(text_width), TextWeight::Regular);
        let mut y = self.rect.y0 - 6.0 + f64::from(subtitle_height) + 10.0;
        for line in self.text.lines() {
            if y > self.rect.y1 {
                break;
            }
            text_renderer.draw_text_weighted(
                scene,
                line,
                12.0,
                Self::line_color(theme, line),
                transform * Affine::translate((self.rect.x0 + 4.0, y)),
                Some(text_width),
                TextWeight::Regular,
            );
            let (_, line_height) =
                text_renderer.measure_weighted(line, 12.0, Some(text_width), TextWeight::Regular);
            y += f64::from(line_height).max(17.0) + 3.0;
        }
        let hint = "click anywhere or press Esc to close";
        let (hint_width, _) = text_renderer.measure(hint, 11.0, None);
        text_renderer.draw_text_weighted(
            scene,
            hint,
            11.0,
            colors.neutral[6],
            transform
                * Affine::translate((
                    self.rect.center().x - f64::from(hint_width) / 2.0,
                    self.rect.y1 + 30.0,
                )),
            None,
            TextWeight::Regular,
        );
    }

    fn name(&self) -> &'static str {
        "ExportConfigBody"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// Shared-modal host for the generated configuration preview.
pub(crate) struct ExportDialog {
    dialog: ModalDialog<ExportConfigBody>,
}

impl ExportDialog {
    pub(crate) fn new() -> Self {
        Self {
            dialog: ModalDialog::new(TITLE, ExportConfigBody::new())
                .with_max_size(MAX_WIDTH, MAX_HEIGHT),
        }
    }

    pub(crate) fn show(&mut self, text: String) {
        self.dialog.content_mut().set_text(text);
        self.dialog.set_open(true);
    }

    pub(crate) fn is_open(&self) -> bool {
        self.dialog.is_open()
    }

    pub(crate) fn accessibility(&self) -> Option<AccessNode> {
        self.is_open().then_some(())?;
        self.dialog
            .accessibility()
            .map(|node| node.with_value(self.dialog.content().text.clone()))
    }

    pub(crate) fn layout_in(
        &mut self,
        viewport: Rect,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) {
        let width_fraction = if viewport.width() < 600.0 {
            COMPACT_WIDTH_FRACTION
        } else {
            WIDTH_FRACTION
        };
        self.dialog.set_max_size(
            (viewport.width() * width_fraction).min(MAX_WIDTH),
            (viewport.height() * HEIGHT_FRACTION).min(MAX_HEIGHT),
        );
        self.dialog.layout_in(viewport, theme, text_renderer);
    }

    pub(crate) fn draw_detached(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.dialog
            .draw(scene, transform, text_renderer, theme, presence);
    }

    pub(crate) fn handle_detached(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        if !self.is_open() {
            return false;
        }
        let dismiss = dismiss_event(event);
        let _ = self
            .dialog
            .handle_detached(event, scale_factor, modifiers, state, path);
        if dismiss {
            self.dialog.set_open(false);
        }
        true
    }

    #[cfg(test)]
    fn dialog_rect(&self) -> Rect {
        self.dialog.dialog_rect()
    }

    #[cfg(test)]
    pub(crate) fn text(&self) -> &str {
        &self.dialog.content().text
    }
}

fn dismiss_event(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } | WindowEvent::KeyboardInput {
            event: winit::event::KeyEvent {
                logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
                state: ElementState::Pressed,
                ..
            },
            ..
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::{DeviceId, KeyEvent};
    use winit::keyboard::{Key, KeyLocation, NamedKey, PhysicalKey};

    fn escape_event() -> WindowEvent {
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.physical_key =
            PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        event.logical_key = Key::Named(NamedKey::Escape);
        event.location = KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        WindowEvent::KeyboardInput {
            device_id: DeviceId::dummy(),
            event,
            is_synthetic: false,
        }
    }

    fn left_click() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    fn cursor_move() -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(12.0, 16.0),
        }
    }

    /// Export dialog layout, dismissal and read-only accessibility.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn export_dialog_layout_input_and_accessibility() {
        // Phase `responsive_layout_matches_the_existing_export_card`.
        {
            let mut dialog = ExportDialog::new();
            dialog.show("[install]\nhostname = tilo".into());
            dialog.layout_in(
                Rect::new(0.0, 0.0, 1280.0, 800.0),
                &Theme::dark(),
                &mut TextRenderer::new(),
            );
            let rect = dialog.dialog_rect();
            assert_eq!(rect.width(), 640.0);
            assert_eq!(rect.height(), 620.0);

            dialog.layout_in(
                Rect::new(0.0, 0.0, 800.0, 600.0),
                &Theme::dark(),
                &mut TextRenderer::new(),
            );
            let narrow = dialog.dialog_rect();
            assert_eq!(narrow.width(), 800.0 * WIDTH_FRACTION);
            assert_eq!(narrow.height(), 600.0 * HEIGHT_FRACTION);

            dialog.layout_in(
                Rect::new(0.0, 0.0, 320.0, 640.0),
                &Theme::dark(),
                &mut TextRenderer::new(),
            );
            let compact = dialog.dialog_rect();
            assert_eq!(compact.width(), 320.0 * COMPACT_WIDTH_FRACTION);
            assert_eq!(compact.height(), 640.0 * HEIGHT_FRACTION);
        }

        // Phase `escape_and_any_left_click_are_captured_and_dismiss`.
        {
            let mut dialog = ExportDialog::new();
            let mut doc = AutoCommit::new();
            dialog.show("# config".into());
            assert!(dialog.handle_detached(
                &cursor_move(),
                1.0,
                Modifiers::default(),
                &mut doc,
                &automerge::ROOT,
            ));
            assert!(
                dialog.is_open(),
                "pointer motion is captured, not dismissed"
            );
            assert!(dialog.handle_detached(
                &escape_event(),
                1.0,
                Modifiers::default(),
                &mut doc,
                &automerge::ROOT,
            ));
            assert!(!dialog.is_open());

            dialog.show("# config".into());
            assert!(dialog.handle_detached(
                &left_click(),
                1.0,
                Modifiers::default(),
                &mut doc,
                &automerge::ROOT,
            ));
            assert!(!dialog.is_open());
        }

        // Phase `syntax_roles_and_accessibility_preserve_the_read_only_document`.
        {
            let theme = Theme::dark();
            assert_eq!(
                ExportConfigBody::line_color(&theme, "# heading"),
                theme.tokens.colors.neutral[6]
            );
            assert_eq!(
                ExportConfigBody::line_color(&theme, "[install]"),
                theme.tokens.colors.accent[6]
            );
            assert_eq!(
                ExportConfigBody::line_color(&theme, "hostname = tilo"),
                theme.tokens.colors.text_primary
            );

            let mut dialog = ExportDialog::new();
            dialog.show("[install]\nhostname = tilo".into());
            let modal = dialog.accessibility().expect("dialog accessibility");
            assert_eq!(modal.role, ui::a11y::Role::Dialog);
            assert!(modal.state.modal);
            assert!(!modal.state.hidden);
            assert_eq!(modal.value.as_deref(), Some("[install]\nhostname = tilo"));
        }
    }
}
