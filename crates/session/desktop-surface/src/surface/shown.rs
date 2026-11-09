//! Where the windows the desk shows stand, as the last layout left them: the whole of each window,
//! and the bounds of the tab it shows. Every display shows a strip of its own, so that is every row
//! the desk draws, not only the one the person works in.
use ui::kurbo::Rect;
use ui::widget::{GroupRow, Widget as _};

use super::{DesktopHandle, TabId};

impl DesktopHandle {
    /// Where each window the desk draws draws its active tab, in the surface's coordinates, as of
    /// the last layout. Hidden tabs are absent, and so is a window whose display shows another
    /// strip than its own.
    pub fn shown_tabs(&self) -> Vec<(TabId, Rect)> {
        self.shared.borrow().shown_tabs.content()
    }

    /// Where each drawn window's active tab stands, in the surface's coordinates, as of the last
    /// layout: the tab's content where it has laid out, and the whole window where it has not (a
    /// window this device shows but has not opened, whose position an offset on it still means
    /// something across). Hidden tabs are absent.
    pub fn shown_tiles(&self) -> Vec<(TabId, Rect)> {
        self.shared.borrow().shown_tabs.tiles()
    }

    /// The top of the drawn windows' content, in the surface's coordinates,
    /// as of the last layout: where the transients pinned into the display's
    /// top-right corner start, below the windows' bars. Zero with no window.
    pub fn content_top(&self) -> f64 {
        self.shared.borrow().shown_tabs.content_top()
    }

    /// Each window floating over the desk, and the rect the desk draws over the windows under it
    /// for that window — its bar, its body and the shadow that lifts it, whole and opaque — in the
    /// surface's coordinates, as of the last draw. A window that has not finished fading in is
    /// absent: what the desk draws of it yet lets the windows it covers show through.
    pub fn float_covers(&self) -> Vec<(TabId, Rect)> {
        self.shared.borrow().float_covers.clone()
    }

    /// Where the whole window of `tab` stands — its bar, and the tab's content in it — in the
    /// surface's coordinates, as of the last layout: where a menu for it hangs from.
    #[must_use]
    pub fn window_rect(&self, tab: TabId) -> Option<Rect> {
        self.shared.borrow().shown_tabs.window_rect(tab)
    }
}

/// Where a window the desk draws stands, and the bounds of the tab it shows.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ShownTab {
    /// The tab the window shows: the active one of its bar.
    tab: TabId,
    /// The window's whole frame: its bar, and the tab's content in it.
    window: Rect,
    /// The bounds of the tab's content, once its window laid it out.
    content: Option<Rect>,
}

/// What one strip holds, window by window, as the last layout left it.
#[derive(Debug, Default)]
pub(super) struct ShownTabs(Vec<ShownTab>);

impl ShownTabs {
    /// What the desk lays out now: every row it draws, window by window.
    pub(super) fn of_drawn(surface: &super::DesktopSurface) -> Self {
        surface
            .drawn_strips()
            .into_iter()
            .filter_map(|strip| surface.rows.get(&strip))
            .map(Self::of_row)
            .reduce(Self::and)
            .unwrap_or_default()
    }

    /// The windows of `row` , as its groups were just laid out.
    pub(super) fn of_row(row: &GroupRow) -> Self {
        Self(
            row.groups
                .iter()
                .map(|group| {
                    let tab = group.active_tab_id();
                    ShownTab {
                        tab,
                        window: group.get_rect(),
                        content: group.content_rect_for_tab(tab),
                    }
                })
                .collect(),
        )
    }

    /// These windows and `other` 's together: every row the desk draws. A window in both (a group
    /// on its way from one strip to another) keeps `other` 's place, the row drawn over it.
    pub(super) fn and(mut self, other: Self) -> Self {
        for shown in other.0 {
            match self.0.iter_mut().find(|have| have.tab == shown.tab) {
                Some(have) => *have = shown,
                None => self.0.push(shown),
            }
        }
        self
    }

    /// Where each window draws its active tab: absent while its app's window is not laid out.
    pub(super) fn content(&self) -> Vec<(TabId, Rect)> {
        self.0
            .iter()
            .filter_map(|shown| Some((shown.tab, shown.content?)))
            .collect()
    }

    /// Where each window of the shown strip's active tab stands: the tab's content where its window
    /// laid one out, and the whole window where it has not. A window shown without its content laid
    /// out (one this device has open bit does not draw here) still has a place on the desk, and
    /// what is drawn on its tile — another person's pointer — keeps meaning there.
    pub(super) fn tiles(&self) -> Vec<(TabId, Rect)> {
        self.0
            .iter()
            .map(|shown| (shown.tab, shown.content.unwrap_or(shown.window)))
            .collect()
    }

    /// Where the whole window of `tab` stands, its bar and content together.
    pub(super) fn window_rect(&self, tab: TabId) -> Option<Rect> {
        let shown = self.0.iter().find(|shown| shown.tab == tab)?;
        Some(shown.window)
    }

    /// The top of the highest window's content: where banners start, below the windows' bars, which
    /// pinned to the strip's top would cover their own controls.
    pub(super) fn content_top(&self) -> f64 {
        self.0
            .iter()
            .filter_map(|shown| shown.content)
            .map(|content| content.y0)
            .fold(0.0, f64::max)
    }
}
