//! What the preview repaints. A frame that only scrolled the pages offers
//! its host a copy of what stays on screen ([`ui::scroll_copy`]) and repaints
//! the strip it exposed; pages whose pixels arrived repaint where they are
//! drawn, the thumbnail strip where it changed, the scroll bars where they
//! moved or lit up. Anything else repaints the pane.

use std::hash::{Hash, Hasher};

use ui::scroll_copy::{Piece, Showing, Since};
use ui::theme::Theme;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Color;

use super::PreviewPane;
use super::raster::FramePages;

/// How one frame showed the preview: its pages as pieces, the scroll bars'
/// thumbs and the thumbnail strip as fixed pieces.
pub(super) type PreviewShown = Showing<PreviewLook>;

/// What the whole pane's paint depends on besides the scroll, the pages'
/// pixels, the bars and the strip.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PreviewLook {
    rect: Rect,
    zoom: u64,
    content: (u64, u64),
    /// The compile shown, by address.
    document: usize,
    selection: Option<(usize, usize)>,
    stale: bool,
    colors: [Color; 4],
}

impl PreviewPane {
    /// Note what this frame shows, drawn from `frame` with the thumbnail
    /// `slots`.
    pub(super) fn note_shown(
        &self,
        transform: Affine,
        theme: &Theme,
        frame: &FramePages,
        slots: &[(Rect, Rect)],
    ) {
        let main = self.viewport.rect();
        let scroll = -self.viewport.content_origin().to_vec2();
        let pieces = self
            .pages_between(main.y0, main.y1)
            .map(|page| {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                if let Some(image) = frame.pages.get(&page) {
                    (image.data.id(), image.width, image.height).hash(&mut hasher);
                }
                if let Some(tiles) = frame.tiles.get(&page) {
                    for (scale, at, image) in &tiles.under {
                        (scale.to_bits(), at, image.data.id()).hash(&mut hasher);
                    }
                    for (at, image) in &tiles.tiles {
                        (at, image.data.id()).hash(&mut hasher);
                    }
                }
                Piece {
                    key: u64::try_from(page).unwrap_or(u64::MAX),
                    rect: self.page_rect(page).unwrap_or_default() + scroll,
                    paint: hasher.finish(),
                }
            })
            .collect();
        let bars = {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            for (thumb, lit) in self.viewport.scrollbar_thumbs() {
                (
                    thumb.map(|thumb| [thumb.x0, thumb.y0, thumb.x1, thumb.y1].map(f64::to_bits)),
                    lit,
                )
                    .hash(&mut hasher);
            }
            hasher.finish()
        };
        let mut fixed: Vec<Piece> = self
            .viewport
            .scrollbar_lanes()
            .into_iter()
            .zip(0..)
            .map(|(lane, key)| Piece {
                key,
                rect: lane,
                paint: bars,
            })
            .collect();
        if self.has_thumbs() {
            let strip = self.thumb_strip();
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            self.current.hash(&mut hasher);
            for (i, (image, label)) in slots.iter().enumerate() {
                if image.y0 > strip.y1 {
                    break;
                }
                [image.x0, image.y0, image.x1, image.y1, label.y0]
                    .map(f64::to_bits)
                    .hash(&mut hasher);
                if let Some(thumb) = frame.thumbs.get(&i) {
                    thumb.data.id().hash(&mut hasher);
                }
            }
            fixed.push(Piece {
                key: 2,
                rect: strip,
                paint: hasher.finish(),
            });
        }
        let colors = &theme.tokens.colors;
        let (content_w, content_h) = self.viewport.content_size();
        self.shown.note(Showing {
            scroll,
            clip: main,
            transform: ui::scroll_copy::on_screen(transform),
            look: PreviewLook {
                rect: self.rect,
                zoom: self.viewport.zoom().to_bits(),
                content: (content_w.to_bits(), content_h.to_bits()),
                document: self.raster.document_address(),
                selection: self
                    .has_selection()
                    .then(|| (self.selection.min(), self.selection.max())),
                stale: self.stale,
                colors: [
                    colors.surfaces.content,
                    colors.text_dim,
                    colors.text_primary,
                    colors.intent.selection_tint,
                ],
            },
            overlays: self.viewport.scrollbar_lanes(),
            pieces,
            fixed,
        });
    }

    /// The preview's damage since the frame on screen, as part of `owner`,
    /// the widget in the tree it is drawn in.
    pub fn collect_damage_for<T: ?Sized>(&self, owner: &T, regions: &mut Vec<Rect>) {
        if self.shown.damage(owner, regions) == Since::Repaint {
            regions.push(self.rect);
        }
    }

    /// From the host's `mark_drawn`: the frame just drawn is on screen.
    pub fn mark_shown(&mut self) {
        self.shown.present();
    }
}
