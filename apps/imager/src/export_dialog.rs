//! Read-only embedded-media-profile dialog.
//!
//! The host owns the policy which produces the export text. This component
//! owns the transient modal state and every UI concern around presenting it.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::a11y::AccessNode;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{ModalDialog, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};

const TITLE: &str = "Embedded setup profile";
/// The one thing the person needs to know about the profile below.
const NOTE: &str = "The installer applies these choices. Passwords and keys are set on the device.";

struct ExportConfigBody {
    text: String,
    rect: Rect,
    theme: Option<Theme>,
}

impl ExportConfigBody {
    fn new() -> Self {
        Self {
            text: String::new(),
            rect: Rect::ZERO,
            theme: None,
        }
    }

    fn set_text(&mut self, text: String) {
        self.text = text;
    }

    /// The note and the profile's lines, with their styles, top to bottom.
    fn blocks<'a>(&'a self, theme: &Theme) -> Vec<(&'a str, TextStyle, bool)> {
        let mut blocks = vec![(NOTE, theme.text(TextRole::Caption), true)];
        blocks.extend(
            self.text
                .lines()
                .map(|line| (line, theme.text(TextRole::Code), false)),
        );
        blocks
    }

    /// The body's height at `width`: the note, a gap, then every line.
    fn height(&self, tr: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        let wrap = Some(width.max(1.0) as f32);
        let gap = f64::from(theme.tokens.spacing.rhythm.section);
        self.blocks(theme)
            .iter()
            .enumerate()
            .map(|(index, (text, style, _))| {
                let line = if text.is_empty() { " " } else { text };
                f64::from(tr.measure_style(line, wrap, style).1)
                    + if index == 0 { gap } else { 0.0 }
            })
            .sum()
    }
}

impl Widget for ExportConfigBody {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = Some(theme.clone());
        #[allow(
            clippy::expect_used,
            reason = "a fresh Taffy leaf cannot fail; Widget::build_layout cannot return a result"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::auto(),
                },
                ..Default::default()
            })
            .expect("fresh export body leaf");
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let Some(theme) = self.theme.clone() else {
            return taffy::Size::ZERO;
        };
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(width) => width,
            _ => 1.0,
        });
        let height = self.height(text_renderer, &theme, f64::from(width));
        taffy::Size {
            width,
            height: height as f32,
        }
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
        let wrap = Some(self.rect.width().max(1.0) as f32);
        let gap = f64::from(theme.tokens.spacing.rhythm.section);
        let mut y = self.rect.y0;
        for (index, (text, style, note)) in self.blocks(theme).into_iter().enumerate() {
            if y > self.rect.y1 {
                break;
            }
            let ink = if note {
                theme.text_dim
            } else {
                theme.text_primary
            };
            text_renderer.draw_style(
                scene,
                text,
                ink,
                transform * Affine::translate((self.rect.x0, y)),
                wrap,
                &style,
            );
            let line = if text.is_empty() { " " } else { text };
            y += f64::from(text_renderer.measure_style(line, wrap, &style).1);
            if index == 0 {
                y += gap;
            }
        }
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
            dialog: ModalDialog::new(TITLE, ExportConfigBody::new()).on_dismiss(|| {}),
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

    /// The sheet, in the window's top layer over the whole window.
    pub(crate) fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.dialog.is_open() {
            return;
        }
        // The body measures in the theme the sheet is painted with.
        self.dialog.content_mut().theme = Some(theme.clone());
        self.dialog
            .draw_overlay(scene, transform, text_renderer, theme, presence);
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

    /// Paint the dialog's top layer over `window`, as the host does.
    fn present(dialog: &mut ExportDialog, window: Rect) {
        let presence = PresenceCtx::empty();
        let previous = presence.push_viewport(window);
        let mut scene = Scene::new();
        dialog.draw_overlay(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &presence,
        );
        presence.pop_viewport(previous);
    }

    /// Export dialog layout, dismissal and read-only accessibility.
    #[test]
    fn export_dialog_layout_input_and_accessibility() {
        // Phase `the_sheet_follows_its_content_inside_the_window`.
        {
            let mut dialog = ExportDialog::new();
            dialog.show("{\n  \"hostname\": \"tilo\"\n}".into());
            for window in [
                Rect::new(0.0, 0.0, 1280.0, 800.0),
                Rect::new(0.0, 0.0, 800.0, 600.0),
                Rect::new(0.0, 0.0, 320.0, 640.0),
            ] {
                present(&mut dialog, window);
                let rect = dialog.dialog_rect();
                assert!(rect.width() > 0.0 && rect.height() > 0.0);
                assert!(
                    rect.x0 >= window.x0 && rect.x1 <= window.x1,
                    "{rect:?} fits {window:?}"
                );
                assert!(
                    rect.height() < window.height(),
                    "a short profile is a short sheet"
                );
            }
        }

        // Phase `escape_and_any_left_click_are_captured_and_dismiss`.
        {
            let mut dialog = ExportDialog::new();
            let mut doc = AutoCommit::new();
            dialog.show("{}".into());
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

            dialog.show("{}".into());
            assert!(dialog.handle_detached(
                &left_click(),
                1.0,
                Modifiers::default(),
                &mut doc,
                &automerge::ROOT,
            ));
            assert!(!dialog.is_open());
        }

        // Phase `accessibility_preserves_the_read_only_document`.
        {
            let mut dialog = ExportDialog::new();
            dialog.show("{\n  \"hostname\": \"tilo\"\n}".into());
            let modal = dialog.accessibility().expect("dialog accessibility");
            assert_eq!(modal.role, ui::a11y::Role::Dialog);
            assert!(modal.state.modal);
            assert!(!modal.state.hidden);
            assert_eq!(
                modal.value.as_deref(),
                Some("{\n  \"hostname\": \"tilo\"\n}")
            );
        }
    }
}
