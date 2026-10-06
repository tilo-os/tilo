//! Widgets that show a theme document: a miniature desktop drawn in it, and
//! the strip of its twelve presence colours. Both paint the *previewed*
//! theme ([`crate::draw`]); the studio's own theme only frames them.

use std::collections::HashMap;

use ui::prelude::*;
use ui::taffy::style_helpers::{length, percent};
use ui::taffy::{Size as TaffySize, Style};
use ui::theme::ThemeDocument;
use ui::tokens::PresenceHue;
use ui::vello::kurbo::RoundedRect;
use ui::vello::peniko::{BlendMode, Fill};

/// What a [`ThemePreview`] shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Show {
    /// A window on the desktop, with a floating panel over it.
    Desktop,
    /// The twelve presence colours.
    Presence,
}

/// A preview of one theme document.
pub struct ThemePreview {
    theme: ui::Theme,
    show: Show,
    label: String,
    rect: Rect,
    width: Option<f32>,
    height: f32,
    id: Option<String>,
}

impl ThemePreview {
    /// A miniature desktop in `document`, `height` logical pixels tall. A
    /// document this build does not admit previews the default theme.
    #[must_use]
    pub fn new(document: &ThemeDocument, height: f32) -> Self {
        let theme = ui::Theme::from_document(document).unwrap_or_default();
        Self {
            label: format!("Preview of {}", theme.name),
            theme: theme.with_your_colour(PresenceHue::Violet),
            show: Show::Desktop,
            rect: Rect::ZERO,
            width: None,
            height,
            id: None,
        }
    }

    /// The presence colours of `document` instead of the desktop.
    #[must_use]
    pub fn presence(mut self) -> Self {
        self.show = Show::Presence;
        self.label = format!("Presence colours of {}", self.theme.name);
        self
    }

    /// A fixed width instead of the parent's.
    #[must_use]
    pub fn with_width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }
}

impl Widget for ThemePreview {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let scale = theme.ui_scale;
        let style = Style {
            size: TaffySize {
                width: self
                    .width
                    .map_or_else(|| percent(1.0_f32), |w| length(w * scale)),
                height: length(self.height * scale),
            },
            flex_shrink: 0.0,
            ..Style::default()
        };
        let Ok(node) = taffy.new_leaf(style) else {
            return NodeId::from(0_u64);
        };
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::from_origin_size(
            (
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
            ),
            (f64::from(layout.size.width), f64::from(layout.size.height)),
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
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        match self.show {
            Show::Presence => {
                crate::draw::presence_strip(scene, transform, self.rect, &self.theme);
            }
            Show::Desktop => {
                let (cw, ch) = crate::draw::CANVAS;
                let scale = (self.rect.width() / cw).min(self.rect.height() / ch);
                let size = (cw * scale, ch * scale);
                let origin = Point::new(
                    self.rect.x0 + (self.rect.width() - size.0) / 2.0,
                    self.rect.y0 + (self.rect.height() - size.1) / 2.0,
                );
                let frame = Rect::from_origin_size(origin, size);
                let radius = f64::from(theme.tokens.radii.control);
                scene.push_layer(
                    Fill::NonZero,
                    BlendMode::default(),
                    1.0,
                    transform,
                    &RoundedRect::from_rect(frame, radius),
                );
                crate::draw::miniature(
                    scene,
                    transform * Affine::translate(origin.to_vec2()) * Affine::scale(scale),
                    text_renderer,
                    &self.theme,
                );
                scene.pop_layer();
            }
        }
    }

    fn name(&self) -> &'static str {
        "ThemePreview"
    }

    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(ui::a11y::AccessNode::new(ui::a11y::Role::Image).with_name(self.label.clone()))
    }
}
