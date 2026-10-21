//! The window's loading line in its tile ([`crate::window_loading`]): what the
//! tile's app reports, where the line stands, and its own damage part.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::TileHost;
use crate::theme::Theme;

impl TileHost {
    /// What the app this tile shows says it is doing: busy, loading with
    /// progress, or nothing ([`crate::window_loading`]). The tile draws the
    /// window's loading line from it. Returns whether that changed anything.
    pub fn set_loading(&mut self, loading: crate::window_loading::WindowLoading) -> bool {
        self.loading.set(loading, crate::motion::frame_now())
    }

    /// The window's loading line.
    pub fn loading(&self) -> &crate::window_loading::LoadingLine {
        &self.loading
    }

    /// Where the window's loading line stands: along the top of the content,
    /// directly under the window's bar. When the app's own header row holds
    /// the bar, under that row.
    pub fn loading_band(&self, theme: &Theme) -> Rect {
        let top = self
            .window_bar_slot()
            .filter(|slot| !slot.hides_with_bar)
            .map_or(self.rect.y0, |slot| slot.row.y1);
        crate::window_loading::band(self.rect, top, theme)
    }

    /// The window's loading line, a damage part of its own: animating it
    /// repaints its band and nothing of the content. Drawn inside the
    /// tile's clip, so it keeps the content's shape. The part is there
    /// whether or not the line shows, so it coming and going changes only
    /// its band.
    pub(super) fn draw_loading(&mut self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let band = self.loading_band(theme);
        let key = crate::scene_parts::PartKey::new("ui.tile-loading", ());
        let line = &mut self.loading;
        crate::scene_parts::part_drawn(scene, key, |scene| {
            line.draw(scene, transform, theme, band)
        });
    }
}
