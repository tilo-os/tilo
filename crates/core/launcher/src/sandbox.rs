//! Bubblewrap argv construction from a [`ManifestView`].
//!
//! Pure function: given a manifest, payload root, the app's runtime root, and resolved
//! service fds, returns the argv for `bwrap`.

use std::path::{Path, PathBuf};

use crate::capabilities::CapabilityPlan;
use crate::manifest::ManifestView;

pub fn payload_mount_for(package_id: &str) -> PathBuf {
    PathBuf::from("/var/tilo-apps").join(package_id)
}

/// In-sandbox mount root for the runtime (`tilo-runtime/<version>/`).
pub fn runtime_mount_for(version: &str) -> PathBuf {
    PathBuf::from("/run/tilo-runtime").join(version)
}

/// Where a sandbox's own store of runtime bundles is mounted, when it is handed one.
pub const RUNTIME_STORE_MOUNT: &str = "/run/tilo-runtimes";

/// Mount the device's runtime store at [`RUNTIME_STORE_MOUNT`], read-only, and name it to the
/// sandbox: a process that runs no app yet finds any bundle it may become an app of. The Browser
/// draws its pages with the Chromium engine, a runtime of its own, at the version its manifest
/// names.
pub fn push_runtime_store(argv: &mut Vec<String>, root: &Path) {
    let mount = Path::new(RUNTIME_STORE_MOUNT);
    push_bind(argv, "--ro-bind", root, mount);
    argv.extend([
        "--setenv".to_owned(),
        runtime_bundle::RUNTIME_DIR_ENV.to_owned(),
        mount.display().to_string(),
    ]);
}

/// The `LD_LIBRARY_PATH` of a sandbox running on the runtime bundle mounted for `version`: the bundle's
/// `lib/` directory.
pub fn runtime_library_path(version: &str) -> PathBuf {
    runtime_mount_for(version).join("lib")
}

/// The `NODE_PATH` of a sandbox running a script on the runtime bundle mounted for `version`: the
/// bundle's `lib/node_modules`, where bun resolves a package the runtime provides (the SDK's native
/// addon) that the app carries none of.
pub fn runtime_node_path(version: &str) -> PathBuf {
    runtime_mount_for(version).join("lib/node_modules")
}

/// The selected signed SDK's catalog, private to this application's namespace.
pub const RUNTIME_FONT_CATALOG_PATH: &str = "/run/tilo-fonts";

pub use sandbox_core::loader_preflight::CANONICAL_LOADER_PATHS;

/// Plan for exposing a filtered D-Bus proxy into the sandbox.
#[derive(Debug, Clone)]
pub struct DBusBindPlan {
    pub host_socket: PathBuf,
    /// In-sandbox path, read via `DBUS_SESSION_BUS_ADDRESS=unix:path=<sandbox_path>`.
    pub sandbox_path: PathBuf,
}

/// Where a GUI app's Wayland socket appears inside the sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaylandBind {
    pub host_socket: PathBuf,
    pub sandbox_socket: PathBuf,
    pub sandbox_runtime_dir: PathBuf,
    /// Value of `WAYLAND_DISPLAY` inside the sandbox.
    pub display_name: String,
}

/// Build the [`WaylandBind`] plan, exposed at `/run/user/<uid>`.
pub fn wayland_bind_for(host_socket: &Path, uid: u32) -> WaylandBind {
    let runtime_dir = PathBuf::from(format!("/run/user/{uid}"));
    let display_name = "wayland-0".to_string();
    WaylandBind {
        host_socket: host_socket.to_path_buf(),
        sandbox_socket: runtime_dir.join(&display_name),
        sandbox_runtime_dir: runtime_dir,
        display_name,
    }
}

/// Splice a Wayland display into an argv built by [`build_bwrap_argv`], before the trailing entrypoint
/// token.
pub fn push_wayland_bind(argv: &mut Vec<String>, bind: &WaylandBind) {
    push_bind(argv, "--ro-bind", &bind.host_socket, &bind.sandbox_socket);
    argv.push("--setenv".to_string());
    argv.push("WAYLAND_DISPLAY".to_string());
    argv.push(bind.display_name.clone());
    argv.push("--setenv".to_string());
    argv.push("XDG_RUNTIME_DIR".to_string());
    argv.push(bind.sandbox_runtime_dir.display().to_string());
    // Redundant once `push_clean_environment` runs; belt-and-braces.
    argv.push("--unsetenv".to_string());
    argv.push(PRIVILEGED_WAYLAND_ENV.to_string());
}

/// The `PATH` a sandboxed app runs with, fixed rather than inherited — a writable dir on the session's
/// PATH would decide which binary `exec` finds.
pub const SANDBOX_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// The whole of what a sandboxed app inherits from the launching session.
pub const INHERITED_ENV: &[&str] = &[
    // `LANGUAGE` is the GNU fallback chain, `LC_ALL` overrides the rest.
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_COLLATE",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_MONETARY",
    "LC_NUMERIC",
    "LC_TIME",
    // `/etc/localtime` is in the system profile; covers a session override.
    "TZ",
    // Absent for a GUI launch: inherited-when-set, not given a default.
    "TERM",
    "COLORTERM",
    // Without it `EnvFilter::from_default_env()` drops app diagnostics silently.
    "RUST_LOG",
    // Without it an app's panic says what failed and never where: the trace names no secret.
    "RUST_BACKTRACE",
];

