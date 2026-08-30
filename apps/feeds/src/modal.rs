//! Content bodies for the feeds dialogs, hosted inside the shared
//! [`ui::widget::ModalDialog`].
//!
//! The dialog primitive owns the backdrop, the centered box, the title bar,
//! the footer Add/Cancel buttons, Esc + backdrop-click dismissal, and the
//! focus trap. Each body here fills the box's inner area with the app-specific
//! content: a helper caption, a field label, and a **real**
//! [`ui::widget::TextInput`] (the shared caret / selection / clipboard core, so
//! Ctrl+C/X/V and drag-select work), or — for a confirmation — just a message.
//!
//! This is the audit's "dialog redundancy" fix applied to feeds: the
//! hand-painted single-input card (with its fake text field and
//! "Press enter to add" copy) is gone; every feeds dialog now routes through
//! the one shared modal.

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{TextInput, Widget};

use crate::model::Id;

/// What a feeds dialog is asking for. Carried alongside the
/// [`ui::widget::ModalDialog`] so the submit handler knows which mutation to
/// run against the real store + document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalIntent {
    /// Add a new subscription from the typed URL.
    AddSubscription,
    /// Rename an existing feed to the typed name.
    RenameFeed { feed_id: Id },
    /// Confirm unsubscribing (destructive) from a feed. No text input.
    Unsubscribe { feed_id: Id },
}

impl ModalIntent {
    /// Whether this intent submits the value of a text field (so an empty
    /// field keeps the dialog open rather than submitting nothing).
    pub fn requires_input(self) -> bool {
        matches!(
            self,
            ModalIntent::AddSubscription | ModalIntent::RenameFeed { .. }
        )
    }
}

/// Top inset before the caption.
const TOP_PAD: f64 = 4.0;
/// Vertical band reserved for the (up to two-line) helper caption.
const CAPTION_BLOCK: f64 = 42.0;
/// Vertical band reserved for the field label above the input.
const LABEL_BLOCK: f64 = 24.0;
/// Height of the embedded text field.
const FIELD_H: f64 = 44.0;

const CAPTION_SIZE: f32 = 13.5;
const LABEL_SIZE: f32 = 12.0;

/// The content widget hosted inside a feeds [`ui::widget::ModalDialog`].
///
/// With an input it renders `caption → label → field`; without one it renders
/// a centered message (the unsubscribe confirmation). The embedded field is a
/// real [`TextInput`] in local mode, so it owns a genuine caret, selection,
/// and clipboard — never a per-app string buffer.
pub struct ModalBody {
    caption: String,
    label: String,
    input: Option<TextInput>,
    rect: Rect,
    field_rect: Rect,
}

impl ModalBody {
    /// A body with a labelled, focused text field.
    pub fn input(caption: &str, label: &str, initial: &str, placeholder: &str) -> Self {
        let mut input = TextInput::local().with_placeholder(placeholder);
        if !initial.is_empty() {
            input = input.with_initial_value(initial);
            input.select_all();
        }
        // The field is the only focusable thing in the dialog, and the host
        // drives the modal outside the App's focus machinery, so seed focus so
        // the caret is live and the first keystroke lands immediately.
        input.set_focus(true);
        Self {
            caption: caption.to_string(),
            label: label.to_string(),
            input: Some(input),
            rect: Rect::ZERO,
            field_rect: Rect::ZERO,
        }
    }

    /// A message-only body (a confirmation with no text entry).
    pub fn message(caption: &str) -> Self {
        Self {
            caption: caption.to_string(),
            label: String::new(),
            input: None,
            rect: Rect::ZERO,
            field_rect: Rect::ZERO,
        }
    }

    /// The current field value (trimmed by the caller). `""` when this body has
    /// no input (a confirmation).
    pub fn value(&self) -> &str {
        self.input.as_ref().map_or("", TextInput::value)
    }

    /// Seed the field text (used to stage a typed URL for a `--shots` capture).
    pub fn set_value(&mut self, s: &str) {
        if let Some(input) = &mut self.input {
            input.set_text(s);
            input.set_cursor_pos(s.len());
        }
    }

    /// Recompute the embedded field rect from the current body rect.
    fn place_field(&mut self) {
        if let Some(input) = &mut self.input {
            let mut y = self.rect.y0 + TOP_PAD;
            if !self.caption.is_empty() {
                y += CAPTION_BLOCK;
            }
            if !self.label.is_empty() {
                y += LABEL_BLOCK;
            }
            let field = Rect::new(self.rect.x0, y, self.rect.x1, y + FIELD_H);
            self.field_rect = field;
            input.apply_layout_external(field);
        }
    }
}

impl Widget for ModalBody {
    fn sync(&mut self, state: &mut automerge::AutoCommit, path: &automerge::ObjId) {
        if let Some(input) = &mut self.input {
            input.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        #[allow(
            clippy::unwrap_used,
            reason = "a fresh leaf on a live taffy tree cannot fail short of OOM, and \
                      `Widget::build_layout` returns `NodeId` directly with no fallible path"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
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
        let width = (self.rect.width()).max(1.0) as f32;

        if self.input.is_some() {
            // caption → label → field.
            let mut y = self.rect.y0 + TOP_PAD;
            if !self.caption.is_empty() {
                tr.draw_text(
                    scene,
                    &self.caption,
                    CAPTION_SIZE,
                    c.text_dim,
                    transform * Affine::translate((self.rect.x0, y)),
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
                    transform * Affine::translate((self.rect.x0, y)),
                    None,
                    TextWeight::Semibold,
                );
            }
            if let Some(input) = &mut self.input {
                input.draw(scene, transform, tr, theme, presence);
            }
        } else {
            // Message-only: vertically centered, left-aligned, wrapped.
            let (_, h) = tr.measure(&self.caption, CAPTION_SIZE + 1.5, Some(width));
            let y = self.rect.y0 + ((self.rect.height() - f64::from(h)) / 2.0).max(0.0);
            tr.draw_text(
                scene,
                &self.caption,
                CAPTION_SIZE + 1.5,
                c.text_primary,
                transform * Affine::translate((self.rect.x0, y)),
                Some(width),
            );
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(input) = &mut self.input {
            input.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            );
        }
    }

    fn is_focusable(&self) -> bool {
        self.input.as_ref().is_some_and(TextInput::is_focusable)
    }

    fn set_focus(&mut self, focused: bool) {
        if let Some(input) = &mut self.input {
            input.set_focus(focused);
        }
    }

    fn is_focused(&self) -> bool {
        self.input.as_ref().is_some_and(TextInput::is_focused)
    }

    fn name(&self) -> &'static str {
        "ModalBody"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
