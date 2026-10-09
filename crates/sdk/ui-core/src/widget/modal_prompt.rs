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
//!     .with_max_width(420.0)
//!     .primary_action("Create", move || submit.set(true))
//!     .secondary_action("Cancel", move || cancel.set(true));
//! // …on submit: dialog.content().value()
//! ```
//!
//! A confirmation that waits for a typed phrase (erasing a disk, deleting a
//! vault) is [`PromptBody::confirm_phrase`] with the dialog's primary action
//! gated on [`PromptBody::confirms`]:
//!
//! ```ignore
//! ModalDialog::new("Erase sdb?", PromptBody::confirm_phrase(message, "sdb"))
//!     .with_destructive(true)
//!     .primary_action("Erase", erase)
//!     .primary_enabled_if(PromptBody::confirms)
//!     .secondary_action("Cancel", cancel);
//! ```

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use winit::event::WindowEvent;

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::{ControlSize, TextInput, Widget};

/// Renders `caption → label → field`, where caption and label are optional.
pub struct PromptBody {
    caption: String,
    label: String,
    input: TextInput,
    multiline: bool,
    /// The phrase a confirmation waits for.
    phrase: Option<String>,
    rect: Rect,
    field_rect: Rect,
    /// The caption's height at the body's width, from the last measure.
    caption_h: f64,
    label_height: f64,
    theme: Theme,
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
            phrase: None,
            rect: Rect::ZERO,
            field_rect: Rect::ZERO,
            caption_h: 0.0,
            label_height: 0.0,
            theme: Theme::dark(),
        }
    }

    /// A confirmation that waits for `phrase` to be typed: `message` says
    /// what will happen, the field asks for the phrase by name. Gate the
    /// dialog's primary action on [`Self::confirms`].
    pub fn confirm_phrase(message: &str, phrase: &str) -> Self {
        let mut body =
            Self::new(&format!("Type {phrase} to confirm"), phrase).with_caption(message);
        body.input = std::mem::replace(&mut body.input, TextInput::local())
            .with_accessible_name(format!("Type {phrase} to confirm"));
        body.phrase = Some(phrase.to_string());
        body
    }

    /// Whether the typed text is the confirmation's phrase (always `true`
    /// for a body that asks for no phrase).
    pub fn confirms(&self) -> bool {
        self.phrase
            .as_deref()
            .is_none_or(|phrase| self.input.value().trim() == phrase)
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

    /// The gap between the caption, the label and the field.
    fn gap(theme: &Theme) -> f64 {
        f64::from(theme.tokens.spacing.s2)
    }

    fn label_h(theme: &Theme) -> f64 {
        f64::from(theme.text(TextRole::Label).line_height_px())
    }

    /// The field's height: one Regular control, or three body lines for a
    /// multiline field before it scrolls.
    fn field_h(&self, theme: &Theme) -> f64 {
        let control = ControlSize::Regular.height(theme);
        if self.multiline {
            control + 2.0 * f64::from(theme.text(TextRole::Body).line_height_px())
        } else {
            control
        }
    }

    /// The stack's height at the caption height last measured.
    fn height(&self, theme: &Theme) -> f64 {
        let gap = Self::gap(theme);
        let mut h = self.field_h(theme);
        if !self.caption.is_empty() {
            h += self.caption_h + gap * 2.0;
        }
        if !self.label.is_empty() {
            h += self.label_height.max(Self::label_h(theme)) + gap;
        }
        h
    }

    fn place_field(&mut self) {
        let theme = &self.theme;
        let gap = Self::gap(theme);
        let mut y = self.rect.y0;
        if !self.caption.is_empty() {
            y += self.caption_h + gap * 2.0;
        }
        if !self.label.is_empty() {
            y += self.label_height.max(Self::label_h(theme)) + gap;
        }
        let field_h = self.field_h(theme);
        let bottom = if self.multiline {
            self.rect.y1.max(y + field_h)
        } else {
            y + field_h
        };
        let field = Rect::new(self.rect.x0, y, self.rect.x1, bottom);
        self.field_rect = field;
        self.input.apply_layout_external(field);
    }
}
impl Widget for PromptBody {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.input.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = theme.clone();
        // Full width; as tall as the stack it shows, so the sheet hugs it.
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            _ => 0.0,
        });
        let theme = self.theme.clone();
        self.caption_h = if self.caption.is_empty() {
            0.0
        } else {
            let wrap = (width > 0.0).then_some(width);
            f64::from(
                text_renderer
                    .measure_style(&self.caption, wrap, &theme.text(TextRole::Body))
                    .1,
            )
        };
        self.label_height = f64::from(
            text_renderer
                .measure_style(
                    &self.label,
                    (width > 0.0).then_some(width),
                    &theme.text(TextRole::Label),
                )
                .1,
        );
        taffy::Size {
            width,
            height: crate::num::f32_of_f64(self.height(&theme).ceil()),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
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
        let width = crate::num::f32_of_f64(self.rect.width().max(1.0));
        let gap = Self::gap(theme);
        self.theme = theme.clone();
        self.caption_h = f64::from(
            tr.measure_style(&self.caption, Some(width), &theme.text(TextRole::Body))
                .1,
        );
        self.label_height = f64::from(
            tr.measure_style(&self.label, Some(width), &theme.text(TextRole::Label))
                .1,
        );
        self.place_field();
        let mut y = self.rect.y0;
        if !self.caption.is_empty() {
            tr.draw_style(
                scene,
                &self.caption,
                c.text_dim,
                transform.pre_translate(Vec2::new(self.rect.x0, y)),
                Some(width),
                &theme.text(TextRole::Body),
            );
            y += self.caption_h + gap * 2.0;
        }
        if !self.label.is_empty() {
            tr.draw_style(
                scene,
                &self.label,
                c.text_primary,
                transform.pre_translate(Vec2::new(self.rect.x0, y)),
                Some(width),
                &theme.text(TextRole::Label),
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

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_cursor_rect()?;
        self.input.ime_edit_fingerprint(key)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Widget::ime_purpose(&self.input)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let mut node = Widget::accessibility(&self.input)?;
        if node.name.is_none() && !self.label.is_empty() {
            node.name = Some(self.label.clone());
        }
        if !self.caption.is_empty() {
            node.description = Some(self.caption.clone());
        }
        Some(node)
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
    use winit::event::ElementState;
    use winit::keyboard::{Key, NamedKey};

    fn laid_out(body: PromptBody, viewport: Rect) -> ModalDialog<PromptBody> {
        let mut d = ModalDialog::new("Prompt", body)
            .open(true)
            .with_max_width(420.0);
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let theme = Theme::dark();
        let node = d.build_layout(&mut taffy, &theme, &mut node_map);
        let mut text = TextRenderer::new();
        let mut bindings = crate::bound_dispatch::BoundNodeMap::default();
        // SAFETY: `node_map` describes the tree just built from `d`.
        unsafe {
            bindings.rebuild(&d, &taffy, node, &node_map);
            crate::widget::layout::compute_widget_layout(
                &mut taffy,
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(crate::num::f32_of_f64(
                        viewport.width(),
                    )),
                    height: taffy::AvailableSpace::Definite(crate::num::f32_of_f64(
                        viewport.height(),
                    )),
                },
                &node_map,
                &bindings,
                &mut text,
            )
            .unwrap();
        }
        d.apply_layout(&taffy, node, Point::new(viewport.x0, viewport.y0));
        d
    }

    #[test]
    fn a_typed_phrase_gates_the_confirmation() {
        let body = PromptBody::confirm_phrase("Everything on sdb will be erased.", "sdb");
        let mut d = laid_out(body, Rect::new(0.0, 0.0, 800.0, 600.0));
        assert!(!d.content().confirms());
        dispatch(&mut d, &char_key("s"));
        dispatch(&mut d, &char_key("d"));
        assert!(!d.content().confirms());
        dispatch(&mut d, &char_key("b"));
        assert!(d.content().confirms(), "the phrase confirms");
        let node = Widget::accessibility(d.content()).unwrap();
        assert_eq!(node.name.as_deref(), Some("Type sdb to confirm"));
        assert!(
            d.content().get_rect().height() > ControlSize::Regular.height(&Theme::dark()) * 2.0,
            "the message and the label stand above the field"
        );
    }

    use crate::synthetic_input::char_key;

    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }

    use crate::synthetic_input::mouse_press;

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
                state: crate::widget::Edits::lend(&mut doc),
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
    }

    #[test]
    fn wrapped_prompt_help_leaves_the_editable_field_below_all_lines() {
        for scale in [1.0, 2.0] {
            let mut body = PromptBody::new(
                "Only the playlist owner can rename it. Saving does not grant editing access. Ask the owner for permission before editing this shared playlist.",
                "Name",
            )
            .with_caption("This change is sent to the provider after confirmation.");
            body.rect = Rect::new(0.0, 0.0, 240.0, 700.0);
            let mut text = TextRenderer::new();
            text.set_text_scale(scale);
            let theme = Theme::high_contrast().with_text_scale(scale);
            body.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut text,
                &theme,
                &PresenceCtx::empty(),
            );
            let (_, caption) =
                text.measure_style(&body.caption, Some(240.0), &theme.text(TextRole::Body));
            let (_, label) =
                text.measure_style(&body.label, Some(240.0), &theme.text(TextRole::Label));
            assert!(
                label > theme.text(TextRole::Label).line_height_px() * 2.0,
                "the long label occupies multiple lines"
            );
            assert!(
                body.field_rect.y0 >= f64::from(caption + label) + PromptBody::gap(&theme) * 3.0,
                "text and editable field do not overlap"
            );
            assert!(body.field_rect.y1 <= body.rect.y1);
        }
    }

    #[test]
    fn prompt_field_is_the_real_editable_input() {
        // Case `typing_into_prompt_updates_the_real_field`.
        {
            let body = PromptBody::new("Name", "New volume");
            let mut d = laid_out(body, Rect::new(0.0, 0.0, 800.0, 600.0));
            dispatch(&mut d, &char_key("h"));
            dispatch(&mut d, &char_key("i"));
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
            dispatch(&mut d, &char_key("x"));
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
            dispatch(&mut d, &char_key("a"));
            dispatch(&mut d, &char_key("b"));
            let center = d.dialog_rect().center();
            dispatch(&mut d, &cursor_at(center));
            dispatch(&mut d, &mouse_press());
            assert!(
                d.content().is_focused(),
                "the field stays focused after an in-box click",
            );
            dispatch(&mut d, &char_key("c"));
            assert!(
                d.content().value().contains('c'),
                "typing continues to reach the field after the click",
            );
        }

        // Case `multiline_prompt_accepts_newlines`.
        {
            let body = PromptBody::new("Notes", "").multiline(true);
            let mut d = laid_out(body, Rect::new(0.0, 0.0, 800.0, 600.0));
            dispatch(&mut d, &char_key("a"));
            let ev = crate::synthetic_input::key_event(
                Key::Named(NamedKey::Enter),
                None,
                ElementState::Pressed,
            );
            // The dialog itself does not intercept Enter (only Escape), so it
            // forwards to the multiline field, which inserts a newline.
            dispatch(
                &mut d,
                &WindowEvent::KeyboardInput {
                    device_id: winit::event::DeviceId::dummy(),
                    event: ev,
                    is_synthetic: false,
                },
            );
            dispatch(&mut d, &char_key("b"));
            assert_eq!(
                d.content().value(),
                "a\nb",
                "Enter in a multiline prompt inserts a newline",
            );
        }
    }
}
