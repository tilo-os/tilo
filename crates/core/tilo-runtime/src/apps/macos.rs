//! Launching through `launcher-macos`: App Sandbox for a trusted signed app,
//! a deny-default Seatbelt profile for everything else.
//!
//! The launcher's contract is that its caller has verified the payload
//! against the package's signed root first ([`verify_installed`]);
//! an unverifiable payload is refused before it reaches the launcher. A
//! package whose entrypoint is a runtime's binary (a tilolang app on the
//! tilolang runtime) runs that binary from the installed runtime package,
//! verified the same way, over its own payload (`TILO_APP_MODULE`), as the
//! Linux launcher runs it. The
//! app inherits its service connections already handshaken with its
//! identity, so a sandbox that denies it every socket path still
//! collaborates.

use super::{Apps, Launch, Sockets};
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
    let container =
        launcher_macos::policy::prepare_container(&apps.data_home, Some(workspace), &app.app_id)
            .map_err(|error| {
                format!("refusing to launch {}: its container: {error}", app.app_id)
            })?;
    let services = ServiceConnections::connect(
        &app.app_id,
        workspace,
        &ServiceSockets {
            collab: &sockets.collab,
            identity: &sockets.identity,
            blobs: &sockets.blobs,
        },
    )
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
    }
    let (capabilities, egress) = apps.launch_capabilities(app, workspace, &manifest);
    let request = launcher_macos::LaunchRequest {
        package_id: app.app_id.to_string(),
        executable: runtime
            .as_deref()
            .unwrap_or(&app.payload_root)
            .join(executable),
        args: Vec::new(),
        payload_dir: app.payload_root.clone(),
        runtime_dir: runtime,
        container_dir: container,
        capabilities,
        egress,
        inherit_fds,
        env,
        trust: launcher_macos::TrustRequirement::DeveloperId,
    };
    let launched = launcher_macos::launch(&request).map_err(|error| error.to_string())?;
    // The app holds its own descriptors now; this process's copies go.
    drop(services);
    drop(chooser.app_end);
    drop(capture.app_end);
    let pid = u32::try_from(launched.pid())
        .map_err(|_| format!("{} started with an invalid pid", app.app_id))?;
    Ok(Launch { pid, app: launched })
}

/// Install the runtime packages the running Tilo.app carries (`Contents/Resources/runtimes/<package
/// root>.tapp`, the tilolang runtime among them) that this profile does not have yet, through the
/// one installer, as the Store installs any package: each must be the exact package its sealed
/// name says and its bundle signed by a publisher the app lists. Once installed, a runtime updates
/// from the registry like every package; a later Tilo.app does not put back an older one.
pub(super) fn install_carried_runtimes(catalog: &package_store::Catalog) {
    use package_store::installer;
    let Some(carried) = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("Resources/runtimes")))
        .filter(|dir| dir.is_dir())
    else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&carried) else {
        return;
    };
    let installed: Vec<String> = catalog
        .list()
        .map(|packages| packages.into_iter().map(|p| p.package_id_hex).collect())
        .unwrap_or_default();
    let store = match package_store::PackageStore::from_env() {
        Ok(store) => store,
        Err(error) => {
            tracing::warn!(%error, "no package store for the runtimes Tilo.app carries");
            return;
        }
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        let Some(root) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| path.extension().is_some_and(|ext| ext == "tapp"))
            .filter(|stem| stem.len() == 64 && stem.bytes().all(|b| b.is_ascii_hexdigit()))
        else {
            continue;
        };
        if installed.iter().any(|id| id.eq_ignore_ascii_case(root)) {
            continue;
        }
        let request = installer::Request {
            trust: installer::Trust {
                require_package_root: Some(root.to_owned()),
                // No attestor vouches for the app's own publisher: the package root, sealed in the
                // app's signature, is what this trusts.
                allow_self_signed: true,
                ..installer::Trust::default()
            },
            ..installer::Request::default()
        };
        match installer::install(
            &store,
            catalog,
            &installer::Candidate::staged(&path, "Tilo.app"),
            &request,
            None,
        ) {
            Ok(_) | Err(installer::Error::AlreadyInstalled(_)) => {}
            Err(error) => {
                tracing::warn!(%error, package = %root, "a runtime Tilo.app carries did not install")
            }
        }
    }
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
    [
        ("GST_PLUGIN_SYSTEM_PATH_1_0", plugins.clone()),
        ("GST_PLUGIN_SYSTEM_PATH", plugins),
        ("GST_PLUGIN_PATH_1_0", std::path::PathBuf::new()),
        ("GST_PLUGIN_PATH", std::path::PathBuf::new()),
        ("GST_PLUGIN_SCANNER_1_0", scanner.clone()),
        ("GST_PLUGIN_SCANNER", scanner),
        ("FONTCONFIG_FILE", fonts.join("fonts.conf")),
        ("FONTCONFIG_PATH", fonts),
        ("PANGOCAIRO_BACKEND", "fontconfig".into()),
        ("LIBTHAI_DICTDIR", root.join("Resources/libthai")),
    ]
    .into_iter()
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
        use tilo_action_policy::{Answer, Lifetime};
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

