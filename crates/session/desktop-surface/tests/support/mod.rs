//! The chrome a session hosts over the desk, driven by tests: search's card is the chrome's, drawn
//! above the windows, so a test that searches drives this beside the desk.

#![allow(
    dead_code,
    reason = "each test binary takes what it needs of these fixtures"
)]

use std::rc::Rc;
use std::time::Duration;

use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::desktop_host::{DesktopChrome, DesktopRules};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::DesktopTree;
use tilo_desktop_model::commands::DesktopCommand;
use tilo_desktop_surface::DesktopHandle;

/// How long a step of a search may take before the test says which one never finished. A search
/// answers on workers, and its rows are drawn once every source is in: seconds is a search that
/// never answers, not a slow one.
pub const SEARCH_STEP: Duration = Duration::from_secs(5);

/// What the chrome over the desk asks the person they may change.
struct ChromeRules {
    handle: DesktopHandle,
}

impl DesktopRules for ChromeRules {
    fn tree(&self) -> DesktopTree {
        self.handle.read(Clone::clone)
    }

    fn author(&self) -> String {
        "Ada".into()
    }

    fn may(&self, _: &DesktopCommand) -> Result<(), String> {
        Ok(())
    }

    fn may_create_workspace(&self) -> Result<(), String> {
        Ok(())
    }
}

/// The chrome the session hosts over the windows, over the desk `handle` holds: search's palette is
/// drawn and answered there, so a test that searches drives this beside the desk.
pub fn chrome_at(handle: &DesktopHandle, size: (u32, u32)) -> ui::headless::FrameHarness {
    let chrome = DesktopChrome::new(
        handle.clone(),
        "g".into(),
        Rc::new(ChromeRules {
            handle: handle.clone(),
        }),
        TopBarChrome::from_tree(&handle.read(Clone::clone)),
        TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    );
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        chrome,
        size,
        ui::Theme::dark(),
    );
    frame.set_caller_palette_enabled(false);
    frame.relayout();
    frame
}

/// What the person types into the open palette's field: search's card is the chrome's, over the
/// windows, so the keys go there, and the desk takes up what the card asked before the card draws
/// it again. The search's sources answer on workers, so what they answered is in before the test
/// reads a row.
pub fn type_search(
    desk: &mut ui::headless::FrameHarness,
    chrome: &mut ui::headless::FrameHarness,
    handle: &DesktopHandle,
    text: &str,
) {
    chrome.type_text(text);
    chrome.relayout();
    desk.relayout();
    chrome.relayout();
    settle_search(desk, chrome, handle);
}

/// A key pressed in the open palette's field, taken up as [`type_search`] is.
pub fn key_search(
    desk: &mut ui::headless::FrameHarness,
    chrome: &mut ui::headless::FrameHarness,
    handle: &DesktopHandle,
    key: &str,
) {
    settle_search(desk, chrome, handle);
    chrome.key(key);
    chrome.relayout();
    desk.relayout();
    chrome.relayout();
    settle_search(desk, chrome, handle);
}

/// Draw the desk's and the card's frames until every source of the open search has answered, saying
/// so when one never does.
pub fn settle_search(
    desk: &mut ui::headless::FrameHarness,
    chrome: &mut ui::headless::FrameHarness,
    handle: &DesktopHandle,
) {
    assert!(
        search_settled(desk, chrome, handle),
        "the search never finished"
    );
}

/// Draw the desk's and the card's frames until every source of the open search has answered. False
/// once [`SEARCH_STEP`] passed with a source still out.
///
/// A source answers on a worker, and its answer wakes the desk: a test pumps what the wake asks for
/// and draws the frame it brings, as a host does. A source still out does not make the search "still
/// searching" once the rows that are ready are shown ([`DesktopHandle::search_searching`]), so this
/// waits on the sources themselves ([`DesktopHandle::search_expecting`]): a row a later answer
/// brings is drawn before the test reads what the card holds.
pub fn search_settled(
    desk: &mut ui::headless::FrameHarness,
    chrome: &mut ui::headless::FrameHarness,
    handle: &DesktopHandle,
) -> bool {
    let background = ui::background::Background::current();
    let now = ui::time::now();
    let deadline = now.checked_add(SEARCH_STEP).unwrap_or(now);
    loop {
        desk.relayout();
        chrome.relayout();
        if !handle.search_expecting() {
            return true;
        }
        if ui::time::now() >= deadline {
            return false;
        }
        let now = ui::time::now();
        background.park(now.checked_add(Duration::from_millis(2)).unwrap_or(now));
    }
}
