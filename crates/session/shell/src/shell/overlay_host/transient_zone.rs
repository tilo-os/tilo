//! The reserved zone transients go in: banners, the follow bar and anything
//! else that comes and goes over the windows stand on the trailing side,
//! below the top bar, and never on a window's bar. Where one would cover a
//! window's bar the bar wins and the transient shifts below it
//! ([`tilo_desktop::notifications::transient_slot`]): the controls the
//! person uses all day (tabs, close, maximize) are never under something
//! that came by.

use vello::kurbo::Rect;

use ui::widget::GroupRow;

impl super::ShellOverlayHost {
    /// The area under the top bar of the display transients open on.
    pub(super) fn transient_viewport(&self) -> Rect {
        let area = self.overlay_area();
        Rect::new(
            area.x0,
            area.y0 + f64::from(self.chrome_top_y),
            area.x1,
            area.y1,
        )
    }

    /// Every window bar on screen, in layout coordinates: what no transient
    /// may cover.
    pub(crate) fn window_bar_rects(&self) -> Vec<Rect> {
        let screen = self.rect;
        self.group_row_ptrs
            .values()
            .flat_map(|&ptr| {
                // SAFETY: same stable boxed-row contract as the other
                // `group_row_ptrs` accessors; this is a read-only query.
                let row: &GroupRow = unsafe { &*ptr };
                row.groups.iter().map(ui::widget::WindowGroup::bar_rect)
            })
            .filter(|bar| bar.area() > 0.0 && bar.intersect(screen).area() > 0.0)
            .collect()
    }
}
