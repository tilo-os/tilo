//! Launching a tilolang app's window on Windows: the tilolang runtime's own binary, from the
//! installed runtime package (verified as on macOS), in the app's `AppContainer`
//! ([`crate::device::capabilities::launch_confined`]). Only tilolang apps run on Windows.
//!
//! The window runs no instance of its own: the instance runs in the app's tilolang process
//! ([`crate::device::processes`]), where agents and other devices call its actions, and the
//! window takes its patch connection on its process link, as a Mac's does
//! ([`super::Apps::hosted_window`]). It holds none of the app's data.

use super::verified::{installed_runtime, verify_installed};
use super::{Apps, Launch, Shows};
use app_catalogue::apps::InstalledApp;
use runtime_portal::process_link::Packet as _;

/// Start the window of the tilolang app `app` in `workspace`, opening `open` when it names a
/// record.
pub(super) fn launch(
    apps: &Apps,
    app: &InstalledApp,
    workspace: &str,
    shows: Shows,
    open: Option<&str>,
) -> Result<Launch, String> {
    if shows != Shows::Window {
        // A tilolang app's code runs with no window in its tilolang process
        // ([`super::instances::Instances::start_with_no_view`]); nothing else runs here.
        return Err(format!(
            "{} runs on this Windows device only as a tilolang app's window",
            app.app_id
        ));
    }
    let catalog = apps
        .catalog
        .as_ref()
        .ok_or("refusing to launch: no app store is available")?;
    verify_installed(catalog, app)?;
    let manifest = launcher::manifest::read_manifest(&app.manifest_path)
        .map_err(|error| format!("refusing to launch {}: {error}", app.app_id))?;
    let runtime = manifest
        .entrypoint
        .runtime
        .as_deref()
        .filter(|runtime| launcher::manifest::is_tilolang(runtime))
        .ok_or_else(|| {
            format!(
                "refusing to launch {}: only tilolang apps run on this Windows device",
                app.app_id
            )
        })?;
    let runtime = installed_runtime(catalog, runtime)
        .map_err(|why| format!("refusing to launch {}: {why}", app.app_id))?;
    let window = apps.hosted_window(&app.app_id, workspace)?;
    let mut command = std::process::Command::new(
        runtime
            .join(launcher::manifest::TILOLANG_BIN)
            .with_extension(std::env::consts::EXE_EXTENSION),
    );
    command
        // The script the runtime's binary runs comes first, from the payload, then what to open.
        .args(
            manifest
                .entrypoint
                .script
                .iter()
                .map(|script| app.payload_root.join(script).into_os_string())
                .chain(open.map(Into::into)),
        )
        .env(launcher::manifest::APP_MODULE_ENV, &app.payload_root)
        .env("TILO_PACKAGE_ID", app.app_id.as_str())
        .env("TILO_WORKSPACE_ID", workspace)
        .env(runtime_portal::process_link::HOSTED_ENV, "1")
        .env("TILO_CONTINUITY_INSTANCE", window.instance.to_string())
        .envs(gstreamer_env(&runtime));
    let (link, mut confined) =
        crate::device::capabilities::launch_confined(launcher_windows::LaunchRequest {
            package_id: app.app_id.to_string(),
            command,
            // What it caches goes to its container's own folder, which Windows gives every
            // AppContainer.
            read: vec![runtime, app.payload_root.clone()],
            read_write: Vec::new(),
        })
        .map_err(|error| format!("{} did not start: {error}", app.app_id))?;
    if let Err(error) = (runtime_portal::process_link::ToProcess::Patch {
        instance: window.instance,
        patch: window.patch,
    })
    .send(&link)
    {
        let pid = confined.pid();
        let _ = confined.take_stderr();
        confined.kill();
        return Err(format!(
            "{}'s window (process {pid}) was not handed its view: {error}",
            app.app_id
        ));
    }
    Ok(Launch {
        pid: confined.pid(),
        app: confined,
        _link: link,
    })
}

/// Where the GStreamer the runtime package carries (its DLLs beside `tilolang-process.exe`, its
/// plugins in `lib/gstreamer-1.0`, tools/ci/windows/build.sh) finds its plugins and its scanner:
/// only in the runtime, which the container reads and nothing writes. Its registry goes where
/// the window caches everything, its container's own folder.
fn gstreamer_env(runtime: &std::path::Path) -> Vec<(&'static str, std::path::PathBuf)> {
    let plugins = runtime.join("lib/gstreamer-1.0");
    if !plugins.is_dir() {
        return Vec::new();
    }
    let scanner = runtime.join("lib/libexec/gstreamer-1.0/gst-plugin-scanner.exe");
    vec![
        ("GST_PLUGIN_SYSTEM_PATH_1_0", plugins.clone()),
        ("GST_PLUGIN_SYSTEM_PATH", plugins),
        ("GST_PLUGIN_PATH_1_0", std::path::PathBuf::new()),
        ("GST_PLUGIN_PATH", std::path::PathBuf::new()),
        ("GST_PLUGIN_SCANNER_1_0", scanner.clone()),
        ("GST_PLUGIN_SCANNER", scanner),
    ]
}
