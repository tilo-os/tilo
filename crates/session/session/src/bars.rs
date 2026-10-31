//! The bars the session shows on its displays, as a harness reads them: one
//! per display, the full bar on the display in use and a compact one on each
//! other, and whether each stepped aside for a fullscreen window there.

use tilo_desktop::desktop_host::DesktopChrome;
use tilo_desktop::topbar::Placement;
use ui::vello::kurbo::Rect;

use crate::Session;

/// A bar the session shows, as it stands on its display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarShown {
    /// The display it stands on; `None` for the full bar, which stands on
    /// the display in use (and on a desk of one display, where the desk
    /// needs no placing).
    pub display: Option<u32>,
    /// Where it stands, in the chrome's coordinates.
    pub rect: Rect,
    /// The full bar (navigation, the centre widget and the session's own
    /// controls) rather than a compact one (navigation and the centre).
    pub full: bool,
    /// A fullscreen window owns its display, so it stepped aside.
    pub aside: bool,
}

impl Session {
    /// The bars the chrome shows, as each stands on its display now: the
    /// full one on the display in use, then a compact one on each other
    /// display, in the order the displays are known.
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
                    aside: bar.steps_aside(),
                })
                .collect()
        })
        .unwrap_or_default()
    }

    /// Where the desk says the bars go: one place per display, the display
    /// in use full, and the displays a fullscreen window owns.
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
}
