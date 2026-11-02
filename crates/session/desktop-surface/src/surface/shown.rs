//! Where the shown strip's windows stand, as the last layout left them: the
//! whole of each window, and the bounds of the tab it shows.
use ui::kurbo::Rect;
use ui::widget::{GroupRow, Widget as _};

use super::{DesktopHandle, TabId};

impl DesktopHandle {
    /// Where each window of the shown strip draws its active tab, in the
    /// surface's coordinates, as of the last layout. Hidden tabs and other
    /// strips are absent.
    pub fn shown_tabs(&self) -> Vec<(TabId, Rect)> {
        self.shared.borrow().shown_tabs.content()
    }

    /// The top of the shown windows' content, in the surface's coordinates,
    /// as of the last layout: where the transients pinned into the display's
    /// top-right corner start, below the windows' bars. Zero with no window.
    pub fn content_top(&self) -> f64 {
        self.shared.borrow().shown_tabs.content_top()
    }

    /// Where the whole window of `tab` stands — its bar, and the tab's
    /// content in it — in the surface's coordinates, as of the last layout:
    /// where a menu for it hangs from.
    #[must_use]
    pub fn window_rect(&self, tab: TabId) -> Option<Rect> {
        self.shared.borrow().shown_tabs.window_rect(tab)
    }
}

/// Where a window of the shown strip stands, and the bounds of the tab it
/// shows.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ShownTab {
    /// The tab the window shows: the active one of its bar.
    tab: TabId,
    /// The window's whole frame: its bar, and the tab's content in it.
    window: Rect,
    /// The bounds of the tab's content, once its window laid it out.
    content: Option<Rect>,
}

/// What the shown strip holds, window by window, as the last layout left it.
#[derive(Debug, Default)]
pub(super) struct ShownTabs(Vec<ShownTab>);

impl ShownTabs {
    /// The windows of `row`, as its groups were just laid out.
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

    /// Where each window draws its active tab: absent while its app's window
    /// is not laid out.
    pub(super) fn content(&self) -> Vec<(TabId, Rect)> {
        self.0
            .iter()
            .filter_map(|shown| Some((shown.tab, shown.content?)))
            .collect()
    }

    /// Where the whole window of `tab` stands, its bar and content together.
    pub(super) fn window_rect(&self, tab: TabId) -> Option<Rect> {
        let shown = self.0.iter().find(|shown| shown.tab == tab)?;
        Some(shown.window)
    }

    /// The top of the highest window's content: where banners start, below
    /// the windows' bars, which pinned to the strip's top would cover their
    /// own controls.
    pub(super) fn content_top(&self) -> f64 {
        self.0
            .iter()
            .filter_map(|shown| shown.content)
            .map(|content| content.y0)
            .fold(0.0, f64::max)
    }
}