/// User-owned type-only accessibility preferences allowed into applications.
pub const ACCESSIBILITY_TEXT_SCALE_ENV: &[&str] = &["TILO_A11Y_TEXT_SCALE", "TILO_TEXT_SCALE"];

fn push_accessibility_environment(argv: &mut Vec<String>) {
    for key in ACCESSIBILITY_TEXT_SCALE_ENV {
        let Some(value) = std::env::var_os(key) else {
            continue;
        };
        let Some(value) = value.to_str() else {
            continue;
        };
        let Ok(scale) = value.trim().parse::<f32>() else {
            continue;
        };
        if !scale.is_finite() || !(0.5..=4.0).contains(&scale) {
            continue;
        }
        argv.extend([
            "--setenv".to_string(),
            (*key).to_string(),
            scale.to_string(),
        ]);
    }
}

/// Empty the environment, then put back what the app is entitled to.
pub fn push_clean_environment(argv: &mut Vec<String>) {
    argv.push("--clearenv".to_string());
    argv.push("--setenv".to_string());
    argv.push("PATH".to_string());
    argv.push(SANDBOX_PATH.to_string());
    for key in INHERITED_ENV {
        let Some(value) = std::env::var_os(key) else {
            continue;
        };
        // Avoids two different empty/unset states for no reason.
        if value.is_empty() {
            continue;
        }
        argv.push("--setenv".to_string());
        argv.push((*key).to_string());
        argv.push(value.to_string_lossy().into_owned());
    }
    // Every launch: no transparent huge pages in its heap.
    argv.extend([
        "--setenv".into(),
        "GLIBC_TUNABLES".into(),
        crate::hardening::tunables(false),
    ]);
    push_accessibility_environment(argv);
    // Fixed content-free presentation/GPU timing booleans, never an input injector or output path.
    for name in [
        "TILO_PROFILE_PRESENTATION",
        "TILO_PROFILE_GPU_STAGES",
        "VELLO_GPU_TIMESTAMPS",
        "VELLO_GPU_DISPATCH_TIMESTAMPS",
    ] {
        if std::env::var(name).is_ok_and(|value| value == "1") {
            argv.extend(["--setenv".into(), name.into(), "1".into()]);
        }
    }
}

/// Env var publishing the compositor's privileged Wayland socket.
pub const PRIVILEGED_WAYLAND_ENV: &str = "TILO_WAYLAND_PRIVILEGED";

/// Env var naming the workspace an app launched into — `ui::collaboration` uses it as the
/// **collaboration cluster id** CRDT deltas are addressed to, so apps in different workspaces never
/// share traffic.
pub const WORKSPACE_ID_ENV: &str = "TILO_WORKSPACE_ID";

/// Attach [`crate::hardening::RuntimeHardening`] to a sandbox: the preload library must also be bound,
/// since an unbound `LD_PRELOAD` path just falls back to the ordinary allocator with a stderr warning,
/// not an error.
pub fn push_runtime_hardening(
    argv: &mut Vec<String>,
    hardening: &crate::hardening::RuntimeHardening,
) {
    if let Some(allocator) = &hardening.allocator {
        // Same path inside and out, so LD_PRELOAD and the bind cannot drift.
        push_bind(argv, "--ro-bind", allocator, allocator);
        argv.push("--setenv".to_string());
        argv.push("LD_PRELOAD".to_string());
        argv.push(allocator.display().to_string());
    }
    if hardening.shadow_stack {
        // Marking the binary isn't enough — glibc also needs this tunable for a shadow stack request.
        argv.push("--setenv".to_string());
        argv.push("GLIBC_TUNABLES".to_string());
        argv.push(crate::hardening::tunables(true));
    }
}

pub fn push_workspace_id(argv: &mut Vec<String>, workspace_id: &str) {
    argv.push("--setenv".to_string());
    argv.push(WORKSPACE_ID_ENV.to_string());
    argv.push(workspace_id.to_string());
}

/// The DRM device directory: the system profile binds it (every GPU on a multi-GPU host);
/// [`push_gpu_selection`] narrows to one.
const DRM_DIR: &str = "/dev/dri";

/// Where a shell-issued startup trace may land: the app's state directory, never its home.
const STARTUP_METRICS_PREFIX: &str = "/run/tilo/app/.local/state/startup-";

/// Splice the compositor's GPU choice into the argv, before the entrypoint token.
pub fn push_gpu_selection(
    argv: &mut Vec<String>,
    selection: &crate::gpu::GpuSelection,
    grants_drm: bool,
) {
    if grants_drm {
        // tmpfs lands on top of the manifest's bind, node lands on top of tmpfs.
        argv.push("--tmpfs".to_string());
        argv.push(DRM_DIR.to_string());
        argv.push("--dev-bind-try".to_string());
        argv.push(selection.node.display().to_string());
        argv.push(selection.node.display().to_string());
    }
    for file in selection.manifest_files() {
        push_bind(argv, "--ro-bind-try", &file, &file);
    }
    for (key, value) in selection.env() {
        argv.push("--setenv".to_string());
        argv.push(key.to_string());
        argv.push(value);
    }
    if let Some(seed) = &selection.pipeline_seed {
        push_pipeline_seed(argv, seed);
    }
}

