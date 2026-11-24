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

use tilo_page_engine::PageSession;
pub use tilo_webview::SharedEngine;

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

/// The engine of a window whose page is private: a fresh, disposable profile under `root`,
/// which holds nothing the person signed in to and is removed when the window closes.
///
/// # Errors
///
/// Why no private engine started (no Chromium runtime here, or the root is not one this
/// person's Browser allocated).
pub fn new_private_page_engine(root: &std::path::Path) -> Result<SharedEngine, String> {
    tilo_webview::chromium::ChromiumEngine::new_private(root).map(|engine| engine as SharedEngine)
}

/// The engine a window shows a page with when it has none yet: this device's Chromium engine,
/// with the extensions the person turned on, or one that says why the page cannot be shown.
#[must_use]
pub fn new_page_engine() -> SharedEngine {
    let extensions = tilo_webview::extensions::extensions_dir()
        .map(|root| tilo_webview::extensions::approved_paths(&root))
        .unwrap_or_default();
    let started = if extensions.is_empty() {
        tilo_webview::chromium::ChromiumEngine::new()
    } else {
        tilo_webview::chromium::ChromiumEngine::new_with_extensions(&extensions)
    };
    match started {
        Ok(engine) => {
            let engine: SharedEngine = engine;
            announce_extensions(&engine, &extensions);
            engine
        }
        Err(why) => {
            let missing: SharedEngine = tilo_webview::chromium::UnavailableEngine::new(why);
            missing
        }
    }
}

/// Tell the engine what each extension it started with may do, from the manifest it installed:
/// the engine's pages give an extension only what its own file asks for, and nothing an
/// extension this device did not install claims.
fn announce_extensions(engine: &SharedEngine, extensions: &[std::path::PathBuf]) {
    for path in extensions {
        let Some(id) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        // Chromium names an unsigned extension itself, so its own access belongs to an
        // identifier no signed package names: only the signed ones are told of here.
        if !tilo_webview::extensions::crx::is_extension_id(id) {
            continue;
        }
        let Some(access) = tilo_webview::extensions::access_of(path) else {
            tracing::warn!(path = %path.display(), "an extension's manifest could not be read");
            continue;
        };
        engine.send(tilo_webview::WebViewCommand::ExtensionAccess {
            id: id.to_owned(),
            permissions: access.permissions,
            hosts: access.hosts,
            scriptable: access.scriptable,
            framed: access.framed,
            token: None,
            commands: access.commands,
        });
    }
}
