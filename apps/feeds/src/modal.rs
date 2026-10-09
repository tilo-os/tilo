//! Bodies of the feeds dialogs, hosted inside the shared
//! [`ui::widget::ModalDialog`].
//!
//! The dialog owns the scrim, the sheet, its title, the footer actions,
//! Escape and scrim dismissal and the focus trap. A body adds what the
//! dialog asks: one sentence of guidance and, for a value, a real
//! [`TextInput`] (the shared caret, selection and clipboard), or only the
//! sentence for a confirmation.

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{AppModel, ModelPath, TaffyTreeExt, Text, TextInput, TextInputKind, Widget};

use crate::model::Id;

/// What a feeds dialog is asking for, so the submit handler knows which
/// mutation to run against the store and the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalIntent {
    /// Add a new subscription from the typed address.
    AddSubscription,
    /// Create a named folder in the sidebar.
    AddFolder,
    /// Rename an existing feed.
    RenameFeed { feed_id: Id },
    /// Confirm unsubscribing from a feed. No text input.
    Unsubscribe { feed_id: Id },
    /// Replace the selected article's comma-separated tags.
    EditArticleTags { article_id: Id },
}

impl ModalIntent {
    /// Whether this intent requires a non-empty field. Tag editing also owns
    /// a field, but an empty value deliberately means "remove all tags".
    pub fn requires_input(self) -> bool {
        matches!(
            self,
            ModalIntent::AddSubscription | ModalIntent::AddFolder | ModalIntent::RenameFeed { .. }
        )
    }
}

/// The content of a feeds dialog: a sentence of guidance over an optional
/// field.
pub struct ModalBody {
    caption: Text,
    // Boxed so the field keeps one retained-widget address for native
    // accessibility while the body moves.
    input: Option<Box<TextInput>>,
    rect: Rect,
    caption_node: Option<NodeId>,
    input_node: Option<NodeId>,
}

impl ModalBody {
    /// A sentence of guidance over a focused field announced as `label`.
    pub fn input(
        caption: &str,
        label: &str,
        initial: &str,
        placeholder: &str,
        kind: TextInputKind,
    ) -> Self {
        let mut input = TextInput::local()
            .with_kind(kind)
            .with_placeholder(placeholder)
            .with_accessible_name(label);
        if !initial.is_empty() {
            input = input.with_initial_value(initial);
            input.select_all();
        }
        // Seed focus for immediate typing while the host rebuilds the
        // retained modal tree and installs its focus trap.
        input.set_focus(true);
        Self {
            caption: Self::caption(caption),
            input: Some(Box::new(input)),
            rect: Rect::ZERO,
            caption_node: None,
            input_node: None,
        }
    }

    /// A confirmation: the sentence alone.
    pub fn message(caption: &str) -> Self {
        Self {
            caption: Self::caption(caption),
            input: None,
            rect: Rect::ZERO,
            caption_node: None,
            input_node: None,
        }
    }

    fn caption(text: &str) -> Text {
        Text::new(text).with_role(TextRole::Body).dimmed()
    }

    /// The field's value (trimmed by the caller); empty for a confirmation.
    pub fn value(&self) -> &str {
        self.input.as_deref().map_or("", TextInput::value)
    }

    /// Put `s` into the field, caret at its end.
    pub fn set_value(&mut self, s: &str) {
        if let Some(input) = &mut self.input {
            input.set_text(s);
            input.set_cursor_pos(s.len());
        }
    }
}

impl Widget for ModalBody {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if let Some(input) = &mut self.input {
            input.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let caption = self.caption.build_layout(taffy, theme, node_map);
        let mut children = vec![caption];
        self.caption_node = Some(caption);
        self.input_node = self.input.as_mut().map(|input| {
            let node = input.build_layout(taffy, theme, node_map);
            children.push(node);
            node
        });
        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            gap: Size {
                width: taffy::LengthPercentage::length(0.0),
                height: taffy::LengthPercentage::length(theme.tokens.spacing.rhythm.section),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &children, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::from_origin_size(
            (
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
            ),
            (f64::from(l.size.width), f64::from(l.size.height)),
        );
        let inner = self.rect.origin();
        if let Some(node) = self.caption_node {
            self.caption.apply_layout(taffy, node, inner);
        }
        if let (Some(input), Some(node)) = (self.input.as_mut(), self.input_node) {
            input.apply_layout(taffy, node, inner);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.caption.draw(scene, transform, text, theme, presence);
        if let Some(input) = &mut self.input {
            input.draw(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(input) = &mut self.input {
            input.handle_event(event, ctx);
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut out: Vec<&dyn Widget> = vec![&self.caption];
        if let Some(input) = &self.input {
            out.push(input.as_ref());
        }
        out
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut out: Vec<&mut (dyn Widget + '_)> = vec![&mut self.caption];
        if let Some(input) = &mut self.input {
            out.push(input.as_mut());
        }
        out
    }

    fn is_focusable(&self) -> bool {
        self.input
            .as_ref()
            .is_some_and(|input| input.is_focusable())
    }

    fn set_focus(&mut self, focused: bool) {
        if let Some(input) = &mut self.input {
            input.set_focus(focused);
        }
    }

    fn is_focused(&self) -> bool {
        self.input.as_ref().is_some_and(|input| input.is_focused())
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.input
            .as_ref()
            .and_then(|input| input.ime_cursor_rect())
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        self.input.as_ref().and_then(|input| input.ime_purpose())
    }

    fn name(&self) -> &'static str {
        "FeedsModalBody"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