/// The loader pin of a GPU-less machine (`gpu::SoftwarePin`): its manifests read-only and the variables
/// that name them.
pub fn push_software_pin(argv: &mut Vec<String>, pin: &crate::gpu::SoftwarePin) {
    for file in pin.manifest_files() {
        push_bind(argv, "--ro-bind-try", &file, &file);
    }
    for (key, value) in pin.env() {
        argv.extend(["--setenv".to_string(), key.to_string(), value]);
    }
}

/// Bind the trusted session seed read-only, independently of DRM access.
pub fn push_pipeline_seed(argv: &mut Vec<String>, seed: &Path) {
    push_bind(
        argv,
        "--ro-bind-try",
        seed,
        Path::new(crate::gpu::SANDBOX_PIPELINE_SEED_DIR),
    );
    argv.extend([
        "--setenv".to_string(),
        crate::gpu::PIPELINE_SEED_ENV.to_string(),
        crate::gpu::SANDBOX_PIPELINE_SEED_DIR.to_string(),
    ]);
}

/// The Landlock rules that make [`push_gpu_selection`]'s binds usable: the selection is only real if
/// Landlock agrees.
pub fn push_gpu_landlock_rules(argv: &mut Vec<String>, selection: &crate::gpu::GpuSelection) {
    for file in selection.manifest_files() {
        argv.push("--ro".to_string());
        argv.push(file.display().to_string());
    }
    argv.push("--rw".to_string());
    argv.push(selection.node.display().to_string());
    if selection.pipeline_seed.is_some() {
        argv.push("--ro".to_string());
        argv.push(crate::gpu::SANDBOX_PIPELINE_SEED_DIR.to_string());
    }
}

/// Whether this launch exposes `/dev/dri` at all: an app granted `gpu` ([`crate::gpu::granted`]), or a
/// capability naming a DRM path.
pub fn grants_drm(gpu_granted: bool, capabilities: &CapabilityPlan) -> bool {
    let from_profile = gpu_granted
        && crate::profile::GPU_DEVICE_PATHS
            .iter()
            .any(|entry| is_drm_path(Path::new(entry)));
    let from_capabilities = capabilities
        .ro_paths
        .iter()
        .chain(capabilities.rw_paths.iter())
        .any(|p| is_drm_path(p));
    from_profile || from_capabilities
}

fn is_drm_path(p: &Path) -> bool {
    p == Path::new(DRM_DIR) || p.starts_with(DRM_DIR)
}

/// The host's human identity directory exposed to Files.
pub const SANDBOX_TRUSTED_IDENTITIES_DIR: &str = "/run/tilo/identity-projection";

/// Bind one already-staged identity projection into Files.
pub fn push_trusted_identities_bind(argv: &mut Vec<String>, host_path: &Path) {
    let host_directory = host_path.parent().unwrap_or(host_path);
    push_bind(
        argv,
        "--ro-bind",
        host_directory,
        Path::new(SANDBOX_TRUSTED_IDENTITIES_DIR),
    );
}

/// Build the bwrap argv from a manifest plus the launcher's resolved inputs: `runtime_root`,
/// `service_binds` (fds reach the child by inheritance, not argv), and `dbus_bind`.
pub fn build_bwrap_argv(
    manifest: &ManifestView,
    payload_root: &Path,
    runtime: Option<(&Path, &str)>,
    service_binds: &[ServiceBind],
    dbus_bind: Option<&DBusBindPlan>,
    capabilities: &CapabilityPlan,
) -> Vec<String> {
    // GUI launch: pick the GUI-flavour bin, falling back to the legacy top-level `bin`.
    let entry = manifest
        .entrypoint
        .gui_bin()
        .unwrap_or(&manifest.entrypoint.bin);
    build_bwrap_argv_for_entry(
        manifest,
        payload_root,
        runtime,
        service_binds,
        dbus_bind,
        entry,
        capabilities,
    )
}

/// The compiled surface the package ships (`surface.json` at its payload root), by its path in the
/// sandbox and its hash: the app reads it from there, checks the hash and names it in every change
/// it signs.
fn push_surface(argv: &mut Vec<String>, payload_root: &Path, payload_mount: &Path) {
    let Ok(bytes) = std::fs::read(payload_root.join("surface.json")) else {
        return;
    };
    argv.extend([
        "--setenv".to_string(),
        "TILO_SURFACE".to_string(),
        payload_mount.join("surface.json").display().to_string(),
        "--setenv".to_string(),
        "TILO_SURFACE_HASH".to_string(),
        blake3::hash(&bytes).to_hex().to_string(),
    ]);
}

