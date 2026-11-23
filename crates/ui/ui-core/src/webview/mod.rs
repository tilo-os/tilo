//! Painting a web engine's frames: the toolkit's side of a web view. The engine client
//! itself — its processes, page protocol and commands — is the `tilo-webview` crate.
//!
//! A `ui.WebView` node names the page it shows, and the window that draws it holds the engine:
//! one page per window, so the engine belongs to the window and not to the app's code. Before
//! it builds the nodes of a view, a window hands the engine it holds to [`set_window_engine`],
//! and every `ui.WebView` it builds from those nodes shows that page.
pub mod paint;

use std::cell::RefCell;
use std::rc::Rc;

pub use tilo_webview::SharedEngine;
use tilo_page_engine::PageSession;

/// The page a window shows, as the window keeps it: its engine and the one session that reads
/// the engine's events for both the view and the app's page calls.
pub type PageHandle = Rc<RefCell<Option<PageSession>>>;

thread_local! {
    /// The engine the window drawing now hands to the `ui.WebView` nodes it builds.
    static WINDOW_ENGINE: RefCell<Option<SharedEngine>> = const { RefCell::new(None) };
    /// The page the window drawing now hands to the `ui.WebView` node it builds, beside the
    /// engine: the view reads its events from this session, so the engine has one reader.
    static WINDOW_PAGE: RefCell<Option<PageHandle>> = const { RefCell::new(None) };
}

/// The engine the window drawing now hands to its `ui.WebView` nodes.
#[must_use]
pub fn window_engine() -> Option<SharedEngine> {
    WINDOW_ENGINE.with(|engine| engine.borrow().clone())
}

/// Hand `engine` to the `ui.WebView` nodes the window builds until the next call; `None` when
/// the window draws no page.
pub fn set_window_engine(engine: Option<SharedEngine>) {
    WINDOW_ENGINE.with(|held| *held.borrow_mut() = engine);
}

/// The page of the window that is building its views now, so the `ui.WebView` reads its events
/// from the window's one reader.
#[must_use]
pub fn window_page() -> Option<PageHandle> {
    WINDOW_PAGE.with(|page| page.borrow().clone())
}

/// Hand `page` to the `ui.WebView` node the window builds until the next call; `None` when the
/// window draws no page.
pub fn set_window_page(page: Option<PageHandle>) {
    WINDOW_PAGE.with(|held| *held.borrow_mut() = page);
}

/// The engine a window shows a page with when it has none yet: this device's Chromium engine,
/// or one that says why the page cannot be shown.
#[must_use]
pub fn new_page_engine() -> SharedEngine {
    match tilo_webview::chromium::ChromiumEngine::new() {
        Ok(engine) => engine,
        Err(why) => {
            let missing: SharedEngine = tilo_webview::chromium::UnavailableEngine::new(why);
            missing
        }
    }
}
