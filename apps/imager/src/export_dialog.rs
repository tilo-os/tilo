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
        let _ = ui::widget::solve_layout(&mut tree, node, size);
        if self
            .dialog
            .adjust_layout_style(&mut tree, node, text_renderer)
        {
            let _ = ui::widget::solve_layout(&mut tree, node, size);
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

    use winit::keyboard::{Key, NamedKey};

    fn escape_event() -> WindowEvent {
        ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
            Key::Named(NamedKey::Escape),
            None,
            ElementState::Pressed,
        ))
    }

    use ui::synthetic_input::mouse_press;

    fn cursor_move() -> WindowEvent {
        ui::synthetic_input::cursor_moved(12.0, 16.0)
    }

    /// Compact pages follow the shorter window axis and resolved UI scale;
    /// accessibility must describe the same full-window surface.
    #[test]
    fn export_dialog_compact_layout_and_accessibility_fill_the_window() {
        let cases = vec![
            (Rect::new(0.0, 0.0, 800.0, 480.0), Theme::dark()),
            (Rect::new(0.0, 0.0, 320.0, 640.0), Theme::light()),
            (Rect::new(0.0, 0.0, 320.0, 640.0), Theme::high_contrast()),
            (
                Rect::new(0.0, 0.0, 1280.0, 800.0),
                Theme::dark().with_ui_scale(1.5),
            ),
        ];
        let mut dialog = ExportDialog::new();
        dialog.show("[install]\nhostname = tilo".into());
        for (viewport, theme) in cases {
            dialog.layout_in(viewport, &theme, &mut TextRenderer::new());
            assert_eq!(dialog.dialog_rect(), viewport);
            let update =
                ui::accesskit_bridge::build_tree_update(&dialog.dialog, &HashMap::new(), None, 1.0);
            let modal = update
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some(TITLE))
                .expect("the open modal is exported");
            let bounds = modal.1.bounds().expect("exported modal bounds");
            assert_eq!(
                Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1),
                viewport,
                "exported accessibility bounds follow the shared-modal layout"
            );
        }
    }

    fn responsive_export_layout() {
        for theme in [Theme::dark(), Theme::light()] {
            let mut dialog = ExportDialog::new();
            dialog.show("[install]\nhostname = tilo".into());
            let mut text = TextRenderer::new();
            let desktop = Rect::new(0.0, 0.0, 1280.0, 800.0);
            dialog.layout_in(desktop, &theme, &mut text);
            let rect = dialog.dialog_rect();
            assert_eq!(rect.width(), 640.0);
            assert_eq!(rect.center().x, desktop.center().x);
            assert!(
                rect.height() <= 620.0,
                "short profile keeps its intrinsic height"
            );
            assert!(rect.height() > 0.0);
            assert_eq!(rect.intersect(desktop), rect);

            let intermediate = Rect::new(0.0, 0.0, 800.0, 600.0);
            dialog.layout_in(intermediate, &theme, &mut text);
            let sheet = dialog.dialog_rect();
            assert_eq!(sheet.width(), 800.0 * WIDTH_FRACTION);
            assert!(sheet.height() <= 600.0 * 0.8);
            assert_eq!(sheet.intersect(intermediate), sheet);

            // Either short axis requires the shared full-window mobile page.
            // Nonzero origins also prove this uses the actual content viewport.
            for viewport in [
                Rect::new(0.0, 0.0, 320.0, 640.0),
                Rect::new(0.0, 0.0, 800.0, 320.0),
                Rect::new(20.0, 40.0, 410.0, 884.0),
            ] {
                dialog.layout_in(viewport, &theme, &mut text);
                // The real host supplies the window to its top-layer paint;
                // the next layout places retained content in that viewport.
                let presence = PresenceCtx::default();
                let previous = presence.push_viewport(viewport);
                dialog.draw_detached(
                    &mut Scene::new(),
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    &presence,
                );
                presence.pop_viewport(previous);
                dialog.layout_in(viewport, &theme, &mut text);
                assert_eq!(
                    dialog.dialog_rect(),
                    viewport,
                    "compact dialog fills its viewport"
                );
                let body = dialog.dialog.content().get_rect();
                assert!(body.width() > 0.0 && body.height() > 0.0);
                assert_eq!(
                    body.intersect(viewport),
                    body,
                    "profile body remains reachable"
                );
            }
        }
    }

    /// Export dialog layout, dismissal and read-only accessibility.
    #[test]
    fn export_dialog_layout_input_and_accessibility() {
        responsive_export_layout();

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
                &mouse_press(),
                1.0,
                Modifiers::default(),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert!(!dialog.is_open());
        }

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
