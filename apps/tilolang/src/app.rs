//! The tilolang runtime as a framework app: the installed package, shown through the framework's
//! root like every native app.
//!
//! The root brings the content ground and the unreadable-document state; the window
//! ([`crate::hosted::HostedWindow`]) shows the package's instance, which a tilolang host runs: the
//! session's, or one in this process without a session.
//!
//! A process runs one package: the one the launcher installed ([`crate::install`]). Its id, title
//! and icon are the app's ([`ResourceApp::named`]).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use tilolang_tilo::tilolang::PackageMeta;
use ui::framework::{AppName, OpeningCx, OpeningView, ResourceApp, ViewCx};
use ui::widget::Element;

use crate::resource::Workspace;

/// The app the installed package is.
pub struct TlApp;

/// The package the process runs: the installed one.
fn installed() -> Option<&'static PackageMeta> {
    crate::installed_bundle().map(|bundle| &bundle.meta)
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
    const ID: &'static str = crate::UNBUNDLED_ID;
    const TITLE: &'static str = "tilolang";
    const ICON: Option<&'static str> = Some("code-editor");
    // The document is the workspace's: every package keeps its resources in it.
    const WORKSPACE_RESOURCE: bool = true;

    fn named() -> Option<AppName> {
        let meta = &crate::installed_bundle()?.meta;
        Some(AppName {
            id: &meta.app,
            title: &meta.title,
            icon: meta.icon.as_deref().map(crate::icon_name),
            // A package declaring `documents: .own` keeps each member's documents in their own.
            own_resources: meta.own_resources,
        })
    }

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::i18n::CATALOG)
    }

    fn label(_state: &crate::resource::WorkspaceState) -> String {
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
        crate::i18n::ensure();
        #[cfg(target_os = "linux")]
        {
            let window = if crate::hosted::is_hosted() {
                crate::hosted::HostedWindow::new(crate::hosted::window_instance())
            } else {
                in_process(cx.me().id(), cx.workspace(), &cx.files())?
            };
            Some(Box::new(window))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = cx;
            None
        }
    }

    fn view(cx: &mut ViewCx<Workspace>) -> Element {
        crate::i18n::ensure();
        #[cfg(target_os = "linux")]
        {
            // The window shown before the document, taken over with what the person did in it.
            let opened = cx
                .take_lent::<ui::framework::Opened>()
                .and_then(|opened| opened.take::<crate::hosted::HostedWindow>());
            if crate::hosted::is_hosted() {
                return opened.unwrap_or_else(|| {
                    Box::new(crate::hosted::HostedWindow::new(
                        crate::hosted::window_instance(),
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
                return Box::new(ui::widget::EmptyState::new(crate::i18n::t(
                    "tilolang-open-failed",
                )));
            };
            if cx.mode() == ui::framework::RunMode::Live {
                live_origins(&mut window, cx.storage().config_root());
            }
            window.follow_standing(cx.standing());
            // A search hit, or an agent's `open`, shows the resource it names.
            if let Some(reveals) = window.reveals() {
                cx.on_open(move |anchor: &str| reveals.borrow_mut().push(anchor.to_owned()));
            }
            cx.surface(window)
        }
        // A page runs the package's instance itself.
        #[cfg(target_arch = "wasm32")]
        {
            let Some(bundle) = crate::installed_bundle_shared() else {
                return Box::new(ui::widget::EmptyState::new(crate::i18n::t(
                    "tilolang-open-failed",
                )));
            };
            let me = cx
                .me()
                .map(ui::tilo_resource::MemberId::id)
                .unwrap_or_default()
                .to_owned();
            cx.surface(crate::page::PageWindow::new(&me, bundle))
        }
        #[cfg(not(any(target_os = "linux", target_arch = "wasm32")))]
        {
            let _ = cx;
            Box::new(ui::widget::EmptyState::new(crate::i18n::t(
                "tilolang-open-failed",
            )))
        }
    }
}

/// The window of the installed package for `me` in `workspace`, running its instance in this
/// process over the app's document on this device, kept in the app's data area (`files`): a
/// window without a session (`tilolang dev`, a capture). `None` when nothing is installed, or
/// the instance does not start.
#[cfg(target_os = "linux")]
fn in_process(
    me: &str,
    workspace: &str,
    files: &ui::storage::AppFiles,
) -> Option<crate::hosted::HostedWindow> {
    let Some(bundle) = crate::installed_bundle_shared() else {
        tracing::error!("no package is installed in this runtime");
        return None;
    };
    // The document is this device's alone: its route only names where it is kept, one per
    // workspace, whether the profile names its workspace by id or by name (`personal`).
    let workspace_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, workspace.as_bytes());
    let storage = match files.paths().data().native_path() {
        Ok(data) => Some(data.join("tilolang")),
        Err(error) => {
            tracing::warn!(%error, "no data area for the app's document: it is kept in memory");
            None
        }
    };
    let route = tilolang_host::route_of(&bundle.meta, workspace_id, me);
    let spec = tilolang_host::hosted::Spec::new(bundle.meta.app.clone(), workspace, 1, me);
    let kind = tilolang_tilo::meta::MetaExt::resource_type(&bundle.meta);
    let person = me.to_owned();
    crate::hosted::HostedWindow::in_process(
        spec,
        |kept| {
            tilolang_tilo::instance::document::InstanceDocument::open_local(
                route,
                &kept.documents,
                person,
                kind,
            )
        },
        bundle,
        storage,
    )
    .inspect_err(|error| tracing::error!(%error, "the app's instance did not start"))
    .ok()
}

/// Keep the types of the package whose origin is a mail server in step with the servers of the
/// workspace's mail accounts (kept under `config`), and those whose origin is a video site with it:
/// the client asks, and the origin, in native code with the network, writes what they answer.
#[cfg(target_os = "linux")]
fn live_origins(window: &mut crate::hosted::HostedWindow, config: Option<std::path::PathBuf>) {
    let Some(meta) = installed() else {
        return;
    };
    if tilolang_tilo::origin::uses_origin(meta, "imap") {
        let wake = ui::wake::WakeHandle::new();
        window.add_origin(tilolang_mail::live_origin(
            tilolang_mail::settings_accounts(config),
            std::sync::Arc::new(move || wake.wake()),
        ));
    }
    if tilolang_tilo::origin::uses_origin(meta, "youtube") {
        let wake = ui::wake::WakeHandle::new();
        let origin = tilolang_youtube::origin(
            std::sync::Arc::new(tilolang_youtube::service::Live::new()),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        let waker = std::sync::Arc::new(move || wake.wake());
        window.add_origin(tilolang_youtube::waking(origin, waker));
    }
}