fn push_continuity_environment(argv: &mut Vec<String>) {
    for key in [
        "TILO_CONTINUITY_INSTANCE",
        "TILO_CONTINUITY_RESUME_SCHEMA",
        "TILO_CONTINUITY_RESUME_HEX",
        tilo_process::environment::NO_WINDOW_ENV,
        tilo_process::environment::NO_WINDOW_IDLE_ENV,
        // The item the window opens on (a conversation another member's send landed in): a
        // process started for that window learns it nowhere else.
        tilo_process::environment::REVEAL_ENV,
    ] {
        let Ok(value) = std::env::var(key) else {
            continue;
        };
        let allowed =
            !value.is_empty() && value.len() <= 64 * 1024 && !value.contains(['\0', '\n', '\r']);
        if allowed
            && (key != "TILO_CONTINUITY_INSTANCE"
                || (value.len() <= 32 && value.bytes().all(|byte| byte.is_ascii_digit())))
            && (key != "TILO_CONTINUITY_RESUME_SCHEMA"
                || (value.len() <= 5
                    && value.bytes().all(|byte| byte.is_ascii_digit())
                    && value.parse::<u16>().is_ok_and(|version| version > 0)))
            && (key != tilo_process::environment::NO_WINDOW_ENV || value == "1")
            && (key != tilo_process::environment::NO_WINDOW_IDLE_ENV
                || (value.len() <= 5
                    && value.bytes().all(|byte| byte.is_ascii_digit())
                    && value
                        .parse::<u32>()
                        .is_ok_and(|seconds| (1..=86_400).contains(&seconds))))
            && (key != "TILO_CONTINUITY_RESUME_HEX"
                || (value.len() <= 64 * 1024 && value.bytes().all(|byte| byte.is_ascii_hexdigit())))
        {
            argv.extend(["--setenv".to_string(), key.to_string(), value]);
        }
    }
    // The shell owns this path and always points it at the app's state directory inside its one
    // writable bind — not the directory itself, which is the app's home and what Files shows.
    if let Ok(value) = std::env::var("TILO_STARTUP_METRICS_PATH")
        && value.starts_with(STARTUP_METRICS_PREFIX)
        && std::path::Path::new(&value)
            .extension()
            .is_some_and(|extension| extension == "json")
        && value.len() <= 160
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'-' | b'_'))
    {
        argv.extend([
            "--setenv".to_string(),
            "TILO_STARTUP_METRICS_PATH".to_string(),
            value,
        ]);
    }
    // Where an unattended session's Save chooser answers.
    if let Ok(value) = std::env::var(SAVE_DIRECTORY_ENV)
        && is_plain_absolute_path(&value)
    {
        argv.extend([
            "--setenv".to_string(),
            SAVE_DIRECTORY_ENV.to_string(),
            value,
        ]);
    }
}

/// The session's save directory for an unattended guest (see
/// `tilo_dialogs::portal::SAVE_DIRECTORY_ENV`).
const SAVE_DIRECTORY_ENV: &str = "TILO_SAVE_DIRECTORY";

/// An absolute path of ordinary components, bounded and free of control characters.
fn is_plain_absolute_path(value: &str) -> bool {
    let path = std::path::Path::new(value);
    value.len() <= 4096
        && !value.chars().any(char::is_control)
        && path.is_absolute()
        && path.components().all(|component| {
            matches!(
                component,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        })
}

pub use crate::manifest::APP_MODULE_ENV;

/// The sandbox directory the entrypoint's binary paths are relative to: the mount of the runtime bundle
/// `[entrypoint].runtime` names, else the package's own payload.
#[must_use]
pub fn entry_root_for(manifest: &ManifestView) -> PathBuf {
    match manifest
        .entrypoint
        .runtime
        .as_deref()
        .and_then(|runtime| runtime.split_once('/'))
    {
        Some((_, version)) => runtime_mount_for(version),
        None => payload_mount_for(&manifest.package_id),
    }
}

fn push_app_metadata(argv: &mut Vec<String>, manifest: &ManifestView, payload_mount: &Path) {
    if let Some(name) = manifest
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        argv.extend([
            "--setenv".to_string(),
            "TILO_APP_NAME".to_string(),
            name.to_string(),
        ]);
    }
    if let Some(icon) = manifest
        .icon
        .as_deref()
        .map(str::trim)
        .filter(|icon| !icon.is_empty())
    {
        argv.extend([
            "--setenv".to_string(),
            "TILO_APP_ICON".to_string(),
            payload_mount
                .join(icon.trim_start_matches('/'))
                .display()
                .to_string(),
        ]);
    }
}

/// Mount a further runtime bundle read-only at its runtime mount.
pub fn push_required_runtime(argv: &mut Vec<String>, root: &Path, version: &str) -> PathBuf {
    let mount = runtime_mount_for(version);
    push_bind(argv, "--ro-bind", root, &mount);
    mount
}

/// `GStreamer` relocates its default plugin lookup beside `libgstreamer`.
const SYSTEM_GSTREAMER_PLUGINS: &[&str] = &[
    "/usr/lib64/gstreamer-1.0",
    "/usr/lib/gstreamer-1.0",
    "/usr/lib/x86_64-linux-gnu/gstreamer-1.0",
    "/usr/lib/aarch64-linux-gnu/gstreamer-1.0",
];

