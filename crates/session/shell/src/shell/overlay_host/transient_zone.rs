//! The reserved zone transients go in: banners, the follow bar and anything
//! else that comes and goes over the windows stand on the trailing side,
//! below the top bar, and never on a window's chrome. Where one would cover
//! a window's bar the bar wins and the transient shifts below it
//! ([`tilo_desktop::notifications::transient_slot`]): the controls the
//! person uses all day (tabs, close, maximize, the header row's sidebar
//! toggle and History) are never under something that came by.
//!
//! A window wears two bars: its group's tab strip, and its own header row
//! inside the window — the one that holds that window's controls (HIG §1,
//! §9.1). Both are chrome, and a transient that hangs over the header row
//! hides a live control exactly as one over the tab strip would.

use vello::kurbo::Rect;

use ui::theme::Theme;
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

    /// Every window's chrome bars on screen, in layout coordinates: what no
    /// transient may cover. Each group's tab strip, and the header row of
    /// the window it shows.
    pub(crate) fn chrome_bar_rects(&self, theme: &Theme) -> Vec<Rect> {
        let screen = self.rect;
        let header = ui::chrome::HeaderRowMetrics::resolve(theme).height;
        self.group_row_ptrs
            .values()
            .flat_map(|&ptr| {
                // SAFETY: same stable boxed-row contract as the other
                // `group_row_ptrs` accessors; this is a read-only query.
                let row: &GroupRow = unsafe { &*ptr };
                row.groups.iter().flat_map(move |group| {
                    let client = group.presentation_bounds().client;
                    [
                        Some(group.bar_rect()),
                        client.map(|client| {
                            Rect::new(client.x0, client.y0, client.x1, client.y0 + header)
                        }),
                    ]
                    .into_iter()
                    .flatten()
                })
            })
            .filter(|bar| bar.area() > 0.0 && bar.intersect(screen).area() > 0.0)
            .collect()
    }
}
