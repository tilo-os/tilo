//! Widgets that show a theme document: a miniature desktop drawn in it, and
//! the strip of its twelve presence colours. Both paint the *previewed*
//! theme ([`crate::draw`]); the studio's own theme only frames them.

use std::collections::HashMap;

use ui::advanced::{Affine, NodeId, Scene, TaffyTree};
use ui::prelude::*;
use ui::taffy::style_helpers::{length, percent};
use ui::taffy::{Size as TaffySize, Style};
use ui::theme::ThemeDocument;
use ui::tokens::PresenceHue;
use ui::vello::kurbo::RoundedRect;
use ui::vello::peniko::{BlendMode, Fill};
use ui::widget::TaffyTreeExt;

/// What a [`ThemePreview`] shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Show {
    /// A window on the desktop, with a floating panel over it.
    Desktop,
    /// A list row surface with selection and presence.
    List,
    /// A dialog over the window it asks about.
    Dialog,
    /// The twelve presence colours.
    Presence,
}

/// How tall a [`ThemePreview`] stands.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Height {
    /// A fixed height in logical pixels, at the full width it is given.
    Fixed(f32),
    /// As tall as the miniature's proportions make it at the width it is
    /// given, so it scales with its tile.
    Proportional,
}

pub struct ThemePreview {
    theme: ui::Theme,
    show: Show,
    label: String,
    rect: Rect,
    height: Height,
    /// It sits inside a card: its corners follow the card's, inset by the
    /// card's padding.
    in_card: bool,
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
            height: Height::Fixed(height),
            in_card: false,
            id: None,
        }
    }

    /// A miniature desktop in `document` that fills the width of the card
    /// it leads at the miniature's own proportions: it grows and shrinks
    /// with the card, and its corners are the card's, inset by its padding.
    #[must_use]
    pub fn tile(document: &ThemeDocument) -> Self {
        Self {
            height: Height::Proportional,
            in_card: true,
            ..Self::new(document, 0.0)
        }
    }

    /// Show a list surface, or a dialog, or the presence colours.
    #[must_use]
    pub fn show(mut self, show: Show) -> Self {
        self.show = show;
        self.label = match show {
            Show::Desktop => format!("Preview of {}", self.theme.name),
            Show::List => format!("List preview of {}", self.theme.name),
            Show::Dialog => format!("Dialog preview of {}", self.theme.name),
            Show::Presence => format!("Presence colours of {}", self.theme.name),
        };
        self
    }

    /// The presence colours of `document` instead of the desktop.
    #[must_use]
    pub fn presence(self) -> Self {
        self.show(Show::Presence)
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
        let style = match self.height {
            Height::Fixed(height) => Style {
                size: TaffySize {
                    width: percent(1.0_f32),
                    height: length(height * theme.ui_scale),
                },
                flex_shrink: 0.0,
                ..Style::default()
            },
            Height::Proportional => {
                let (cw, ch) = crate::draw::CANVAS;
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "the canvas is a small fixed proportion"
                )]
                let ratio = (cw / ch) as f32;
                Style {
                    size: TaffySize {
                        width: percent(1.0_f32),
                        height: ui::taffy::Dimension::auto(),
                    },
                    aspect_ratio: Some(ratio),
                    flex_shrink: 0.0,
                    ..Style::default()
                }
            }
        };
        taffy.widget_leaf(style, node_map, self)
    }

    ui::rect_geometry!();

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
            Show::List | Show::Dialog | Show::Desktop => {
                let (cw, ch) = crate::draw::CANVAS;
                let scale = (self.rect.width() / cw).min(self.rect.height() / ch);
                let size = (cw * scale, ch * scale);
                let origin = Point::new(
                    self.rect.x0 + (self.rect.width() - size.0) / 2.0,
                    self.rect.y0 + (self.rect.height() - size.1) / 2.0,
                );
                let frame = Rect::from_origin_size(origin, size);
                let radii = &theme.tokens.radii;
                let radius = f64::from(if self.in_card {
                    radii.concentric(radii.tile, theme.tokens.spacing.rhythm.section)
                } else {
                    radii.control
                });
                scene.push_layer(
                    Fill::NonZero,
                    BlendMode::default(),
                    1.0,
                    transform,
                    &RoundedRect::from_rect(frame, radius),
                );
                let miniature = match self.show {
                    Show::List => crate::draw::miniature_list,
                    Show::Dialog => crate::draw::miniature_dialog,
                    _ => crate::draw::miniature,
                };
                miniature(
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

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(ui::a11y::AccessNode::new(ui::a11y::Role::Image).with_name(self.label.clone()))
    }
}
