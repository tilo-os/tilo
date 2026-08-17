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
    /// Confirm marking every article in the current view as read. No text
    /// input — a plain confirmation before a bulk, not-easily-undone change.
    MarkAllRead,
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
    /// An inline validation / failure message shown in the theme's danger
    /// colour just below the field. `Some` keeps the dialog honest about why a
    /// submit didn't go through (a bad URL, an unreachable feed) while leaving
    /// the input fully usable so the user can fix it in place.
    error: Option<String>,
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
            error: None,
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
            error: None,
        }
    }

    /// Show `msg` as an inline error under the field. Clearing (via
    /// [`Self::clear_error`]) or a fresh submit removes it.
    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.error = Some(msg.into());
    }

    /// Remove any inline error (e.g. as the user edits the field).
    pub fn clear_error(&mut self) {
        self.error = None;
    }

    /// The current inline error, if any.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The current field value (trimmed by the caller). `""` when this body has
    /// no input (a confirmation).
    pub fn value(&self) -> &str {
        self.input.as_ref().map(|i| i.value()).unwrap_or("")
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
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
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
            // Inline error under the field — the honest "why it didn't submit".
            if let Some(err) = &self.error {
                let ey = self.field_rect.y1 + 8.0;
                tr.draw_text(
                    scene,
                    err,
                    CAPTION_SIZE,
                    c.semantic.danger,
                    transform * Affine::translate((self.rect.x0, ey)),
                    Some(width),
                );
            }
        } else {
            // Message-only: vertically centered, left-aligned, wrapped.
            let (_, h) = tr.measure(&self.caption, CAPTION_SIZE + 1.5, Some(width));
            let y = self.rect.y0 + ((self.rect.height() - h as f64) / 2.0).max(0.0);
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
        // The moment the user edits the field, drop a stale error so the
        // dialog doesn't nag while they fix the URL.
        if self.error.is_some()
            && matches!(
                event,
                WindowEvent::KeyboardInput { event: ke, .. } if ke.state == winit::event::ElementState::Pressed
            )
        {
            self.error = None;
        }
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
        self.input.as_ref().map(|i| i.is_focusable()).unwrap_or(false)
    }

    fn set_focus(&mut self, focused: bool) {
        if let Some(input) = &mut self.input {
            input.set_focus(focused);
        }
    }

    fn is_focused(&self) -> bool {
        self.input.as_ref().map(|i| i.is_focused()).unwrap_or(false)
    }

    fn name(&self) -> &str {
        "ModalBody"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