fn push_gstreamer_plugins(argv: &mut Vec<String>, root: &Path, mount: &Path) {
    if !root.join("lib/libgstreamer-1.0.so.0").is_file() {
        return;
    }
    let bundled = mount.join("lib/gstreamer-1.0");
    let mut paths = vec![bundled.display().to_string()];
    paths.extend(
        SYSTEM_GSTREAMER_PLUGINS
            .iter()
            .map(|path| (*path).to_owned()),
    );
    argv.extend([
        "--setenv".into(),
        "GST_PLUGIN_SYSTEM_PATH_1_0".into(),
        paths.join(":"),
    ]);
}

/// Where a sandbox sees the device's compiled modules.
pub const COMPILED_MOUNT: &str = "/run/tilo/compiled";

/// Bind the device's compiled modules read-only and name them for the process
/// ([`runtime_portal::process_link::COMPILED_ENV`]): a tilolang module the daemon compiled runs
/// on that machine code instead of being interpreted. Nothing is bound when the daemon has
/// compiled none, and the process then interprets its modules. The store is the daemon's, and
/// read-only here: a process cannot write what it loads.
pub(crate) fn push_compiled_modules(argv: &mut Vec<String>) {
    let Some(dir) = tilo_paths::data_dir().map(|data| data.join("tilolang-compiled")) else {
        return;
    };
    if !dir.is_dir() {
        return;
    }
    argv.extend([
        "--ro-bind".to_string(),
        dir.display().to_string(),
        COMPILED_MOUNT.to_string(),
    ]);
    argv.extend([
        "--setenv".to_string(),
        runtime_portal::process_link::COMPILED_ENV.to_string(),
        COMPILED_MOUNT.to_string(),
    ]);
}

/// Bind the directory the device stages theme bundles in ([`theme_bundles_dir`]) read-only at
/// [`THEME_BUNDLES_MOUNT`]: the document and painter modules of the theme package the appearance
/// applied, and nothing of the package store — no registry, no other package's payload. The
/// directory is bound rather than its files, so a bundle staged after the process started is read
/// without a relaunch. Nothing is bound when no data home names one.
pub(crate) fn push_theme_bundles(argv: &mut Vec<String>) {
    let Some(dir) = runtime_portal::process_link::theme_bundles_dir() else {
        return;
    };
    // The session stages the bundles here; the sandbox only reads them.
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    argv.extend([
        "--ro-bind".to_string(),
        dir.display().to_string(),
        runtime_portal::process_link::THEME_BUNDLES_MOUNT.to_string(),
    ]);
}

/// The Landlock rules for [`push_theme_bundles`] and [`push_compiled_modules`]: the staged bundles
/// and the compiled painter sets a process reads its theme from. A rule for a path nothing bound is
/// skipped, so a call may name one unconditionally.
pub(crate) fn push_theme_bundles_landlock(argv: &mut Vec<String>) {
    if runtime_portal::process_link::theme_bundles_dir().is_some_and(|dir| dir.is_dir()) {
        argv.extend([
            "--ro".to_string(),
            runtime_portal::process_link::THEME_BUNDLES_MOUNT.to_string(),
        ]);
    }
    if tilo_paths::data_dir().is_some_and(|data| data.join("tilolang-compiled").is_dir()) {
        argv.extend(["--ro".to_string(), COMPILED_MOUNT.to_string()]);
    }
}

pub(crate) fn push_runtime_bundle(argv: &mut Vec<String>, runtime: Option<(&Path, &str)>) {
    let Some((runtime_root, runtime_version)) = runtime else {
        return;
    };
    let runtime_mount = runtime_mount_for(runtime_version);
    push_bind(argv, "--ro-bind", runtime_root, &runtime_mount);
    push_gstreamer_plugins(argv, runtime_root, &runtime_mount);
    let fonts = runtime_root.join("lib/fonts");
    if fonts.is_dir() {
        push_bind(
            argv,
            "--ro-bind",
            &fonts,
            Path::new(RUNTIME_FONT_CATALOG_PATH),
        );
    }
    argv.extend([
        "--setenv".to_string(),
        "LD_LIBRARY_PATH".to_string(),
        runtime_library_path(runtime_version).display().to_string(),
    ]);
    for canonical in CANONICAL_LOADER_PATHS {
        let in_bundle = runtime_root
            .join("lib")
            .join(canonical.trim_start_matches('/'));
        if in_bundle.exists() {
            argv.extend([
                "--ro-bind".to_string(),
                in_bundle.display().to_string(),
                (*canonical).to_string(),
            ]);
        }
    }
}

