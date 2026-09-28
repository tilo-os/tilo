//! Shared content body for a text-entry [`ModalDialog`](crate::widget::ModalDialog).
//!
//! [`PromptBody`] drops into `ModalDialog` as the content `C`; the dialog
//! owns the backdrop/box/title/footer/Esc while the body renders an
//! optional caption, an optional field label, and a [`TextInput`] with
//! selection, clipboard, and accessibility support. The host reads the result
//! with [`PromptBody::value`] on submit.
//!
//! ```ignore
//! let body = PromptBody::new("Name", "New volume")
//!     .with_caption("Choose a name for the volume.");
//! let dialog = ModalDialog::new("Create volume", body)
//!     .open(true)
//!     .with_max_size(420.0, 200.0)
//!     .primary_action("Create", move || submit.set(true))
//!     .secondary_action("Cancel", move || cancel.set(true));
//! // …on submit: dialog.content().value()
//! ```

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use winit::event::WindowEvent;

use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::{TextInput, Widget};

/// Top inset before the caption / label / field stack.
const TOP_PAD: f64 = 4.0;
/// Vertical band reserved for the (up to two-line) helper caption.
const CAPTION_BLOCK: f64 = 40.0;
/// Vertical band reserved for the field label above the input.
const LABEL_BLOCK: f64 = 22.0;
/// Height of a single-line embedded text field.
const FIELD_H: f64 = 44.0;

const CAPTION_SIZE: f32 = 13.5;
const LABEL_SIZE: f32 = 12.0;

/// Renders `caption → label → field`, where caption and label are optional.
pub struct PromptBody {
    caption: String,
    label: String,
    input: TextInput,
    multiline: bool,
    rect: Rect,
    field_rect: Rect,
}

impl PromptBody {
    /// The field is focused here since the host drives the modal outside the
    /// App's focus machinery.
    pub fn new(label: &str, placeholder: &str) -> Self {
        let mut input = TextInput::local().with_placeholder(placeholder);
        input.set_focus(true);
        Self {
            caption: String::new(),
            label: label.to_string(),
            input,
            multiline: false,
            rect: Rect::ZERO,
            field_rect: Rect::ZERO,
        }
    }

    #[must_use]
    pub fn with_caption(mut self, caption: &str) -> Self {
        self.caption = caption.to_string();
        self
    }

    /// Selects the seeded value so the first keystroke replaces it (the
    /// rename idiom).
    #[must_use]
    pub fn with_initial(mut self, initial: &str) -> Self {
        if !initial.is_empty() {
            self.input = self.input.with_initial_value(initial);
            self.input.select_all();
        }
        self
    }

    /// The field then fills the body's remaining height rather than a fixed
    /// single-line box.
    #[must_use]
    pub fn multiline(mut self, on: bool) -> Self {
        self.multiline = on;
        self.input = self.input.multiline(on);
        self
    }

    #[must_use]
    pub fn secret(mut self, on: bool) -> Self {
        self.input = self.input.with_secret(on);
        self
    }

    pub fn value(&self) -> &str {
        self.input.value()
    }

    pub fn set_value(&mut self, s: &str) {
        self.input.set_text(s);
        self.input.set_cursor_pos(s.chars().count());
    }

    pub fn input_mut(&mut self) -> &mut TextInput {
        &mut self.input
    }

    fn place_field(&mut self) {
        let mut y = self.rect.y0 + TOP_PAD;
        if !self.caption.is_empty() {
            y += CAPTION_BLOCK;
        }
        if !self.label.is_empty() {
            y += LABEL_BLOCK;
        }
        let bottom = if self.multiline {
            self.rect.y1.max(y + FIELD_H)
        } else {
            y + FIELD_H
        };
        let field = Rect::new(self.rect.x0, y, self.rect.x1, bottom);
        self.field_rect = field;
        self.input.apply_layout_external(field);
    }
}

/// Narrows a bounded on-screen dimension (a modal body's width, capped by
/// [`crate::widget::ModalDialog::with_max_size`]) to `f32` for the text
/// renderer's wrap width. Never approaches `f32::MAX`.
fn px(v: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "v is a modal body width, bounded well below f32::MAX"
    )]
    let out = v as f32;
    out
}

impl Widget for PromptBody {
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.input.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        self.place_field();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        let width = px(self.rect.width().max(1.0));
        let mut y = self.rect.y0 + TOP_PAD;
        if !self.caption.is_empty() {
            tr.draw_text(
                scene,
                &self.caption,
                CAPTION_SIZE,
                c.text_dim,
                transform.pre_translate(Vec2::new(self.rect.x0, y)),
                Some(width),
            );
            y += CAPTION_BLOCK;
        }
        if !self.label.is_empty() {
            tr.draw_text_weighted(
                scene,
                &self.label,
                LABEL_SIZE,
                c.text_primary,
                transform.pre_translate(Vec2::new(self.rect.x0, y)),
                None,
                TextWeight::Semibold,
            );
        }
        self.input.draw(scene, transform, tr, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.input.handle_event(event, ctx);
    }

