//! A described component's instance under a desktop-sized window, as the component tests read it.

use crate::headless::Driver;
use crate::theme::Theme;
use crate::widget::Widget;

/// The widget at a desktop-sized window, laid out in the light theme.
pub fn harness<W: Widget + 'static>(root: W) -> Driver {
    Driver::for_root(crate::State::new(), root, (900, 600), Theme::light())
}
