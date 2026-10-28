//! Document pages rasterised on demand, for every viewer that shows pages:
//! the PDF reader and the Typst preview draw through this one pipeline.
//!
//! - A frame asks for what it needs as one batch that replaces the worker's
//!   queue ([`queue::BatchQueue`]): the pages on screen first, then
//!   thumbnails, then a screen ahead in the scroll direction and half a screen
//!   behind ([`reach`]). A page scrolled past is never rendered ahead of one
//!   on screen.
//! - Rendering runs below the UI thread. The PDF reader's engine owner thread
//!   serves the queue among its other work; any other page source
//!   ([`PageSource`]) is served by [`PageRenderer`]. Either wakes the window
//!   when a result is ready.
//! - Bitmaps live in a byte-budgeted cache evicted least recently drawn,
//!   never evicting what is on screen ([`images::PageImages`]). A page draws
//!   the best bitmap held (another width, scaled) while the sharp one renders.
//! - A page drawn wider than [`tiles::TILE_MIN_PAGE_PX`] is cut into tiles at
//!   a discrete zoom level, so any zoom is sharp at bounded cost ([`tiles`]).

pub mod images;
pub mod queue;
pub mod reach;
pub mod tiles;
mod worker;

pub use worker::{PageRenderer, PageSource, Rendered};

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Color, Fill, ImageData};

/// The paper a page is printed on: document content, white in every theme.
pub const PAPER: Color = Color::WHITE;

/// Paint a page at `rect`: paper, then `image` stretched over it, then
/// `tiles` over that. A page none of whose pixels have arrived is paper.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "affine composition, not integer arithmetic"
)]
pub fn paint_page(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    image: Option<&ImageData>,
    tiles: Option<&tiles::PageTiles>,
) {
    scene.fill(Fill::NonZero, transform, PAPER, None, &rect);
    if let Some(image) = image
        && image.width > 0
        && image.height > 0
    {
        let sx = rect.width() / f64::from(image.width);
        let sy = rect.height() / f64::from(image.height);
        scene.draw_image(
            image,
            transform * Affine::translate((rect.x0, rect.y0)) * Affine::scale_non_uniform(sx, sy),
        );
    }
    if let Some(tiles) = tiles {
        tiles.paint(scene, transform);
    }
}

/// A displayed page (0-based) and the width bucket it was rendered at.
pub type PageKey = (usize, u32);

/// Pages are rasterised at this multiple of their on-screen size, so they stay
/// crisp when the compositor scales the surface and when zooming in a notch.
pub const SUPERSAMPLE: f64 = 2.0;

/// The width bucket a page render of `target_w` device pixels snaps to, so a
/// small zoom nudge reuses the bitmap held instead of rendering again.
#[must_use]
pub fn bucket_for(target_w: u32) -> u32 {
    (target_w / 24).max(1).saturating_mul(24).clamp(96, 4096)
}

/// How many renders not already asked for may join one frame's batch:
/// whole pages and thumbnails (renders at most [`tiles::THUMB_MAX_PX`] wide)
/// are counted apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Admission {
    pub pages: usize,
    pub thumbnails: usize,
}

impl Admission {
    /// What a frame admits: one new page (a real page can cost tens to
    /// hundreds of milliseconds, so the open→first-paint path pays for the
    /// top page alone and the rest are asked for as results wake later
    /// frames) and a couple of thumbnails, so a rail fills in briskly.
    pub const FRAME: Self = Self {
        pages: 1,
        thumbnails: 2,
    };
}

/// The batch a frame hands the worker, from the renders it wants in `order`
/// (most important first): each render once, those already `pending` keep
/// their place, and at most `admission` new ones join, so the open→first
/// paint path pays for the top page alone and the rest follow as results
/// wake later frames.
#[must_use]
pub fn admit<S: std::hash::BuildHasher>(
    order: &[PageKey],
    pending: &std::collections::HashSet<PageKey, S>,
    admission: Admission,
) -> Vec<PageKey> {
    let (mut new_pages, mut new_thumbs) = (admission.pages, admission.thumbnails);
    let mut batch: Vec<PageKey> = Vec::new();
    for key in order {
        if batch.contains(key) {
            continue;
        }
        if pending.contains(key) {
            batch.push(*key);
            continue;
        }
        let budget = if key.1 > tiles::THUMB_MAX_PX {
            &mut new_pages
        } else {
            &mut new_thumbs
        };
        if *budget > 0 {
            *budget = budget.saturating_sub(1);
            batch.push(*key);
        }
    }
    batch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_snap_and_stay_in_range() {
        assert_eq!(bucket_for(0), 96);
        assert_eq!(bucket_for(1000), 984);
        assert_eq!(bucket_for(1007), 984);
        assert_eq!(bucket_for(1008), 1008);
        assert_eq!(bucket_for(u32::MAX), 4096);
    }

    #[test]
    fn a_batch_keeps_pending_renders_and_admits_a_few_new_ones() {
        let pending: std::collections::HashSet<PageKey> = [(3, 960)].into_iter().collect();
        let order = [
            (1, 960),
            (2, 960),
            (3, 960),
            (1, 960),
            (0, 120),
            (1, 120),
            (2, 120),
        ];
        let batch = admit(
            &order,
            &pending,
            Admission {
                pages: 1,
                thumbnails: 2,
            },
        );
        assert_eq!(batch, vec![(1, 960), (3, 960), (0, 120), (1, 120)]);
    }
}
