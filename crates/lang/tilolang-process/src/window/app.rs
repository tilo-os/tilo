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
        // A window with no session reaches the outside services its instance keeps in step
        // through the connectors this launch starts; a hosted window's host reaches them over
        // the connections its session handed it.
        #[cfg(unix)]
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
    let route = crate::route_of(&bundle.meta, workspace_id, me);
    let spec = crate::hosted::Spec::new(bundle.meta.app.clone(), workspace, 1, me);
    super::hosted::HostedWindow::in_process(spec, route, bundle, storage)
        .inspect_err(|error| tracing::error!(%error, "the app's instance did not start"))
        .ok()
}

/// Keep the types of the package whose origin is a video or music site in step with it: the
/// client asks, and the origin, in native code with the network, writes what they answer. A
/// window that runs its instance here has no more network than a host has, so it reaches the
/// service through the connector the system runs for the origin, as a session's host reaches the
/// connector its daemon starts: the connector is this binary's own connector role, started once
/// for the window. A mail server's types are kept only by the connector the session starts,
/// which the session tells the workspace's accounts and their sign-ins: a window outside a
/// tilolang host reaches none.
#[cfg(all(unix, feature = "app-launch"))]
fn live_origins(window: &mut super::hosted::HostedWindow) {
    let Some(meta) = installed() else {
        return;
    };
    for origin in ["youtube", "music"] {
        if !tilolang_tilo::origin::uses_origin(meta, origin) {
            continue;
        }
        let wake = ui::wake::WakeHandle::new();
        let waker = std::sync::Arc::new(move || wake.wake());
        match connector_origin(origin, waker) {
            Ok(origin) => window.add_origin(origin),
            Err(error) => {
                tracing::error!(%error, origin, "the app's origin is not kept in step with its service");
            }
        }
    }
}

/// The origin `origin` kept in step through the connector process this launch starts for it over
/// a socket pair, waking the instance with `wake` when an answer is ready. The connector runs as
/// this binary's connector role (`TILO_TILOLANG_CONNECTOR`), which opens the origin's own library
/// from the runtime bundle: the process every app runs links none of the service's client.
///
/// # Errors
///
/// No socket pair, no connector process, the connector not taking the connection, or the origin
/// not attaching to it.
#[cfg(all(unix, feature = "app-launch"))]
fn connector_origin(
    origin: &'static str,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
) -> Result<tilolang_tilo::origin::SharedOrigin, String> {
    // Only reached where a connector process can be told its socket: the launcher's service
    // descriptor on Linux, the process link on macOS. Elsewhere the window has no connector.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        /// What became of the connector started for an origin, by origin: one is started per
        /// window, and a view of the window shown again is handed the same one. An origin belongs
        /// to the thread that draws the window, so this is the window's own.
        type Started = std::collections::BTreeMap<
            &'static str,
            Result<tilolang_tilo::origin::SharedOrigin, String>,
        >;
        thread_local! {
            static STARTED: std::cell::RefCell<Started> =
                const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
        }
        STARTED.with(|started| {
            let mut started = started.borrow_mut();
            if let Some(earlier) = started.get(origin) {
                return earlier.clone();
            }
            let attached: Result<tilolang_tilo::origin::SharedOrigin, String> =
                spawn_connector(origin).and_then(|stream| match origin {
                    "youtube" => tilolang_youtube::remote_origin(stream, wake)
                        .map(|origin| origin as tilolang_tilo::origin::SharedOrigin),
                    "music" => tilolang_music::remote_origin(stream, wake)
                        .map(|origin| origin as tilolang_tilo::origin::SharedOrigin),
                    other => Err(format!("there is no connector for the origin `{other}`")),
                });
            started.insert(origin, attached.clone());
            attached
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (origin, wake);
        Err("this host starts no connector for a window without a session".to_owned())
    }
}

/// The window's end of a connection to the connector of `origin`, which this starts as this
/// binary's connector role over a socket pair the connector takes its service socket from.
///
/// # Errors
///
/// A socket the connection cannot be made over, the connector not starting, or the connector not
/// taking the connection.
#[cfg(all(
    unix,
    feature = "app-launch",
    any(target_os = "linux", target_os = "macos")
))]
fn spawn_connector(origin: &'static str) -> Result<tilo_socket::Stream, String> {
    use runtime_portal::origin_connector;
    use std::os::fd::AsRawFd as _;
    use std::os::unix::process::CommandExt as _;

    let (ours, theirs) = origin_connector::pair()
        .map_err(|error| format!("no socket for the {origin} connector: {error}"))?;
    let mut command = connector_command()?;
    command.env(origin_connector::ORIGIN_ENV, origin).env(
        connector_socket_env(),
        origin_connector::CHILD_FD.to_string(),
    );
    // The launcher places a connector's socket at `CHILD_FD`; so does this, in the child alone.
    let socket = theirs.as_raw_fd();
    // SAFETY: `pre_exec` runs between fork and exec, where only async-signal-safe calls belong:
    // `dup2` is one, and it writes the descriptor the child reads its socket from.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(socket, origin_connector::CHILD_FD) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("the {origin} connector did not start: {error}"))?;
    // The child holds the socket now; the window's copy closes so the connector ends with it.
    drop(theirs);
    // The connector runs until this window lets its socket go, which it does as the window's
    // process ends: reap it then, off the frame.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    let (instance, connector_end) = origin_connector::connection()
        .map_err(|error| format!("no connection to the {origin} connector: {error}"))?;
    origin_connector::send_attachment(&ours, &connector_end)
        .map_err(|error| format!("the {origin} connector took no connection: {error}"))?;
    // The connector keeps answering on its socket until the window's end closes: that end stays
    // open for as long as this process, since the connector lives as long as the window does.
    kept_open()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(ours);
    Ok(instance.into())
}

