//! Where the windows the shell shows stand, as the last layout left them: the whole of each window,
//! and the bounds of the tab it shows. Every display shows a strip of its own, so that is every row
//! the shell draws, not only the one the person works in.
use ui::kurbo::Rect;

use super::{DesktopHandle, GroupId, TabId};

impl DesktopHandle {
    /// Where each window the shell draws draws its active tab, in the surface's coordinates, as of
    /// the last layout. Hidden tabs are absent, and so is a window whose display shows another
    /// strip than its own.
    pub fn shown_tabs(&self) -> Vec<(TabId, Rect)> {
        self.shared.borrow().shown_tabs.content()
    }

    /// Make the next press carry the window it lands in from anywhere in it, as a press on its bar
    /// does: the press at `point` (the surface's coordinates) was made with Super held. Whether a
    /// window the shell draws stands there to carry.
    pub fn carry_next_press_at(&self, point: ui::kurbo::Point) -> bool {
        let mut shared = self.shared.borrow_mut();
        let under = shared
            .shown_tabs
            .tiles()
            .iter()
            .any(|(_, rect)| rect.contains(point));
        shared.carry_press = under.then_some(point);
        under
    }

    /// Where each drawn window's active tab stands, in the surface's coordinates, as of the last
    /// layout: the tab's content where it has laid out, and the whole window where it has not (a
    /// window this device shows but has not opened, whose position an offset on it still means
    /// something across). Hidden tabs are absent.
    pub fn shown_tiles(&self) -> Vec<(TabId, Rect)> {
        self.shared.borrow().shown_tabs.tiles()
    }

    /// Whether a window the shell draws shows `item` of `app` in the tab in front, as of the last
    /// layout: the window it opened on ([`DesktopHandle::set_window_items`]) stands on screen, so
    /// what lands in the item is before the person.
    #[must_use]
    pub fn shows_item(&self, app: &str, item: &str) -> bool {
        let shown = self.shown_tabs();
        let items = self.shared.borrow().items.clone();
        self.read(|tree| {
            tree.tabs().any(|tab| {
                items.get(&tab.id).is_some_and(|opened| opened == item)
                    && shown.iter().any(|(id, _)| *id == tab.id)
                    && tilo_desktop_model::apps::canonical_app_id(&tab.tile)
                        .is_some_and(|id| tilo_desktop_model::apps::app_ids_match(&id, app))
            })
        })
    }

    /// The top of the drawn windows' content, in the surface's coordinates,
    /// as of the last layout: where the transients pinned into the display's
    /// top-right corner start, below the windows' bars. Zero with no window.
    pub fn content_top(&self) -> f64 {
        self.shared.borrow().shown_tabs.content_top()
    }

    /// The window covering its display whose own bar stands over its content, as the person
    /// reached for the bar over the display, and where that bar stands, in the surface's
    /// coordinates, as of the last layout. The window is drawn around it, and a press there is the
    /// bar's.
    pub fn covering_bar(&self) -> Option<(TabId, Rect)> {
        self.shared.borrow().shown_tabs.bar_over_content()
    }

    /// Each window floating over the shell, and the rect the shell draws over the windows under it
    /// for that window — its bar, its body and the shadow that lifts it, whole and opaque — in the
    /// surface's coordinates, as of the last draw. A window that has not finished fading in is
    /// absent: what the shell draws of it yet lets the windows it covers show through.
    pub fn float_covers(&self) -> Vec<(TabId, Rect)> {
        self.shared.borrow().float_covers.clone()
    }

    /// The application the window of `tab` runs, or `None` for a window that is not an
    /// application's.
    #[must_use]
    pub fn app_of(&self, tab: TabId) -> Option<String> {
        crate::desktop::host::records::app_of(&self.shared.borrow().tree, tab).map(str::to_owned)
    }

    /// Where the whole window of `tab` stands — its bar, and the tab's content in it — in the
    /// surface's coordinates, as of the last layout: where a menu for it hangs from.
    #[must_use]
    pub fn window_rect(&self, tab: TabId) -> Option<Rect> {
        self.shared.borrow().shown_tabs.window_rect(tab)
    }

    /// Each window group's size on the display it is laid out on, as of the last layout: the tab's
    /// content — the pixels the window draws — where the window has laid it out, and the whole
    /// window where it has not. A group whose row is not drawn (its strip is not on screen, or its
    /// display shows another strip) is not here; a host pictures it at the size its share of the
    /// strip would give it. A picture of the window fills its card by this size: a card drawn from
    /// it stands at the window's own aspect, so what the window shows fills the card rather than
    /// leaving bars of ground round a smaller picture.
    #[must_use]
    pub fn group_sizes(&self) -> Vec<(GroupId, ui::kurbo::Size)> {
        self.shared.borrow().shown_tabs.sizes()
    }
}

