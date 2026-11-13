//! The bars the session shows on its displays, as a harness reads them: one per display, the full
//! bar on the display in use and a compact one on each other, and whether each stands in the desk's
//! column, is hidden, or is shown over a window covering its display.

use tilo_desktop::desktop_host::DesktopChrome;
use tilo_desktop::topbar::Placement;
use ui::vello::kurbo::Rect;

use crate::Session;

/// A bar the session shows, as it stands on its display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarShown {
    /// The display it stands on; `None` for the full bar, which stands on the display in use (and
    /// on a desk of one display, where the desk needs no placing).
    pub display: Option<u32>,
    /// Where it stands, in the chrome's coordinates.
    pub rect: Rect,
    /// The full bar (navigation, the centre widget and the session's own controls) rather than a
    /// compact one (navigation and the centre).
    pub full: bool,
    /// Docked in the column, hidden, or shown over the window covering the display.
    pub presence: tilo_desktop::chrome::TopBarPresence,
}

impl Session {
    /// The bars the chrome shows, as each stands on its display now: the full one on the display in
    /// use, then a compact one on each other display, in the order the displays are known.
    #[must_use]
    pub fn bars(&self) -> Vec<BarShown> {
        self.with_chrome(|chrome| {
            chrome
                .bars()
                .into_iter()
                .map(|(display, bar)| BarShown {
                    display,
                    rect: bar.bar_rect(),
                    full: !bar.is_compact(),
                    presence: bar.presence(),
                })
                .collect()
        })
        .unwrap_or_default()
    }

    /// Where the desk says the bars go: one place per display, the display in use full, and the
    /// displays a fullscreen window owns.
    #[must_use]
    pub fn bar_placement(&self) -> Placement {
        self.with_chrome(|chrome| chrome.placement().clone())
            .unwrap_or_default()
    }

    /// The chrome as the session hosts it.
    pub(crate) fn with_chrome<T>(&self, read: impl FnOnce(&DesktopChrome) -> T) -> Option<T> {
        let mut host = self.shared.chrome.borrow_mut();
        let root = host.host_mut().root_widget_mut();
        Some(read(root.as_any_mut()?.downcast_ref::<DesktopChrome>()?))
    }

    /// The chrome as the session hosts it, to change.
    pub(crate) fn with_chrome_mut<T>(
        &self,
        change: impl FnOnce(&mut DesktopChrome) -> T,
    ) -> Option<T> {
        let mut host = self.shared.chrome.borrow_mut();
        let root = host.host_mut().root_widget_mut();
        Some(change(root.as_any_mut()?.downcast_mut::<DesktopChrome>()?))
    }

    /// Bring the bars' reveal up to date for the pass: the person's own "hide the top bar", the
    /// pointer, a key or a finger reaching for the bar, and what the chrome holds open over a
    /// covering window.
    pub(crate) fn follow_bars(&self) {
        let hide = self
            .shared
            .hide_top_bar
            .load(std::sync::atomic::Ordering::Relaxed);
        let held = self.quick_settings_open();
        let now = ui::time::now();
        let changed = self
            .with_chrome_mut(|chrome| chrome.follow_reveals(hide, held, now))
            .unwrap_or(false);
        if changed {
            // A bar docked in the desk's column takes its place there: docking it, hiding it or
            // showing it over a window lays the windows out again for what is left.
            self.shared
                .desktop
                .with_widgets(crate::platform::surfaces::WidgetHost::relayout);
        }
    }
}