/// The window's ends of the connectors' sockets, kept open for the process's life.
#[cfg(all(
    unix,
    feature = "app-launch",
    any(target_os = "linux", target_os = "macos")
))]
fn kept_open() -> &'static std::sync::Mutex<Vec<runtime_portal::origin_connector::Descriptor>> {
    static KEPT: std::sync::OnceLock<
        std::sync::Mutex<Vec<runtime_portal::origin_connector::Descriptor>>,
    > = std::sync::OnceLock::new();
    KEPT.get_or_init(Default::default)
}

/// The program the connector role runs: this one, the runtime's app process, which runs the
/// origin's connector when started as it (`TILO_TILOLANG_CONNECTOR`) instead of a window.
#[cfg(all(
    unix,
    feature = "app-launch",
    not(feature = "test-support"),
    any(target_os = "linux", target_os = "macos")
))]
fn connector_command() -> Result<std::process::Command, String> {
    let program =
        std::env::current_exe().map_err(|error| format!("this process's own program: {error}"))?;
    Ok(std::process::Command::new(program))
}

/// The program the connector role runs in a test: this test binary, running one child test as the
/// connector over the socket it is handed, as the runtime's own process runs the real connector.
#[cfg(all(unix, feature = "app-launch", feature = "test-support"))]
fn connector_command() -> Result<std::process::Command, String> {
    let program =
        std::env::current_exe().map_err(|error| format!("this test's own program: {error}"))?;
    let mut command = std::process::Command::new(program);
    command.args([
        "--exact",
        "hosted::origins::the_connector_process",
        "--nocapture",
    ]);
    Ok(command)
}

/// The variable the connector finds its socket in: the launcher's service descriptor for the
/// origin connector on Linux, and the process link on macOS.
#[cfg(all(unix, feature = "app-launch", target_os = "linux"))]
fn connector_socket_env() -> String {
    runtime_portal::session_services::fd_env(runtime_portal::origin_connector::SERVICE)
}

/// The variable the connector finds its socket in on macOS, where the connector's socket is its
/// process link.
#[cfg(all(unix, feature = "app-launch", target_os = "macos"))]
fn connector_socket_env() -> String {
    runtime_portal::process_link::FD_ENV.to_owned()
}