/// `build_bwrap_argv` variant for `run-installed`'s CLI entrypoint.
pub fn build_bwrap_argv_for_entry(
    manifest: &ManifestView,
    payload_root: &Path,
    runtime: Option<(&Path, &str)>,
    service_binds: &[ServiceBind],
    dbus_bind: Option<&DBusBindPlan>,
    entry_bin: &str,
    capabilities: &CapabilityPlan,
) -> Vec<String> {
    let mut argv: Vec<String> = Vec::new();

    // First, because `--clearenv` keeps only what a *later* `--setenv` puts back — including the ones
    // `run.rs` splices in after this returns.
    push_clean_environment(&mut argv);

    // Network namespace is shared ONLY for unrestricted access (host network, or a `*` grant) —
    // anything more specific keeps the empty namespace and uses the egress proxy, since `--share-net`
    // is all-or-nothing.
    argv.push("--unshare-all".to_string());
    if crate::egress_proxy::egress_decision(manifest, capabilities, payload_root)
        .shares_host_network()
    {
        argv.push("--share-net".to_string());
    }
    argv.push("--die-with-parent".to_string());

    let payload_mount = payload_mount_for(&manifest.package_id);
    push_bind(&mut argv, "--ro-bind", payload_root, &payload_mount);
    // The machine code the device's daemon compiled for this app's modules.
    push_compiled_modules(&mut argv);
    // The bundles this device staged for its apps: the process reads the applied theme's bundle
    // from there and draws the theme the shell draws.
    push_theme_bundles(&mut argv);

    // TILO_PACKAGE_ID picks a per-app fallback state dir in standalone mode.
    argv.push("--setenv".to_string());
    argv.push("TILO_PACKAGE_ID".to_string());
    argv.push(manifest.package_id.clone());
    push_surface(&mut argv, payload_root, &payload_mount);
    // An entrypoint that executes its runtime's binary is told where the package it runs is: the
    // payload is then the app (a tilolang app's compiled module and metadata), and the binary is the
    // runtime's.
    if manifest.entrypoint.runtime.is_some() {
        argv.extend([
            "--setenv".to_string(),
            APP_MODULE_ENV.to_string(),
            payload_mount.display().to_string(),
        ]);
    }

    // Same-identity continuation is launcher-authored metadata.
    push_continuity_environment(&mut argv);

    // The human-readable app name from `[package].name`; omitted when unset, and the toolkit derives
    // one from the package id.
    push_app_metadata(&mut argv, manifest, &payload_mount);

    // Hardened allocator + shadow stacks for packages the image lists as handling untrusted bytes —
    // decided against an image-owned policy, never something the manifest can ask for.
    push_runtime_hardening(
        &mut argv,
        &crate::hardening::for_package(&manifest.package_id),
    );

    // Standard mounts, before the system profile so its device entries land on top of the `--dev`
    // tmpfs instead of under it.
    argv.push("--tmpfs".to_string());
    argv.push("/tmp".to_string());
    argv.push("--proc".to_string());
    argv.push("/proc".to_string());
    argv.push("--dev".to_string());
    argv.push("/dev".to_string());
    // A script run by a runtime's interpreter gets the little of procfs it reads (measured), and
    // nothing sensitive beside it: the kernel's sockets, tunables, addresses and keys are masked
    // out of its sandbox. No other app reads procfs at all.
    if manifest.entrypoint.script.is_some() {
        push_proc_masks(&mut argv);
    }

    // Bind the runtime and its canonical ELF interpreter paths before the broad read-only `/lib` and
    // `/lib64` profile mounts below.
    push_runtime_bundle(&mut argv, runtime);

    // The system read profile: fixed, identical for every package, fed by no manifest field.
    crate::profile::push_system_profile(
        &mut argv,
        crate::egress_proxy::egress_decision(manifest, capabilities, payload_root)
            .shares_host_network(),
    );

    // User-granted capabilities after the system profile (read-write wins over read-only).
    push_capability_binds(&mut argv, capabilities);

    // XDG portals: wire in the bind plan the caller supplied.
    if let Some(bind) = dbus_bind {
        push_bind(
            &mut argv,
            "--ro-bind",
            &bind.host_socket,
            &bind.sandbox_path,
        );
        argv.push("--setenv".to_string());
        argv.push("DBUS_SESSION_BUS_ADDRESS".to_string());
        argv.push(format!("unix:path={}", bind.sandbox_path.display()));
    }

    // Service sockets reach the app as inherited descriptors: dup2'd onto `child_fd` in `pre_exec`,
    // surviving exec at the same number the app reads from `TILO_SVC_<NAME>_FD`.
    for sb in service_binds {
        argv.push("--setenv".to_string());
        argv.push(svc_env_var(&sb.name));
        argv.push(sb.child_fd.to_string());
    }

    let entry_abs = entry_root_for(manifest).join(entry_bin);
    argv.push(entry_abs.display().to_string());

    argv
}

/// The parts of its own procfs a script run by a runtime's interpreter reads. Measured on bun 1.4.5
/// and `CPython` 3.13 running a hello-world and then this repository's script probe: bun opens its
/// own `maps`, `statm`, `cgroup` and `fd/`, and aborts before running a script without `maps`;
/// `CPython` reads none of it. Neither reads `cpuinfo`, `meminfo`, `status`, `stat`, `cmdline`,
/// `auxv`, `exe` or `mountinfo`. The sandbox's procfs is a fresh one in its private PID namespace,
/// so these are the app's own process alone.
pub const PROC_SCRIPT_READ_PATHS: &[&str] = &[
    // Its own address map: bun aborts before it runs a script without it.
    "/proc/self/maps",
    // Its own memory, cgroup and descriptors, which bun reads as it starts; CPython reads none.
    "/proc/self/statm",
    "/proc/self/cgroup",
    "/proc/self/fd",
];