    fn is_focusable(&self) -> bool {
        self.input.is_focusable()
    }

    fn set_focus(&mut self, focused: bool) {
        self.input.set_focus(focused);
    }

    fn is_focused(&self) -> bool {
        self.input.is_focused()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        Widget::ime_cursor_rect(&self.input)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Widget::ime_purpose(&self.input)
    }

    fn name(&self) -> &'static str {
        "PromptBody"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::ModalDialog;
    use automerge::AutoCommit;
    use vello::kurbo::Point;
    use winit::event::{DeviceId, ElementState, KeyEvent, MouseButton};
    use winit::keyboard::{Key, KeyLocation, NamedKey, PhysicalKey};

    fn synth_device() -> DeviceId {
        unsafe { std::mem::zeroed() }
    }

    fn laid_out(body: PromptBody, viewport: Rect) -> ModalDialog<PromptBody> {
        let mut d = ModalDialog::new("Prompt", body)
            .open(true)
            .with_viewport(viewport)
            .with_max_size(420.0, 220.0);
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let theme = Theme::dark();
        let node = d.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(px(viewport.width())),
                    height: taffy::AvailableSpace::Definite(px(viewport.height())),
                },
            )
            .unwrap();
        d.apply_layout(&taffy, node, Point::new(viewport.x0, viewport.y0));
        d
    }

    fn char_event(ch: &str) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(ch.into());
        ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(ch.into());
        WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }

    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth_device(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }

    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    fn dispatch(d: &mut ModalDialog<PromptBody>, event: &WindowEvent) {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.leaf_node(Style::default());
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        d.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
    }

    #[test]
    fn prompt_field_is_the_real_editable_input() {
        // Case `typing_into_prompt_updates_the_real_field`.
        {
            let body = PromptBody::new("Name", "New volume");
            let mut d = laid_out(body, Rect::new(0.0, 0.0, 800.0, 600.0));
            dispatch(&mut d, &char_event("h"));
            dispatch(&mut d, &char_event("i"));
            assert_eq!(
                d.content().value(),
                "hi",
                "keystrokes must land in the embedded TextInput's real value",
            );
        }

        // Case `initial_value_is_selected_for_replace`.
        {
            let body = PromptBody::new("Name", "").with_initial("old-name");
            let mut d = laid_out(body, Rect::new(0.0, 0.0, 800.0, 600.0));
            assert_eq!(d.content().value(), "old-name");
            dispatch(&mut d, &char_event("x"));
            assert_eq!(
                d.content().value(),
                "x",
                "typing over a selected initial value must replace it",
            );
        }

        // Case `click_in_field_reaches_the_input`.
        {
            let body = PromptBody::new("Name", "hint");
            let mut d = laid_out(body, Rect::new(0.0, 0.0, 800.0, 600.0));
            dispatch(&mut d, &char_event("a"));
            dispatch(&mut d, &char_event("b"));
            let center = d.dialog_rect().center();
            dispatch(&mut d, &cursor_at(center));
            dispatch(&mut d, &left_press());
            assert!(
                d.content().is_focused(),
                "the field stays focused after an in-box click",
            );
            dispatch(&mut d, &char_event("c"));
            assert!(
                d.content().value().contains('c'),
                "typing continues to reach the field after the click",
            );
        }

        // Case `multiline_prompt_accepts_newlines`.
        {
            let body = PromptBody::new("Notes", "").multiline(true);
            let mut d = laid_out(body, Rect::new(0.0, 0.0, 800.0, 600.0));
            dispatch(&mut d, &char_event("a"));
            let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
            ev.logical_key = Key::Named(NamedKey::Enter);
            ev.physical_key =
                PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
            ev.location = KeyLocation::Standard;
            ev.state = ElementState::Pressed;
            ev.repeat = false;
            ev.text = None;
            // The dialog itself does not intercept Enter (only Escape), so it
            // forwards to the multiline field, which inserts a newline.
            dispatch(
                &mut d,
                &WindowEvent::KeyboardInput {
                    device_id: synth_device(),
                    event: ev,
                    is_synthetic: false,
                },
            );
            dispatch(&mut d, &char_event("b"));
            assert_eq!(
                d.content().value(),
                "a\nb",
                "Enter in a multiline prompt inserts a newline",
            );
        }
    }
}
