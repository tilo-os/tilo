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
    // This process runs the package's module itself: it is its app's runtime, so it attaches to
    // none beside it. It joins its workspace over the connection the launcher handed it, as a
    // window a session vouched for does.
    const ATTACHES_RUNTIME: bool = false;

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

    /// A capture stands on its scenario run whole: one that stopped at a step fails the capture
    /// there, in the words of the step that failed and why.
    fn scenario_input(
        _name: Option<&str>,
        _input: &mut ui::headless::Driver,
    ) -> Result<(), String> {
        super::hosted::take_stage_failure().map_or(Ok(()), Err)
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
        // A window whose instance this process runs reaches its origins' services through the
        // connectors the device's daemon runs, over the connection the launch handed this
        // process, as a hosted instance is handed its own. A window the daemon did not start
        // (a program run by hand) has none, and reaches no outside service. History previews a
        // past version of the same window: it starts nothing.
        #[cfg(unix)]
        if cx.mode() != ui::framework::RunMode::Past {
            handed_origins(&mut window);
        }
        window.follow_access(cx.access());
        // A search hit, or an agent's `open`, shows the resource it names.
        if let Some(reveals) = window.reveals() {
            cx.on_open(move |anchor: &str| reveals.borrow_mut().push(anchor.to_owned()));
        }
        cx.surface(window)
    }
}

/// The window of the installed package for `me` in `workspace`, running its instance in this
/// process over the app's document on this device, kept in the app's data area (`files`): a
/// window without a session (`tilolang dev`, a capture). `None` when nothing is installed, or
/// the instance does not start.
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
    // The machine code the device's daemon compiled for this app's modules, read from the store
    // the launcher granted read-only; a module it did not compile runs on the interpreter. No
    // module is compiled in this process.
    if let Some(dir) =
        std::env::var_os(runtime_portal::process_link::COMPILED_ENV).filter(|dir| !dir.is_empty())
    {
        tilolang_tilo::runtime::engine::compiled_from(std::path::PathBuf::from(dir));
    }
    // A capture the device's daemon started for its person joins the workspace over the
    // connections the daemon made for its instance: what it keeps is the workspace's, and what
    // it calls goes through the device's dispatch, as a launched window's calls do.
    #[cfg(all(unix, feature = "app-launch"))]
    if let Some(joined) = runtime_portal::joined_capture::take() {
        let Ok(joined_workspace) = uuid::Uuid::parse_str(&joined.workspace) else {
            tracing::error!(
                workspace = joined.workspace,
                "a joined capture's workspace is no id"
            );
            return None;
        };
        let route = crate::route_of(&bundle.meta, joined_workspace, &joined.person);
        let spec = crate::hosted::Spec::new(
            bundle.meta.app.clone(),
            &joined.workspace,
            joined.instance,
            &joined.person,
        );
        return super::hosted::HostedWindow::in_process_joined(
            spec,
            route,
            bundle,
            (joined.collaboration, joined.identity),
        )
        .inspect_err(
            |error| tracing::error!(%error, "the app's instance did not join its workspace"),
        )
        .ok();
    }
    let route = crate::route_of(&bundle.meta, workspace_id, me);
    let spec = crate::hosted::Spec::new(bundle.meta.app.clone(), workspace, 1, me);
    super::hosted::HostedWindow::in_process(spec, route, bundle, storage)
        .inspect_err(|error| tracing::error!(%error, "the app's instance did not start"))
        .ok()
}

/// Keep the types of the package whose origin is a video or music site in step with it: the
/// client asks, and the origin, in native code with the network, writes what they answer. A
/// window that runs its instance here has no more network than a host has, so it reaches the
/// service over the connection the device's daemon made for it, as a hosted instance is handed
/// its own: the daemon runs the origin's connector, and the launch handed this process the
/// connection that reaches it ([`runtime_portal::origin_connector::take_reader`]). A window the
/// daemon did not start has none.
#[cfg(all(unix, feature = "app-launch"))]
fn handed_origins(window: &mut super::hosted::HostedWindow) {
    let Some(meta) = installed() else {
        return;
    };
    // An instance reaches one connector: a package declares one origin.
    let Some(label) = ["youtube", "music"]
        .into_iter()
        .find(|origin| tilolang_tilo::origin::uses_origin(meta, origin))
    else {
        return;
    };
    let Some(connection) = runtime_portal::origin_connector::take_reader() else {
        return;
    };
    let wake = ui::wake::WakeHandle::new();
    let waker = std::sync::Arc::new(move || wake.wake());
    let attached = match label {
        "youtube" => tilolang_youtube::remote_origin(connection.into(), waker)
            .map(|origin| origin as tilolang_tilo::origin::SharedOrigin),
        "music" => tilolang_music::remote_origin(connection.into(), waker)
            .map(|origin| origin as tilolang_tilo::origin::SharedOrigin),
        other => Err(format!("there is no connector for the origin `{other}`")),
    };
    match attached {
        Ok(origin) => window.add_origin(origin),
        Err(error) => {
            tracing::error!(%error, origin = label, "the app's origin is not kept in step with its service");
        }
    }
}
