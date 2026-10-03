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
    /// Create a named folder in the subscription sidebar.
    AddFolder,
    /// Rename an existing feed to the typed name.
    RenameFeed { feed_id: Id },
    /// Confirm unsubscribing (destructive) from a feed. No text input.
    Unsubscribe { feed_id: Id },
    /// Replace the selected article's comma-separated labels.
    EditArticleTags { article_id: Id },
}

impl ModalIntent {
    /// Whether this intent requires a non-empty text field. Tag editing also
    /// owns a field, but an empty value deliberately means "remove all tags".
    pub fn requires_input(self) -> bool {
        matches!(
            self,
            ModalIntent::AddSubscription | ModalIntent::AddFolder | ModalIntent::RenameFeed { .. }
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
    // Keep a distinct retained-widget address. An inline field can alias its
    // container and lose its native accessibility layout identity.
    input: Option<Box<TextInput>>,
    rect: Rect,
    field_rect: Rect,
}

impl ModalBody {
    /// A body with a labelled, focused text field.
    pub fn input(caption: &str, label: &str, initial: &str, placeholder: &str) -> Self {
        let mut input = TextInput::local()
            .with_placeholder(placeholder)
            .with_accessible_name(label);
        if !initial.is_empty() {
            input = input.with_initial_value(initial);
            input.select_all();
        }
        // Seed focus for immediate input while the host rebuilds the retained
        // modal tree and installs its focus trap.
        input.set_focus(true);
        Self {
            caption: caption.to_string(),
            label: label.to_string(),
            input: Some(Box::new(input)),
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
        self.input.as_deref().map_or("", TextInput::value)
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
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let children: Vec<_> = self
            .input
            .as_mut()
            .map(|input| input.build_layout(taffy, theme, node_map))
            .into_iter()
            .collect();
        #[allow(
            clippy::unwrap_used,
            reason = "fresh children on the same live taffy tree cannot fail short of OOM, and \
                      `Widget::build_layout` returns `NodeId` directly with no fallible path"
        )]
        let node = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &children,
            )
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(input) = &mut self.input {
            input.handle_event(event, ctx);
        }
    }

    fn name(&self) -> &'static str {
        "ModalBody"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.input
            .as_ref()
            .map(|input| input.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.input
            .as_mut()
            .map(|input| input.as_mut() as &mut dyn Widget)
            .into_iter()
            .collect()
    }
}