/// Sensitive procfs entries masked out of a script runtime's sandbox: an empty tmpfs over the
/// directory entries, `/dev/null` over the single files. A runtime needs none of them, and they
/// describe the machine and its other processes rather than the app.
// The directory entries a procfs lets a sandbox mask with an empty tmpfs. `/proc/net` is a
// symlink to the app's own `/proc/self/net`, and procfs refuses a mount over a symlink or a new
// entry inside it, so single files such as `/proc/kcore` and `/proc/kallsyms` are refused by the
// rules instead: no rule names them, and Landlock refuses every path it does not grant.
const PROC_MASKED_DIRS: &[&str] = &["/proc/sys", "/proc/fs", "/proc/irq", "/proc/bus"];
/// Mask procfs for a script runtime: the measurements above are the whole of what it reads, and
/// everything else sensitive becomes an empty tmpfs directory or `/dev/null`.
fn push_proc_masks(argv: &mut Vec<String>) {
    for directory in PROC_MASKED_DIRS {
        argv.push("--tmpfs".to_string());
        argv.push((*directory).to_string());
    }
}

/// Where the egress proxy socket is on each side of the sandbox boundary, and the loopback port the
/// in-sandbox relay serves.
#[derive(Debug, Clone)]
pub struct EgressProxyBind {
    pub host_socket: PathBuf,
    pub sandbox_socket: PathBuf,
    /// Loopback port the relay listens on inside the sandbox.
    pub port: u16,
    /// The sandbox end of the call's datagram pair, handed down while this bind lives and never
    /// after: a launch without calls carries none.
    pub udp_client: Option<std::sync::Arc<tilo_socket::Datagram>>,
}

/// Wire the egress proxy into the sandbox: socket + env vars for an ordinary HTTP client
/// ([`crate::egress_proxy::proxy_env`]), pointed at the in-sandbox relay.
pub fn push_egress_proxy(argv: &mut Vec<String>, bind: &EgressProxyBind) {
    push_bind(argv, "--bind", &bind.host_socket, &bind.sandbox_socket);
    if let Some(client) = &bind.udp_client {
        use std::os::fd::AsRawFd as _;
        let fd = client.as_raw_fd();
        // bwrap passes unknown descriptors through; the shim is told the number where its
        // arguments are spliced in (`--net-udp-fd`), forks the relay with it, and closes its own
        // copy before exec, so the app never holds it.
        // SAFETY: fcntl reads and writes this descriptor's own flags.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags != -1 {
                let _ = libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
        }
    }
    for (var, value) in crate::egress_proxy::proxy_env(bind.port) {
        argv.push("--setenv".to_string());
        argv.push(var.to_string());
        argv.push(value);
    }
    // Convenience for an app that speaks to the check directly instead of via the loopback relay.
    argv.push("--setenv".to_string());
    argv.push("TILO_EGRESS_PROXY".to_string());
    argv.push(bind.sandbox_socket.display().to_string());
}

/// One service-socket triple resolved by the launcher.
#[derive(Debug, Clone)]
pub struct ServiceBind {
    /// Service name as the manifest spells it.
    pub name: String,
    /// Host-side fd we keep open across exec for bwrap to read from.
    pub host_fd: i32,
    /// Child-side fd number (the slot the app will see).
    pub child_fd: i32,
}

/// The env var name the entrypoint reads to find an opened service fd.
pub use runtime_portal::session_services::fd_env as svc_env_var;

/// Render a [`CapabilityPlan`]'s paths as bwrap binds.
pub fn push_capability_binds(argv: &mut Vec<String>, capabilities: &CapabilityPlan) {
    push_grant_binds(argv, capabilities, |_| true);
    push_private_store_masks(argv, capabilities);
}

/// The grant binds `keep` selects, read-only ones first.
fn push_grant_binds(
    argv: &mut Vec<String>,
    capabilities: &CapabilityPlan,
    keep: impl Fn(&Path) -> bool,
) {
    for path in capabilities.ro_paths.iter().filter(|p| keep(p)) {
        push_bind(argv, "--ro-bind-try", path, path);
    }
    for path in capabilities.rw_paths.iter().filter(|p| keep(p)) {
        let flag = if path.starts_with("/dev") {
            "--dev-bind-try"
        } else {
            "--bind-try"
        };
        push_bind(argv, flag, path, path);
    }
}

/// Hide the private stores a grant contains — after every grant, or a later bind would show them again
/// (bwrap applies its operations in order).
fn push_private_store_masks(argv: &mut Vec<String>, capabilities: &CapabilityPlan) {
    for dir in &capabilities.pinned_dirs {
        push_bind(argv, "--bind", dir, dir);
    }
    push_grant_binds(argv, capabilities, |path| {
        capabilities
            .pinned_dirs
            .iter()
            .any(|dir| path != dir && path.starts_with(dir))
    });
    for dir in &capabilities.masked_dirs {
        argv.push("--tmpfs".to_string());
        argv.push(dir.display().to_string());
        argv.push("--remount-ro".to_string());
        argv.push(dir.display().to_string());
    }
}

