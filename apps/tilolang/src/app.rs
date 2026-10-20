//! The tilolang app as a framework app: one package of the room's tilolang record, shown through
//! the framework's root like every native app.
//!
//! The root brings the content ground, the save state, per-author undo, History, permissions, the
//! action palette, the agent door, search and the unreadable-record state; [`Window`] only renders
//! the package's views onto the native shell.
//!
//! Which package a window runs is named by the launcher ([`PACKAGE_ID_ENV`]). The built-in
//! packages are compiled once per process ([`compiled`]); every one of them is loaded into the
//! runtime, since a view may show a resource of another package (a board's notes).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use tilolang_tilo::tilolang::{self, Compiled, IdsLock, PackageMeta};
use ui::framework::{ResourceApp, ViewCx};
use ui::widget::Element;

use crate::packages::EMBEDDED;
use crate::resource::Room;
use crate::window::Window;

/// The variable the launcher names the package a window runs with: its app id
/// (`so.tilo.notes`) or its directory under `apps/tl` (`notes`).
pub const PACKAGE_ID_ENV: &str = "TILO_PACKAGE_ID";

/// The app every built-in package is hosted in.
pub struct TlApp;

/// One built-in package, compiled.
pub struct Built {
    /// Its directory under `apps/tl`.
    pub dir: &'static str,
    pub compiled: Compiled,
}

/// Every built-in package, compiled, in load order. A package that does not compile is left out
/// and logged: the others still run.
pub fn compiled() -> &'static [Built] {
    static COMPILED: OnceLock<Vec<Built>> = OnceLock::new();
    COMPILED.get_or_init(|| {
        let mut out: Vec<Built> = Vec::new();
        for package in EMBEDDED {
            let lock = match IdsLock::parse(package.ids) {
                Ok(lock) => lock,
                Err(error) => {
                    tracing::error!(package = package.dir, %error, "a built-in ids lock is broken");
                    continue;
                }
            };
            let imports: Vec<PackageMeta> =
                out.iter().map(|done| done.compiled.meta.clone()).collect();
            match tilolang::compile_package(package.source, &lock, &imports) {
                Ok(compiled) => out.push(Built {
                    dir: package.dir,
                    compiled,
                }),
                Err(diagnostics) => {
                    let rendered: Vec<String> = diagnostics
                        .iter()
                        .map(|diagnostic| diagnostic.render("app.tl"))
                        .collect();
                    tracing::error!(
                        package = package.dir,
                        diagnostics = rendered.join("\n"),
                        "a built-in package did not compile"
                    );
                }
            }
        }
        out
    })
}

/// The built-in package `name` names: by its app id or its directory.
#[must_use]
pub fn package_named(name: &str) -> Option<&'static PackageMeta> {
    compiled()
        .iter()
        .find(|built| built.compiled.meta.app == name || built.dir == name)
        .map(|built| &built.compiled.meta)
}

thread_local! {
    /// The package windows made on this thread run, when it was chosen in process (a test, the
    /// development window) rather than by the launcher.
    static CHOSEN: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Run `app` (an app id or directory) in the windows made on this thread from now on.
pub fn choose_package(app: &str) {
    CHOSEN.with(|chosen| *chosen.borrow_mut() = Some(app.to_owned()));
}

/// The package a window made now runs: the one chosen on this thread, else the one the launcher
/// names, else the first built-in package with a window.
#[must_use]
pub fn hosted() -> Option<&'static PackageMeta> {
    let named = CHOSEN
        .with(|chosen| chosen.borrow().clone())
        .or_else(|| std::env::var(PACKAGE_ID_ENV).ok());
    if let Some(name) = named {
        return package_named(&name);
    }
    compiled()
        .iter()
        .map(|built| &built.compiled.meta)
        .find(|meta| {
            meta.roots
                .first()
                .is_some_and(|root| meta.default_view(root).is_some())
        })
}

/// The scenario names of `meta`, kept for the process (the framework lists them statically).
fn scenario_names(meta: &PackageMeta) -> &'static [&'static str] {
    static NAMES: OnceLock<Mutex<HashMap<String, &'static [&'static str]>>> = OnceLock::new();
    let mut names = NAMES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    names.entry(meta.app.clone()).or_insert_with(|| {
        let leaked: Vec<&'static str> = meta
            .scenarios
            .iter()
            .map(|scenario| &*Box::leak(scenario.name.clone().into_boxed_str()))
            .collect();
        Box::leak(leaked.into_boxed_slice())
    })
}

impl ResourceApp for TlApp {
    type Resource = Room;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = "Tilo Lang";
    const ICON: Option<&'static str> = Some("code-editor");
    // The record is the room's: every package keeps its resources in it.
    const ROOM_RECORD: bool = true;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::i18n::CATALOG)
    }

    fn label(_state: &crate::resource::RoomState) -> String {
        hosted().map_or_else(|| Self::TITLE.to_owned(), |meta| meta.title.clone())
    }

    fn scenarios() -> &'static [&'static str] {
        hosted().map_or(&[], scenario_names)
    }

    fn view(cx: &mut ViewCx<Room>) -> Element {
        crate::i18n::ensure();
        let Some(meta) = hosted() else {
            return Box::new(ui::widget::EmptyState::new(crate::i18n::t(
                "tilolang-open-failed",
            )));
        };
        let window = Window::new(
            meta.app.clone(),
            cx.me().unwrap_or_default().to_owned(),
            cx.mode(),
        );
        // A search hit, or an agent's `open`, shows the resource it names.
        let opens = window.open_requests();
        cx.on_open(move |anchor: &str| {
            opens.borrow_mut().push(anchor.to_owned());
        });
        cx.surface(window)
    }
}
