//! Launching through `launcher-macos`: App Sandbox for a trusted signed app,
//! a deny-default Seatbelt profile for everything else.
//!
//! The launcher's contract is that its caller has verified the payload
//! against the package's signed root first ([`super::verified::verify_installed`]);
//! an unverifiable payload is refused before it reaches the launcher. A
//! package whose entrypoint is a runtime's binary (a tilolang app on the
//! tilolang runtime) runs that binary from the installed runtime package,
//! verified the same way, over its own payload (`TILO_APP_MODULE`), as the
//! Linux launcher runs it. The
//! app inherits its service connections already handshaken with its
//! identity, so a sandbox that denies it every socket path still
//! collaborates.
//!
//! A tilolang app's window runs no instance of its own: the instance runs in the app's tilolang
//! process ([`crate::device::processes`]), started as on Linux, where agents and other devices
//! call its actions. The window takes its patch connection on its process link and holds none of
//! the app's data (no collaboration connection), exactly as a Linux session's window.

use super::verified::{installed_runtime, verify_installed, verify_package};
use super::{Apps, Launch};
use crate::daemon::Sockets;
use crate::host::{ServiceConnections, ServiceSockets};
use app_catalogue::apps::InstalledApp;
use std::ffi::OsString;
use std::os::fd::AsRawFd as _;