/// Prove the installed package behind `app` is still the one that was
/// installed: its record's signature by its publisher, the manifest and
/// every payload file against the signed hashes, no file the package did
/// not ship, and a publisher key this profile has not revoked.
///
/// # Errors
///
/// Names the first check that failed; the launch is refused.
pub(super) fn verify_installed(
    catalog: &package_store::Catalog,
    app: &InstalledApp,
) -> Result<(), String> {
    let refuse = |why: String| format!("refusing to launch {}: {why}", app.app_id);
    let package = catalog
        .list()
        .map_err(|error| refuse(error.to_string()))?
        .into_iter()
        .find(|package| package.manifest_path == app.manifest_path)
        .ok_or_else(|| refuse("it is no longer installed".into()))?;
    verify_package(&package).map_err(refuse)
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
    let refuse = |why: String| format!("refusing to launch {}: {why}", app.app_id);
    let provides = |package: &package_store::registry::InstalledPackage| {
        let manifest = std::fs::read_to_string(&package.manifest_path)
            .ok()
            .and_then(|source| tilo_app_package::Manifest::from_toml_str(&source).ok());
        manifest.is_some_and(|manifest| {
            manifest.package.kind == tilo_app_package::Kind::Runtime
                && manifest.package.provides.as_deref() == Some(runtime)
        })
    };
    let package = catalog
        .list()
        .map_err(|error| refuse(error.to_string()))?
        .into_iter()
        .find(provides)
        .ok_or_else(|| {
            refuse(format!(
                "the runtime it runs on, {runtime}, is not installed"
            ))
        })?;
    verify_package(&package).map_err(|why| refuse(format!("its runtime {runtime}: {why}")))?;
    Ok(package.payload_root)
}

/// Prove `package` is still what was installed: its record's signature by its publisher, the
/// manifest and every payload file against the signed hashes, no file it did not ship, and a
/// publisher key this profile has not revoked. The error says which check failed.
fn verify_package(package: &package_store::registry::InstalledPackage) -> Result<(), String> {
    use package_store::integrity;
    let report =
        integrity::verify(package, &package.payload_root).map_err(|error| error.to_string())?;
    if let Some(fault) = report.faults.first() {
        return Err(format!("its payload changed after install ({fault})"));
    }
    if let Some(unexpected) = report.unexpected.first() {
        return Err(format!(
            "its payload holds a file the package did not ship ({unexpected})"
        ));
    }
    let signed = integrity::read_record(
        &integrity::record_dir(package),
        &package.id,
        &package.package_id_hex,
    )
    .map_err(|error| error.to_string())?;
    let revoked = tilo_app_package::RevocationList {
        revoked: tilo_control::packages::trust()
            .1
            .into_iter()
            .map(|key| tilo_app_package::RevokedKey {
                pubkey_hex: key.pubkey_hex,
                reason: key.reason,
            })
            .collect(),
    };
    if let Some(entry) = revoked.find(&signed.root.publisher_identity) {
        return Err(format!("its publisher's key is revoked ({})", entry.reason));
    }
    Ok(())
}
