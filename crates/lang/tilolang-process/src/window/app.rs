//! The tilolang runtime as a framework app: the installed package, shown through the framework's
//! root like every native app.
//!
//! The root brings the content ground and the unreadable-document state; the window
//! ([`super::hosted::HostedWindow`]) shows the package's instance, which a tilolang host runs: the
//! session's, or one in this process without a session.
//!
//! A process runs one package: the one the launcher installed ([`super::install`]). Its id, title
//! and icon are the app's ([`ResourceApp::named`]).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use tilolang_tilo::tilolang::PackageMeta;
use ui::framework::{AppName, OpeningCx, OpeningView, ResourceApp, ViewCx};
use ui::widget::Element;

use super::resource::Workspace;

/// The app the installed package is.
pub struct TlApp;

/// The package the process runs: the installed one.
fn installed() -> Option<&'static PackageMeta> {
    super::installed_bundle().map(|bundle| &bundle.meta)
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
    type Resource = Workspace;
    const ID: &'static str = super::UNBUNDLED_ID;
    const TITLE: &'static str = "tilolang";
    const ICON: Option<&'static str> = Some("code-editor");
    // The document is the workspace's: every package keeps its resources in it.
    const WORKSPACE_RESOURCE: bool = true;

    fn named() -> Option<AppName> {
        let meta = &super::installed_bundle()?.meta;
        Some(AppName {
            id: &meta.app,
            title: &meta.title,
            icon: meta.icon.as_deref().map(super::icon_name),
        })
    }

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&super::i18n::CATALOG)
    }

    fn label(_state: &super::resource::WorkspaceState) -> String {
        installed().map_or_else(|| Self::TITLE.to_owned(), |meta| meta.title.clone())
    }

    fn scenarios() -> &'static [&'static str] {
        installed().map_or(&[], scenario_names)
    }

    // The first frame shows what the workspace's indexes hold; the document is read off the frame.
    const OPENS_BEFORE_RESOURCE: bool = true;

    /// The window, before the document: in a session, the one whose instance its tilolang host
    /// runs; without one, the window running its instance in this process.
    fn opening(cx: &OpeningCx<'_>) -> Option<Box<dyn OpeningView>> {
        super::i18n::ensure();
        let window = if super::hosted::is_hosted() {
            super::hosted::HostedWindow::new(super::hosted::window_instance())
        } else {
            in_process(cx.me().id(), cx.workspace(), &cx.files())?
        };
        Some(Box::new(window))
    }

    fn view(cx: &mut ViewCx<Workspace>) -> Element {
        super::i18n::ensure();
        // The window shown before the document, taken over with what the person did in it.
        let opened = cx
            .take_lent::<ui::framework::Opened>()
            .and_then(|opened| opened.take::<super::hosted::HostedWindow>());
        if super::hosted::is_hosted() {
            return opened.unwrap_or_else(|| {
                Box::new(super::hosted::HostedWindow::new(
                    super::hosted::window_instance(),
                ))
            });
        }
        let window = match opened {
            Some(window) => Some(*window),
            None => cx.workspace().and_then(|workspace| {
                in_process(
                    cx.me()
                        .map(ui::tilo_resource::MemberId::id)
                        .unwrap_or_default(),
                    &workspace,
                    &cx.storage().files(),
                )
            }),
        };
        let Some(mut window) = window else {
            return Box::new(ui::widget::EmptyState::new(super::i18n::t(
                "tilolang-open-failed",
            )));
        };
        if cx.mode() == ui::framework::RunMode::Live {
            live_origins(&mut window);
        }
        window.follow_access(cx.access());
        // A search hit, or an agent's `open`, shows the resource it names.
        if let Some(reveals) = window.reveals() {
            cx.on_open(move |anchor: &str| reveals.borrow_mut().push(anchor.to_owned()));
        }
        cx.surface(window)
    }
}

/// A window on Windows shows only an instance a host runs, which a session starts: one started
/// without a session has nothing to show.
#[cfg(windows)]
fn in_process(
    _me: &str,
    _workspace: &str,
    _files: &ui::storage::AppFiles,
) -> Option<super::hosted::HostedWindow> {
    tracing::error!("a tilolang window on Windows shows an instance its session's host runs");
    None
}

/// The window of the installed package for `me` in `workspace`, running its instance in this
/// process over the app's document on this device, kept in the app's data area (`files`): a
/// window without a session (`tilolang dev`, a capture). `None` when nothing is installed, or
/// the instance does not start.
#[cfg(unix)]
fn in_process(
    me: &str,
    workspace: &str,
    files: &ui::storage::AppFiles,
) -> Option<super::hosted::HostedWindow> {
    let Some(bundle) = super::installed_bundle_shared() else {
        tracing::error!("no package is installed in this runtime");
        return None;
    };
    // The app's resources are this device's alone, under the workspace's own id: a profile names
    // its workspace by id, as a session does, so the app's access to the workspace covers its
    // route; a profile naming it by a plain name (`personal`) gets one derived, so the same
    // window kept twice is one workspace's.
    let workspace_id = tilo_failure::ignored!(
        "a workspace named by a plain name rather than its id gets one derived from that name",
        uuid::Uuid::parse_str(workspace).ok()
    )
    .unwrap_or_else(|| uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, workspace.as_bytes()));
    let storage = match files.paths().data().native_path() {
        Ok(data) => Some(data.join("tilolang")),
        Err(error) => {
            tracing::warn!(%error, "no data area for the app's document: it is kept in memory");
            None
        }
    };
    // Its own compiled modules kept beside its document, as the app process a session starts for
    // an app's instances keeps its own: a later start of this app adopts the compiled module in
    // its first frame instead of compiling it there again.
    if let Some(storage) = &storage
        && let Err(error) =
            tilolang_tilo::runtime::engine::keep_compiled_in(storage.join(".compiled"))
    {
        tracing::warn!(%error, "compiled tilolang modules are not kept");
    }
    let route = crate::route_of(&bundle.meta, workspace_id, me);
    let spec = crate::hosted::Spec::new(bundle.meta.app.clone(), workspace, 1, me);
    super::hosted::HostedWindow::in_process(spec, route, bundle, storage)
        .inspect_err(|error| tracing::error!(%error, "the app's instance did not start"))
        .ok()
}

/// Keep the types of the package whose origin is a video or music site in step with it: the
/// client asks, and the origin, in native code with the network, writes what they answer. A mail
/// server's types are kept only by the connector the session starts, which the session tells the
/// workspace's accounts and their sign-ins: a window outside a tilolang host reaches none.
fn live_origins(window: &mut super::hosted::HostedWindow) {
    let Some(meta) = installed() else {
        return;
    };
    if tilolang_tilo::origin::uses_origin(meta, "youtube") {
        let wake = ui::wake::WakeHandle::new();
        let origin = tilolang_youtube::origin(
            std::sync::Arc::new(tilolang_youtube::service::Live::new()),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        let waker = std::sync::Arc::new(move || wake.wake());
        window.add_origin(tilolang_youtube::waking(origin, waker));
    }
    if tilolang_tilo::origin::uses_origin(meta, "music") {
        let wake = ui::wake::WakeHandle::new();
        let origin = tilolang_music::origin(
            std::sync::Arc::new(tilolang_music::service::Live::new()),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        let waker = std::sync::Arc::new(move || wake.wake());
        window.add_origin(tilolang_music::waking(origin, waker));
    }
}