/// Where a window the shell draws stands, and the bounds of the tab it shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShownTab {
    /// The tab the window shows: the active one of its bar.
    pub tab: TabId,
    /// The window group's own id, which a picture of it is named by.
    pub group: GroupId,
    /// The window's whole frame: its bar, and the tab's content in it.
    pub window: Rect,
    /// The bounds of the tab's content, once its window laid it out.
    pub content: Option<Rect>,
    /// The window's bar where it stands over that content: a window covering its display shows it
    /// there while the person reaches for the bar over the display.
    pub bar_over_content: Option<Rect>,
    /// Whether the window floats over the shell rather than laid out in its strip.
    pub floated: bool,
}

/// What one strip holds, window by window, as the last layout left it.
#[derive(Debug, Default)]
pub struct ShownTabs(pub Vec<ShownTab>);

impl ShownTabs {
    /// These windows and `other` 's together: every row the shell draws. A window in both (a group
    /// on its way from one strip to another) keeps `other` 's place, the row drawn over it.
    #[must_use]
    pub fn and(mut self, other: Self) -> Self {
        for shown in other.0 {
            match self.0.iter_mut().find(|have| have.tab == shown.tab) {
                Some(have) => *have = shown,
                None => self.0.push(shown),
            }
        }
        self
    }

    /// Where each window draws its active tab: absent while its app's window is not laid out.
    pub fn content(&self) -> Vec<(TabId, Rect)> {
        self.0
            .iter()
            .filter_map(|shown| Some((shown.tab, shown.content?)))
            .collect()
    }

    /// Where each window of the shown strip's active tab stands: the tab's content where its window
    /// laid one out, and the whole window where it has not. A window shown without its content laid
    /// out (one this device has open bit does not draw here) still has a place on the shell, and
    /// what is drawn on its tile — another person's pointer — keeps meaning there.
    pub fn tiles(&self) -> Vec<(TabId, Rect)> {
        self.0
            .iter()
            .map(|shown| (shown.tab, shown.content.unwrap_or(shown.window)))
            .collect()
    }

    /// The window whose bar stands over its own content, and where: the shell draws there over
    /// what the window shows.
    pub fn bar_over_content(&self) -> Option<(TabId, Rect)> {
        self.0
            .iter()
            .find_map(|shown| Some((shown.tab, shown.bar_over_content?)))
    }

    /// Where the whole window of `tab` stands, its bar and content together.
    pub fn window_rect(&self, tab: TabId) -> Option<Rect> {
        let shown = self.0.iter().find(|shown| shown.tab == tab)?;
        Some(shown.window)
    }

    /// Each window group's size on its display: the tab's content where the window laid it out —
    /// the pixels the window draws, which a picture of it shows — and the whole window where it has
    /// not.
    pub fn sizes(&self) -> Vec<(GroupId, ui::kurbo::Size)> {
        self.0
            .iter()
            .map(|shown| (shown.group, shown.content.unwrap_or(shown.window).size()))
            .collect()
    }

    /// The top of the strips' windows' content: where banners start, below the windows' bars, which
    /// pinned to the strip's top would cover their own controls. A floating window stands lower,
    /// and banners keep clear of it whole ([`DesktopHandle::float_covers`]) rather than start
    /// under its bar, over what it shows.
    pub fn content_top(&self) -> f64 {
        self.0
            .iter()
            .filter(|shown| !shown.floated)
            .filter_map(|shown| shown.content)
            .map(|content| content.y0)
            .fold(0.0, f64::max)
    }
}

impl DesktopHandle {
    /// The app of `tab` left a sideways wheel to the shell: its content did not scroll with it, so
    /// the strip its window is on moves by `travel` logical pixels, positive toward later windows,
    /// as the wheel over the strip's own canvas does. What the app scrolled with itself never
    /// arrives here, so a gesture is never split between its content and the strip: the app decided
    /// at its first event ([`ui::widget::sideways_travel`], and the app's own server of an event it
    /// left).
    pub fn sideways_wheel(&self, tab: TabId, travel: f64) {
        let mut shared = self.shared.borrow_mut();
        shared.sideways.push((tab, travel));
        // Where the windows are is nothing the workspace publishes: the shell draws the moved strip
        // in the frame this asks for, and nothing else.
        shared.wake.wake();
    }
}