pub(super) fn launch(
    apps: &Apps,
    app: &InstalledApp,
    workspace: &str,
    sockets: &Sockets,
    shows: super::Shows,
    open: Option<&str>,
    runs: &super::Runs,
) -> Result<Launch, String> {
    let catalog = apps
        .catalog
        .as_ref()
        .ok_or("refusing to launch: no app store is available")?;
    verify_installed(catalog, app)?;
    let manifest = launcher::manifest::read_manifest(&app.manifest_path)
        .map_err(|error| format!("refusing to launch {}: {error}", app.app_id))?;
    let executable = manifest
        .entrypoint
        .gui_bin()
        .filter(|bin| !bin.is_empty())
        .ok_or_else(|| format!("{} has no window to open", app.app_id))?;
    let runtime = manifest
        .entrypoint
        .runtime
        .as_deref()
        .map(|runtime| runtime_payload(catalog, app, runtime))
        .transpose()?;
    if apps.data_home.as_os_str().is_empty() {
        return Err("refusing to launch: HOME is not set, so the app has nowhere to write".into());
    }
    // A tilolang app's instance runs in its app's process; the window shows it.
    let hosted = super::hosts_window(&manifest, shows)
        .then(|| apps.hosted_window_link(&app.app_id, workspace))
        .transpose()?;
    let container =
        launcher_macos::policy::prepare_container(&apps.data_home, Some(workspace), &app.app_id)
            .map_err(|error| {
                format!("refusing to launch {}: its container: {error}", app.app_id)
            })?;
    let service_sockets = ServiceSockets {
        collab: &sockets.collab,
        identity: &sockets.identity,
        blobs: &sockets.blobs,
    };
    let services = if hosted.is_some() {
        ServiceConnections::connect_without_collab(&app.app_id, workspace, &service_sockets)
    } else {
        ServiceConnections::connect(&app.app_id, workspace, &service_sockets)
    }
    .map_err(|error| format!("could not connect {} to its services: {error}", app.app_id))?;
    let chooser = Chooser::start(&app.app_id)
        .map_err(|error| format!("could not give {} a file chooser: {error}", app.app_id))?;
    let capture = Capture::start(apps, app, &manifest)
        .map_err(|error| format!("could not give {} its capture broker: {error}", app.app_id))?;
    let mut inherit_fds = services.fds();
    inherit_fds.push(chooser.app_end.as_raw_fd());
    inherit_fds.push(capture.app_end.as_raw_fd());
    let mut env: Vec<(OsString, OsString)> = services
        .env(&app.app_id, workspace)
        .into_iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .collect();
    env.push((
        OsString::from(tilo_dialogs::confined::PICKER_FD_ENV),
        OsString::from(chooser.app_end.as_raw_fd().to_string()),
    ));
    env.push((
        OsString::from(tilo_capture::broker::CAPTURE_FD_ENV),
        OsString::from(capture.app_end.as_raw_fd().to_string()),
    ));
    if runtime.is_some() {
        env.push((
            OsString::from(launcher::manifest::APP_MODULE_ENV),
            app.payload_root.clone().into_os_string(),
        ));
    }
    if let Some(hosted) = &hosted {
        inherit_fds.push(hosted.window_end.as_raw_fd());
        env.extend(
            [
                (
                    runtime_portal::process_link::FD_ENV,
                    hosted.window_end.as_raw_fd().to_string(),
                ),
                (runtime_portal::process_link::HOSTED_ENV, "1".to_owned()),
                ("TILO_CONTINUITY_INSTANCE", hosted.instance.to_string()),
            ]
            .map(|(key, value)| (OsString::from(key), OsString::from(value))),
        );
    } else if let Some(window) = runs.window {
        // The tab the shell holds for this launch: the app's window takes it.
        env.push((
            OsString::from("TILO_CONTINUITY_INSTANCE"),
            OsString::from(window.to_string()),
        ));
    }
    if let Some(runtime) = runtime.as_deref() {
        env.extend(gstreamer_env(runtime));
    }
    if shows == super::Shows::Nothing {
        // The process draws nothing, takes the app's actions and publishes what it offers, and
        // ends after the idle time a tilolang process does; the dispatch starts it again.
        env.push((
            OsString::from(ui::process::NO_WINDOW_ENV),
            OsString::from("1"),
        ));
        env.push((
            OsString::from(ui::process::NO_WINDOW_IDLE_ENV),
            OsString::from(crate::IDLE.as_secs().to_string()),
        ));
    } else if shows == super::Shows::Capture {
        // A capture draws into its own pictures: it is handed no display and opens no window.
    } else if shows == super::Shows::Connection {
        // It keeps the app's connection, with no idle end, until the device stops it.
        env.push((
            OsString::from(ui::process::NO_WINDOW_ENV),
            OsString::from("1"),
        ));
    }
    let (capabilities, egress) = apps.launch_capabilities(app, workspace, &manifest);
    let request = launcher_macos::LaunchRequest {
        package_id: app.app_id.to_string(),
        executable: runtime
            .as_deref()
            .unwrap_or(&app.payload_root)
            .join(executable),
        // The script the runtime's binary runs comes first, from the payload, then what to open.
        args: manifest
            .entrypoint
            .script
            .iter()
            .map(|script| app.payload_root.join(script).into_os_string())
            .chain(open.map(OsString::from))
            // A capture names what it captures, as a person's own run of the app does.
            .chain(runs.args.iter().map(OsString::from))
            .collect(),
        payload_dir: app.payload_root.clone(),
        runtime_dir: runtime,
        container_dir: container,
        capabilities,
        egress,
        inherit_fds,
        stderr: None,
        env,
        trust: launcher_macos::TrustRequirement::DeveloperId,
    };
    let launched = launcher_macos::launch(&request).map_err(|error| error.to_string())?;
    // The app holds its own descriptors now; this process's copies go.
    drop(services);
    drop(chooser.app_end);
    drop(capture.app_end);
    let link = hosted.map(|hosted| hosted.ours);
    let pid = u32::try_from(launched.pid())
        .map_err(|_| format!("{} started with an invalid pid", app.app_id))?;
    Ok(Launch {
        stderr: std::sync::Arc::default(),
        pid,
        app: launched,
        _link: link,
    })
}

