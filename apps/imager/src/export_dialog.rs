//! Read-only embedded-media-profile dialog.
//!
//! The host owns the policy which produces the export text. This component
//! owns the transient modal state and every UI concern around presenting it.

#[cfg(test)]
use automerge::AutoCommit;
use std::collections::HashMap;

use automerge::ObjId;
use taffy::TaffyTree;
#[cfg(test)]
use ui::a11y::AccessNode;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{ModalDialog, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};

const TITLE: &str = "Embedded setup profile";
const WIDTH_FRACTION: f64 = 0.62;
const COMPACT_WIDTH_FRACTION: f64 = 0.9;

fn profile_preview(text: &str) -> ui::widget::Text {
    ui::widget::Text::new(text).with_role(ui::TextRole::Code)
}

fn profile_body(text: &str) -> ui::widget::Column {
    use ui::TextRole;
    use ui::widget::Text;
    ui::widget::Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(
            Text::new("Exact embedded profile; secrets are collected on the device.")
                .with_role(TextRole::Caption)
                .dimmed(),
        ))
        .push(Box::new(profile_preview(text)))
        .push(Box::new(
            Text::new("Press Escape to close")
                .with_role(TextRole::Caption)
                .dimmed(),
        ))
}

/// Shared-modal host for the generated configuration preview.
pub(crate) struct ExportDialog {
    dialog: ModalDialog<ui::widget::Column>,
    text: String,
}

impl ExportDialog {
    pub(crate) fn new() -> Self {
        Self {
            dialog: ModalDialog::new(TITLE, profile_body(""))
                .with_max_width(f64::from(ui::tokens::Metrics::BASE.reading_measure)),
            text: String::new(),
        }
    }

    pub(crate) fn show(&mut self, text: String) {
        self.text = text;
        *self.dialog.content_mut() = profile_body(&self.text);
        self.dialog.invalidate_layout();
        self.dialog.set_open(true);
    }

    pub(crate) fn is_open(&self) -> bool {
        self.dialog.is_open()
    }

    #[cfg(test)]
    pub(crate) fn accessibility(&self) -> Option<AccessNode> {
        self.is_open().then_some(())?;
        self.dialog
            .accessibility()
            .map(|node| node.with_value(self.text.clone()))
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
        self.dialog.set_max_width(
            (viewport.width() * width_fraction)
                .min(f64::from(theme.tokens.metrics.reading_measure)),
        );
        let mut tree = TaffyTree::new();
        let node = self
            .dialog
            .build_layout(&mut tree, theme, &mut HashMap::new());
        let size = taffy::Size {
            width: taffy::AvailableSpace::Definite(viewport.width() as f32),
            height: taffy::AvailableSpace::Definite(viewport.height() as f32),
        };
        let _ = tree.compute_layout(node, size);
        if self
            .dialog
            .adjust_layout_with_measure(&mut tree, node, text_renderer)
        {
            let _ = tree.compute_layout(node, size);
        }
        self.dialog.apply_layout(&tree, node, viewport.origin());
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
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    pub(crate) fn handle_detached(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        state: &mut ui::widget::Edits<'_>,
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
        &self.text
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
            assert!(
                rect.height() <= 620.0,
                "short profile uses its intrinsic shared-modal height"
            );
            assert!(rect.height() > 0.0);

            dialog.layout_in(
                Rect::new(0.0, 0.0, 800.0, 600.0),
                &Theme::dark(),
                &mut TextRenderer::new(),
            );
            let narrow = dialog.dialog_rect();
            assert_eq!(narrow.width(), 800.0 * WIDTH_FRACTION);
            assert!(narrow.height() <= 600.0 * 0.8);

            dialog.layout_in(
                Rect::new(0.0, 0.0, 320.0, 640.0),
                &Theme::dark(),
                &mut TextRenderer::new(),
            );
            let compact = dialog.dialog_rect();
            assert_eq!(
                compact.width(),
                (320.0 * COMPACT_WIDTH_FRACTION)
                    .min(320.0 - 2.0 * f64::from(Theme::dark().tokens.spacing.rhythm.group),),
            );
            assert!(compact.height() <= 640.0 * 0.8);
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
                &mut ui::widget::Edits::lend(&mut doc),
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
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert!(!dialog.is_open());

            dialog.show("# config".into());
            assert!(dialog.handle_detached(
                &left_click(),
                1.0,
                Modifiers::default(),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert!(!dialog.is_open());
        }

        // Phase `syntax_roles_and_accessibility_preserve_the_read_only_document`.
        {
            let preview = profile_preview("[install]\nhostname = tilo");
            assert_eq!(preview.role(), ui::TextRole::Code);
            let body = profile_body("[install]\nhostname = tilo");
            let children = body.children();
            assert_eq!(
                children.len(),
                3,
                "explanation, exact profile and dismissal hint"
            );
            let code = children[1]
                .accessibility()
                .expect("the retained profile is readable");
            assert_eq!(code.name.as_deref(), Some("[install]\nhostname = tilo"));

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
