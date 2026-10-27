//! What a widget paints itself, kept from one frame to the next, so the
//! frame damages the widget's box only where it painted differently: a hover
//! in the sidebar leaves the file list, the path bar and the rename editor
//! as they were, and repaints none of them.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

/// One draw: where it painted, and what.
#[derive(Clone, PartialEq)]
struct Drawn {
    rect: Rect,
    transform: Affine,
    paint: ui::scroll_copy::Painted,
}

/// A widget's own paint this frame and as the last presented frame showed
/// it.
#[derive(Default)]
pub(crate) struct OwnPaint {
    drawn: Option<Drawn>,
    presented: Option<Drawn>,
}

impl OwnPaint {
    /// Paint with `draw` into `scene`, keeping what it painted. `draw`
    /// returns the box it painted within. Not drawing in a frame paints
    /// nothing there: [`Self::skip`].
    pub(crate) fn record(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        draw: impl FnOnce(&mut Scene) -> Rect,
    ) {
        let mut own = Scene::new();
        let rect = draw(&mut own);
        scene.append(&own, None);
        self.drawn = Some(Drawn {
            rect,
            transform,
            paint: ui::scroll_copy::Painted(own),
        });
    }

    /// This frame painted nothing of it.
    pub(crate) fn skip(&mut self) {
        self.drawn = None;
    }

    /// Where this frame's paint differs from the last presented frame's:
    /// the box it paints in now and the one it painted in then.
    pub(crate) fn damage(&self, regions: &mut Vec<Rect>) {
        if self.drawn != self.presented {
            regions.extend(
                [&self.drawn, &self.presented]
                    .into_iter()
                    .flatten()
                    .map(|drawn| drawn.rect),
            );
        }
    }

    /// The frame was presented: what it painted is what the screen shows.
    pub(crate) fn present(&mut self) {
        self.presented = self.drawn.take();
    }
}