/// Start `app`'s connector for `origin` in `workspace`, handed `socket` on its process link: the
/// tilolang runtime's binary with no window and no module, in the sandbox the app's manifest
/// declares, so with its origin's network, as the Linux launcher starts it.
pub(super) fn connector(
    apps: &Apps,
    app: &InstalledApp,
    workspace: &str,
    (origin, socket): (&str, std::os::fd::BorrowedFd<'_>),
) -> Result<launcher_macos::LaunchedApp, String> {
    let catalog = apps
        .catalog
        .as_ref()
        .ok_or("refusing to start a connector: no app store is available")?;
    verify_installed(catalog, app)?;
    let manifest = launcher::manifest::read_manifest(&app.manifest_path)
        .map_err(|error| format!("refusing to start {}'s connector: {error}", app.app_id))?;
    if !manifest
        .entrypoint
        .origins
        .iter()
        .any(|declared| declared == origin)
    {
        return Err(format!(
            "{} declares no `{origin}` origin to connect",
            app.app_id
        ));
    }
    let runtime = launcher::manifest::code_runtime(&manifest, &app.payload_root)
        .ok_or_else(|| format!("{} runs on no tilolang runtime", app.app_id))?;
    let runtime = runtime_payload(catalog, app, runtime)?;
    if apps.data_home.as_os_str().is_empty() {
        return Err("refusing to start a connector: HOME is not set".into());
    }
    let container =
        launcher_macos::policy::prepare_container(&apps.data_home, Some(workspace), &app.app_id)
            .map_err(|error| {
                format!(
                    "refusing to start {}'s connector: its container: {error}",
                    app.app_id
                )
            })?;
    let (capabilities, egress) = apps.launch_capabilities(app, workspace, &manifest);
    let request = launcher_macos::LaunchRequest {
        package_id: app.app_id.to_string(),
        executable: runtime.join(launcher::manifest::TILOLANG_BIN),
        args: Vec::new(),
        payload_dir: app.payload_root.clone(),
        runtime_dir: Some(runtime),
        container_dir: container,
        capabilities,
        egress,
        inherit_fds: vec![socket.as_raw_fd()],
        stderr: None,
        env: [
            (
                runtime_portal::origin_connector::ORIGIN_ENV,
                origin.to_owned(),
            ),
            (
                runtime_portal::process_link::FD_ENV,
                socket.as_raw_fd().to_string(),
            ),
        ]
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .to_vec(),
        trust: launcher_macos::TrustRequirement::DeveloperId,
    };
    launcher_macos::launch(&request).map_err(|error| error.to_string())
}

/// Where the GStreamer a runtime package carries beside its binaries (`lib/gstreamer`, the tilolang
/// runtime's on an Apple silicon Mac) finds its plugins, scanner, fonts and subtitle data: only
/// in the runtime, which the sandbox lets the app read and nothing write. The registry and the
/// font cache go to the app's own cache directory, as everything it caches does.
fn gstreamer_env(runtime: &std::path::Path) -> Vec<(OsString, OsString)> {
    let root = runtime.join("lib/gstreamer");
    if !root.is_dir() {
        return Vec::new();
    }
    let plugins = root.join("PlugIns");
    let scanner = root.join("Helpers/gst-plugin-scanner");
    let fonts = root.join("Resources/fontconfig");
    super::verified::gstreamer_env(plugins, scanner)
        .into_iter()
        .chain([
            ("FONTCONFIG_FILE", fonts.join("fonts.conf")),
            ("FONTCONFIG_PATH", fonts),
            ("PANGOCAIRO_BACKEND", "fontconfig".into()),
            ("LIBTHAI_DICTDIR", root.join("Resources/libthai")),
        ])
        .map(|(key, value)| (OsString::from(key), value.into_os_string()))
        .collect()
}

/// The file chooser a confined app asks for over its end of a socket pair:
/// this runtime shows every panel, outside the app's sandbox, and grants the
/// app exactly what the person picked (`tilo_dialogs::confined`).
struct Chooser {
    app_end: std::os::unix::net::UnixStream,
}

impl Chooser {
    /// Serve `package_id`'s chooser requests until the app closes its end.
    fn start(package_id: &str) -> std::io::Result<Self> {
        let (app_end, runtime_end) = std::os::unix::net::UnixStream::pair()?;
        let exe = std::env::current_exe()?;
        let package = package_id.to_owned();
        std::thread::Builder::new()
            .name("tilo-app-chooser".into())
            .spawn(move || {
                let shown = tilo_dialogs::confined::serve(runtime_end, |request| {
                    let mut helper = std::process::Command::new(&exe);
                    helper.arg(crate::cli::CHOOSER_HELPER_ARG);
                    tilo_dialogs::confined::show_in_helper(helper, request)
                });
                if let Err(error) = shown {
                    tracing::warn!(%package, %error, "the app's file chooser stopped");
                }
            })?;
        Ok(Self { app_end })
    }
}

/// The camera and microphone a confined app asks for over its end of a
/// socket pair: this runtime, which holds the TCC permission, captures on its
/// behalf once the person consents, and only the devices the app declares
/// (`camera` / `microphone` in its `[capabilities]`), as the Linux launcher
/// binds them (`tilo_capture::broker`).
struct Capture {
    app_end: std::os::unix::net::UnixDatagram,
}

impl Capture {
    fn start(
        apps: &Apps,
        app: &InstalledApp,
        manifest: &launcher::manifest::ManifestView,
    ) -> std::io::Result<Self> {
        use tilo_capture::DeviceKind;
        use tilo_capture::broker::{Policy, Sources, SystemSources};
        let declared = [DeviceKind::Camera, DeviceKind::Microphone]
            .into_iter()
            .filter(|device| {
                launcher::capabilities::declares(manifest, crate::apps::device_cap(*device))
            })
            .collect();
        let (app_end, runtime_end) = std::os::unix::net::UnixDatagram::pair()?;
        let exe = std::env::current_exe()?;
        let name = app.display_name.clone();
        let ask = move |device: DeviceKind| {
            let args = crate::i18n::device_args(&name, device);
            tilo_dialogs::confined::Consent {
                message: crate::i18n::t_args("device-consent-question", &args),
                detail: crate::i18n::t_args("device-consent-detail", &args),
                allow: crate::i18n::t("device-consent-allow"),
                deny: crate::i18n::t("device-consent-deny"),
            }
        };
        let policy: std::sync::Arc<dyn Policy> =
            std::sync::Arc::new(tilo_capture::broker::Asking {
                answers: DeviceAnswers {
                    grants: apps.device_grants().clone(),
                    app: app.app_id.to_string(),
                },
                declared,
                consent: move |device: DeviceKind| {
                    let mut helper = std::process::Command::new(&exe);
                    helper.arg(crate::cli::CONSENT_HELPER_ARG);
                    tilo_dialogs::confined::ask_consent_in_helper(helper, &ask(device))
                },
            });
        let sources: std::sync::Arc<dyn Sources> = std::sync::Arc::new(SystemSources);
        let package = app.app_id.to_string();
        std::thread::Builder::new()
            .name("tilo-app-capture".into())
            .spawn(move || {
                if let Err(error) = tilo_capture::broker::serve(&runtime_end, &policy, &sources) {
                    tracing::warn!(%package, %error, "the app's capture broker stopped");
                }
            })?;
        Ok(Self { app_end })
    }
}

/// The person's answers about one app's camera and microphone on this device, kept in their own
/// store.
struct DeviceAnswers {
    grants: tilo_peer_state::DeviceGrants,
    app: String,
}

impl tilo_capture::broker::Answers for DeviceAnswers {
    fn answer(&self, device: tilo_capture::DeviceKind) -> Option<bool> {
        use tilo_action_policy::Answer;
        match self
            .grants
            .decide(&self.app, None, crate::apps::device_cap(device))
        {
            Answer::Allow => Some(true),
            Answer::Deny => Some(false),
            Answer::Ask => None,
        }
    }

    fn keep(&self, device: tilo_capture::DeviceKind, allowed: bool) -> Result<(), String> {
        use tilo_action_policy::Answer;
        use tilo_action_types::Lifetime;
        let answer = if allowed { Answer::Allow } else { Answer::Deny };
        self.grants.answer(
            &self.app,
            crate::apps::device_cap(device),
            answer,
            Lifetime::UntilRevoked,
        );
        Ok(())
    }
}

/// The payload of the installed runtime package that provides `runtime` (`<name>/<version>`,
/// the app's `[entrypoint] runtime`), proven as the app's own is: the app's binary is there.
///
/// # Errors
///
/// When no installed package provides it, or the one that does fails [`verify_package`]; the
/// launch is refused.
fn runtime_payload(
    catalog: &package_store::Catalog,
    app: &InstalledApp,
    runtime: &str,
) -> Result<std::path::PathBuf, String> {
    installed_runtime(catalog, runtime)
        .map_err(|why| format!("refusing to launch {}: {why}", app.app_id))
}