/// Tell the app where the **user's** home is, when a capability binds it — inside the sandbox `HOME` is
/// the app's own per-workspace directory, wrong for an app whose job *is* the user's files.
pub fn push_user_home(argv: &mut Vec<String>, capabilities: &CapabilityPlan) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| !h.as_os_str().is_empty())?;
    let bound = capabilities
        .ro_paths
        .iter()
        .chain(capabilities.rw_paths.iter())
        .any(|granted| home.starts_with(granted));
    if !bound {
        return None;
    }
    argv.push("--setenv".to_string());
    argv.push("TILO_USER_HOME".to_string());
    argv.push(home.display().to_string());
    Some(home)
}

/// Name the user's data directory only when an existing capability binds it.
pub fn push_user_data(argv: &mut Vec<String>, capabilities: &CapabilityPlan) -> Option<PathBuf> {
    let data = tilo_paths::data_dir().filter(|path| path.is_absolute())?;
    if !capabilities
        .ro_paths
        .iter()
        .chain(capabilities.rw_paths.iter())
        .any(|granted| data.starts_with(granted))
    {
        return None;
    }
    argv.extend([
        "--setenv".to_owned(),
        "TILO_USER_DATA_HOME".to_owned(),
        data.display().to_string(),
    ]);
    Some(data)
}

/// Pass on the session's workspace roots this app's capability plan has bound.
pub fn push_workspace_roots(
    argv: &mut Vec<String>,
    capabilities: &CapabilityPlan,
) -> Option<String> {
    let raw = std::env::var("TILO_WORKSPACE_ROOTS").ok()?;
    let value = granted_workspace_roots(&raw, capabilities)?;
    argv.push("--setenv".to_string());
    argv.push("TILO_WORKSPACE_ROOTS".to_string());
    argv.push(value.clone());
    Some(value)
}

/// The roots of `raw` that `capabilities` has actually bound.
fn granted_workspace_roots(raw: &str, capabilities: &CapabilityPlan) -> Option<String> {
    let kept: Vec<&str> = raw
        .split(':')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter(|root| {
            let root = Path::new(root);
            capabilities
                .ro_paths
                .iter()
                .chain(capabilities.rw_paths.iter())
                .any(|granted| root.starts_with(granted))
        })
        .collect();
    if kept.is_empty() {
        return None;
    }
    Some(kept.join(":"))
}

fn push_bind(argv: &mut Vec<String>, kind: &str, src: &Path, dst: &Path) {
    argv.push(kind.to_string());
    argv.push(src.display().to_string());
    argv.push(dst.display().to_string());
}

/// Where a sandbox given an X11 display finds the display's cookie.
pub const X11_AUTHORITY_MOUNT: &str = "/tmp/.X11-authority";

/// Give the sandbox Bubblewrap's `args` set up the X11 display `display` (`:N`), whose cookie is
/// the file `authority`: its socket and its cookie bound read-only into the sandbox's private
/// `/tmp`, `DISPLAY` and `XAUTHORITY` naming them, and the Landlock rules that let the program
/// reach both.
///
/// The display is known only once the program's keeper picked it, after the arguments were
/// planned; nothing else of the sandbox changes, and nothing else of the session is reachable.
///
/// # Errors
/// `display` is not a local display `:N`, `authority` is not an absolute path, or `args` are not
/// a sandbox with a private `/tmp` behind the Landlock shim.
pub fn give_x11_display(
    args: &mut Vec<std::ffi::OsString>,
    display: &str,
    authority: &Path,
) -> Result<(), String> {
    let number: u32 = display
        .strip_prefix(':')
        .and_then(|number| number.parse().ok())
        .ok_or_else(|| format!("{display:?} is not a local X11 display"))?;
    if !authority.is_absolute() {
        return Err(format!(
            "the cookie {} is not an absolute path",
            authority.display()
        ));
    }
    let socket = format!("/tmp/.X11-unix/X{number}");
    // Where the mounts go: right after the private /tmp; and the rules: right after the shim.
    let tmp = args
        .windows(2)
        .position(|pair| matches!(pair, [flag, path] if flag == "--tmpfs" && path == "/tmp"))
        .and_then(|at| at.checked_add(2))
        .ok_or("the sandbox has no private /tmp to bind the display's socket in")?;
    let shim = args
        .iter()
        .position(|arg| arg == crate::landlock_shim::SANDBOX_EXEC_SUBCOMMAND)
        .filter(|shim| *shim >= tmp)
        .and_then(|at| at.checked_add(1))
        .ok_or("the sandbox has no Landlock shim")?;
    let rules: [std::ffi::OsString; 4] = [
        "--ro".into(),
        X11_AUTHORITY_MOUNT.into(),
        "--rw".into(),
        socket.clone().into(),
    ];
    args.splice(shim..shim, rules);
    let mounts: [std::ffi::OsString; 12] = [
        "--ro-bind".into(),
        socket.clone().into(),
        socket.into(),
        "--ro-bind".into(),
        authority.into(),
        X11_AUTHORITY_MOUNT.into(),
        "--setenv".into(),
        "DISPLAY".into(),
        format!(":{number}").into(),
        "--setenv".into(),
        "XAUTHORITY".into(),
        X11_AUTHORITY_MOUNT.into(),
    ];
    args.splice(tmp..tmp, mounts);
    Ok(())
}
