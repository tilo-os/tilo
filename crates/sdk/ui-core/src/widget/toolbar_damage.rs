//! What a toolbar repaints: the commands, separators and More button whose
//! face changed (a label, a toggle, a hover, the focus ring), not the row.
//! Hosted controls say what of them changed themselves.

use std::hash::{Hash, Hasher};

use vello::kurbo::{Affine, Rect, Vec2};
use vello::peniko::Color;

use super::{Target, Toolbar, ToolbarItem};
use crate::scroll_copy::{Piece, Showing, Since};
use crate::theme::Theme;

/// How one frame showed the row: each command's face as a piece.
pub(super) type BarShown = Showing<BarLook>;

/// What the whole row's paint depends on besides its commands' faces.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct BarLook {
    rect: Rect,
    colors: [Color; 4],
}

fn bits(rect: Rect) -> [u64; 4] {
    [rect.x0, rect.y0, rect.x1, rect.y1].map(f64::to_bits)
}

impl Toolbar {
    /// Note what this frame shows, once its draw is done.
    pub(super) fn note_shown(&self, transform: Affine, theme: &Theme) {
        let face = |target: Target, hasher: &mut rustc_hash::FxHasher| {
            (
                self.hovered == Some(target),
                self.pressed == Some(target),
                self.focused && self.focus == Some(target) && !self.menu.is_open(),
            )
                .hash(hasher);
        };
        let mut pieces = Vec::new();
        for (index, item) in self.items.iter().enumerate() {
            let Some(rect) = self
                .item_rects
                .get(index)
                .copied()
                .filter(|rect| !rect.is_zero_area())
            else {
                continue;
            };
            let mut hasher = rustc_hash::FxHasher::default();
            bits(rect).hash(&mut hasher);
            match item {
                ToolbarItem::Button(command) => {
                    (
                        &command.label,
                        command.icon,
                        command
                            .swatch
                            .map(|color| color.components.map(f32::to_bits)),
                        command.glyph.map(|paint| paint as usize),
                        command.active,
                        command.is_enabled(),
                        command.dot,
                        &command.accessible_name,
                        self.item_menu.as_ref().is_some_and(|(i, _)| *i == index),
                    )
                        .hash(&mut hasher);
                    face(Target::Item(index), &mut hasher);
                }
                ToolbarItem::Separator => self.separator_shown(index).hash(&mut hasher),
                ToolbarItem::Spacer | ToolbarItem::Element(_) => continue,
            }
            pieces.push(Piece {
                key: u64::try_from(index).unwrap_or(u64::MAX - 1),
                rect,
                paint: hasher.finish(),
            });
        }
        if self.has_overflow() {
            let mut hasher = rustc_hash::FxHasher::default();
            bits(self.overflow_rect).hash(&mut hasher);
            face(Target::Overflow, &mut hasher);
            pieces.push(Piece {
                key: u64::MAX,
                rect: self.overflow_rect,
                paint: hasher.finish(),
            });
        }
        let colors = &theme.tokens.colors;
        self.shown.note(Showing {
            scroll: Vec2::ZERO,
            clip: self.rect,
            transform: crate::scroll_copy::on_screen(transform),
            look: BarLook {
                rect: self.rect,
                colors: [
                    colors.surfaces.content,
                    colors.text_primary,
                    theme.text_dim,
                    colors.intent.accent_ink,
                ],
            },
            overlays: Vec::new(),
            pieces,
            fixed: Vec::new(),
        });
    }

    /// The row's damage since the frame on screen: the faces that changed
    /// where they were and are, and what its hosted controls damaged.
    pub(super) fn collect_shown_damage(&self, regions: &mut Vec<Rect>) {
        if self.shown.damage(self, regions) == Since::Repaint {
            regions.push(self.rect);
        }
        for item in &self.items {
            if let ToolbarItem::Element(element) = item {
                element.collect_damage(regions);
            }
        }
    }
}
