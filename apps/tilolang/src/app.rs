//! The tilolang runtime as a framework app: the installed package, shown through the framework's
//! root like every native app.
//!
//! The root brings the content ground, the save state, per-author undo, History, permissions, the
//! action palette, the agent door, search and the unreadable-record state; [`Window`] only renders
//! the package's views onto the native shell.
//!
//! A process runs one package: the one the launcher installed ([`crate::install`]). Its id, title
//! and icon are the app's ([`ResourceApp::named`]). The window's own tests run the built-in
//! packages side by side instead, compiled in the test process.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use tilolang_tilo::host::Host;
use tilolang_tilo::tilolang::PackageMeta;
use ui::framework::{AppName, ResourceApp, ViewCx};
use ui::widget::Element;

use crate::resource::Room;
use crate::window::Window;

/// The app the installed package is.
pub struct TlApp;

/// The package a window made now runs: the installed one; in the window's tests, the one chosen
/// on the thread.
#[must_use]
pub fn hosted() -> Option<&'static PackageMeta> {
    #[cfg(test)]
    if let Some(meta) = builtin::chosen() {
        return Some(meta);
    }
    crate::installed_bundle().map(|bundle| &bundle.meta)
}

/// Load what a window of the hosted package runs into `host`: the installed package, or the
/// latest later build of it a development session handed over ([`crate::updates`]), with the
/// record schemas of the packages it uses; in the window's tests, every built-in package. The
/// later build it loaded, 0 for the installed package.
pub fn load(host: &Host) -> u64 {
    #[cfg(test)]
    if builtin::chosen().is_some() {
        builtin::load(host);
        if let Some(update) = crate::updates::latest()
            && host.load_bundle(&update.bundle).is_ok()
        {
            return update.generation;
        }
        return 0;
    }
    if let Some(update) = crate::updates::latest() {
        match host.load_bundle(&update.bundle) {
            Ok(_) => return update.generation,
            Err(error) => {
                tracing::error!(app = update.bundle.meta.app, %error, "the later build did not load");
            }
        }
    }
    let Some(bundle) = crate::installed_bundle() else {
        tracing::error!("no package is installed in this runtime");
        return 0;
    };
    if let Err(error) = host.load_bundle(bundle) {
        tracing::error!(app = bundle.meta.app, %error, "the installed package did not load");
    }
    0
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
    const ID: &'static str = crate::UNBUNDLED_ID;
    const TITLE: &'static str = "tilolang";
    const ICON: Option<&'static str> = Some("code-editor");
    // The record is the room's: every package keeps its resources in it.
    const ROOM_RECORD: bool = true;

    fn named() -> Option<AppName> {
        let meta = &crate::installed_bundle()?.meta;
        Some(AppName {
            id: &meta.app,
            title: &meta.title,
            icon: meta.icon.as_deref().map(crate::icon_name),
        })
    }

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

/// The built-in packages, compiled in the test process, which the window's tests run side by
/// side.
#[cfg(test)]
pub mod builtin {
    use std::cell::RefCell;
    use std::sync::OnceLock;

    use tilolang_tilo::host::Host;
    use tilolang_tilo::tilolang::{self, Compiled, IdsLock, PackageMeta};

    use crate::packages::EMBEDDED;

    /// One built-in package, compiled.
    pub struct Built {
        /// Its directory under `apps/tl`.
        pub dir: &'static str,
        pub compiled: Compiled,
    }

    /// Every built-in package, compiled, in load order.
    ///
    /// # Panics
    /// A built-in package does not compile.
    pub fn compiled() -> &'static [Built] {
        static COMPILED: OnceLock<Vec<Built>> = OnceLock::new();
        COMPILED.get_or_init(|| {
            let mut out: Vec<Built> = Vec::new();
            for package in EMBEDDED {
                let lock = IdsLock::parse(package.ids).expect("a built-in ids lock parses");
                let imports: Vec<PackageMeta> =
                    out.iter().map(|done| done.compiled.meta.clone()).collect();
                let compiled = tilolang::compile_package(package.source, &lock, &imports)
                    .unwrap_or_else(|diagnostics| {
                        let rendered: Vec<String> = diagnostics
                            .iter()
                            .map(|diagnostic| diagnostic.render("app.tl"))
                            .collect();
                        panic!("{} does not compile:\n{}", package.dir, rendered.join("\n"))
                    });
                out.push(Built {
                    dir: package.dir,
                    compiled,
                });
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
        static CHOSEN: RefCell<Option<&'static PackageMeta>> = const { RefCell::new(None) };
    }

    /// Run `app` (an app id or directory) in the windows made on this thread from now on.
    ///
    /// # Panics
    /// `app` is no built-in package.
    pub fn choose(app: &str) {
        let meta = package_named(app).expect("a built-in package");
        CHOSEN.with(|chosen| *chosen.borrow_mut() = Some(meta));
    }

    pub(super) fn chosen() -> Option<&'static PackageMeta> {
        CHOSEN.with(|chosen| *chosen.borrow())
    }

    pub(super) fn load(host: &Host) {
        for built in compiled() {
            host.load_compiled(&built.compiled)
                .expect("a built-in package loads");
        }
    }
}
