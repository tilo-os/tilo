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

/// Host-governed human identity directory exposed to Files.
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
/// Only the selected app/person policy directory is visible to the child.
pub const SANDBOX_ACTION_AUTHORITY_DIR: &str = "/run/tilo/action-authority";

pub fn push_action_authority_bind(argv: &mut Vec<String>, host_path: &Path) {
    push_bind(
        argv,
        "--ro-bind",
        host_path.parent().unwrap_or(host_path),
        Path::new(SANDBOX_ACTION_AUTHORITY_DIR),
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

    // Participation: tell the app which shape it is in.
    if let Some(mode) = capabilities.participation.env_value() {
        argv.push("--setenv".to_string());
        argv.push("TILO_PARTICIPATION".to_string());
        argv.push(mode.to_string());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Entrypoint;
    use crate::manifest::PortalPolicy;
    use std::collections::BTreeMap;
    use tilo_test_env::SpawnUnderEnvLock as _;

    fn mk_manifest(net_mode: &str) -> ManifestView {
        ManifestView {
            package_id: "com.example.notes".to_string(),
            name: Some("Notes".to_string()),
            localized_names: BTreeMap::default(),
            keywords: Vec::new(),
            category: Some("Office".to_string()),
            icon: None,
            version: "0.1.0".to_string(),
            kind: crate::manifest::KIND_APP.to_string(),
            runtime: None,
            required_runtimes: Vec::new(),
            entrypoint: Entrypoint {
                bin: "bin/notes".to_string(),
                r#type: crate::manifest::EntrypointType::Gui,
                gui_bin: None,
                cli_bin: None,
                runtime: None,
                script: None,
                origins: Vec::new(),
            },
            sandbox: crate::manifest::Sandbox {
                net: crate::manifest::Net {
                    mode: net_mode.to_string(),
                    hosts: Vec::new(),
                    calls: false,
                },
                services: BTreeMap::new(),
                portals: BTreeMap::new(),
                ..Default::default()
            },
            cli_commands: Vec::new(),
            opens_ext: Vec::new(),
            opens_mime: Vec::new(),
            opens_schemes: Vec::new(),
            opens_secondary: false,
            continuity: crate::manifest::ContinuityContract::default(),
            capabilities: Vec::new(),
            dev: None,
        }
    }

    /// Build argv for `mk_manifest`'s package id with `policy`, exercising the real `build_bwrap_argv`
    /// → `hardening::for_package` path.
    fn argv_with_hardening_policy(policy: &str) -> Vec<String> {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("untrusted-parsers.conf");
        std::fs::write(&file, policy).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(crate::hardening::POLICY_PATH_ENV, &file);
        plain_argv(&mk_manifest("off"))
    }

    /// The argv a launch of `m` gets with no runtime, services, bus or grants.
    fn plain_argv(m: &ManifestView) -> Vec<String> {
        build_bwrap_argv(
            m,
            Path::new("/host/pkg"),
            None,
            &[],
            None,
            &CapabilityPlan::empty(),
        )
    }

    /// The item a window opens on, as a launch names it.
    const REVEAL: &str = "chat.workspace/messages/m1";

    /// Value of `--setenv <key> <value>` for `key`, or `None` if absent.
    fn setenv_value<'a>(argv: &'a [String], key: &str) -> Option<&'a str> {
        argv.windows(3)
            .find(|w| w[0] == "--setenv" && w[1] == key)
            .map(|w| w[2].as_str())
    }

    tilo_test_env::scenario! {
        fn the_selected_runtime_is_bound_read_only_where_the_app_looks;

        phase selected_runtime_fonts_are_private_readonly_and_follow_the_selected_sdk;
        {
            let directory = tempfile::tempdir().unwrap();
            for version in ["held", "new"] {
                let root = directory.path().join(version);
                let fonts = root.join("lib/fonts");
                std::fs::create_dir_all(&fonts).unwrap();
                std::fs::write(fonts.join("manifest.toml"), version).unwrap();
                let mut argv = vec!["--unshare-all".to_string()];
                push_runtime_bundle(&mut argv, Some((&root, version)));
                crate::profile::push_system_profile(&mut argv, false);
                // Execute the production mount plan without starting an app or compositor.
                argv.extend([
                    "--ro-bind".into(),
                    "/usr/bin/cat".into(),
                    "/usr/bin/cat".into(),
                    "/usr/bin/cat".into(),
                    format!("{RUNTIME_FONT_CATALOG_PATH}/manifest.toml"),
                ]);
                let output = std::process::Command::new("bwrap")
                    .args(&argv)
                    .output_locked()
                    .expect("execute actual private runtime mount");
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(output.stdout, version.as_bytes());
                let mut write_argv = argv[..argv.len() - 2].to_vec();
                write_argv.extend([
                    "--ro-bind".into(),
                    "/usr/bin/touch".into(),
                    "/usr/bin/touch".into(),
                    "/usr/bin/touch".into(),
                    format!("{RUNTIME_FONT_CATALOG_PATH}/new-file"),
                ]);
                let write = std::process::Command::new("bwrap")
                    .args(write_argv)
                    .output_locked()
                    .unwrap();
                assert!(!write.status.success(), "SDK fonts must remain read-only");
                assert!(!fonts.join("new-file").exists());
                assert_eq!(
                    std::fs::read(fonts.join("manifest.toml")).unwrap(),
                    version.as_bytes()
                );
            }
            let mut legacy = Vec::new();
            push_runtime_bundle(&mut legacy, Some((directory.path(), "legacy")));
            assert!(!legacy.iter().any(|arg| arg == RUNTIME_FONT_CATALOG_PATH));
        }

        phase runtime_gstreamer_plugin_paths_are_fixed_and_already_readable;
        {
            let runtime = tempfile::tempdir().unwrap();
            std::fs::create_dir(runtime.path().join("lib")).unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("GST_PLUGIN_SYSTEM_PATH_1_0", "/home/private/plugins");
            let manifest = mk_manifest("off");
            let argv_for = || {
                build_bwrap_argv(
                    &manifest,
                    Path::new("/host/pkg"),
                    Some((runtime.path(), "selected")),
                    &[],
                    None,
                    &CapabilityPlan::empty())
            };
            assert_eq!(
                setenv_value(&argv_for(), "GST_PLUGIN_SYSTEM_PATH_1_0"),
                None
            );
            std::fs::write(runtime.path().join("lib/libgstreamer-1.0.so.0"), b"fixture").unwrap();
            let argv = argv_for();
            let paths = setenv_value(&argv, "GST_PLUGIN_SYSTEM_PATH_1_0").unwrap();
            assert!(paths.starts_with("/run/tilo-runtime/selected/lib/gstreamer-1.0:"));
            assert!(!paths.contains("/home/private"));
            for path in SYSTEM_GSTREAMER_PLUGINS {
                assert!(paths.split(':').any(|candidate| candidate == *path));
                assert!(
                    crate::profile::SYSTEM_READ_PATHS
                        .iter()
                        .any(|parent| Path::new(path).starts_with(parent))
                );
            }
            assert!(!argv.iter().any(|arg| arg == "--share-net"));
        }

        phase procfs_is_masked_for_a_script_runtime_and_absent_for_any_other;
        {
            let masked = |argv: &[String], pair: [&str; 2]| {
                argv.windows(2)
                    .any(|w| w[0] == pair[0] && w[1] == pair[1])
            };
            let mut ordinary = mk_manifest("off");
            let plain = plain_argv(&ordinary);
            assert!(!masked(&plain, ["--tmpfs", "/proc/net"]));
            assert!(!masked(&plain, ["--tmpfs", "/proc/sys"]));
            assert!(!masked(&plain, ["--ro-bind", "/dev/null"]));

            ordinary.entrypoint.script = Some("main.js".to_string());
            let argv = plain_argv(&ordinary);
            for directory in ["/proc/sys", "/proc/fs", "/proc/irq", "/proc/bus"] {
                assert!(
                    masked(&argv, ["--tmpfs", directory]),
                    "{directory} is not masked: {argv:?}"
                );
            }
        }

        phase an_entrypoint_runtime_execs_the_runtime_binary_over_the_payload;
        {
            // A tilolang app: its payload is the compiled module; the binary is the tilolang runtime
            // bundle's, told where the module is.
            let mut m = mk_manifest("off");
            m.required_runtimes = vec!["tilolang/0.1.0".to_string()];
            m.entrypoint.bin = "bin/tilolang".to_string();
            m.entrypoint.runtime = Some("tilolang/0.1.0".to_string());
            let argv = plain_argv(&m);
            assert_eq!(
                argv.last().map(String::as_str),
                Some("/run/tilo-runtime/0.1.0/bin/tilolang")
            );
            assert_eq!(
                setenv_value(&argv, APP_MODULE_ENV),
                Some("/var/tilo-apps/com.example.notes")
            );
            assert_eq!(
                setenv_value(&argv, "TILO_PACKAGE_ID"),
                Some("com.example.notes")
            );
            // An ordinary app runs its own binary and hears of no module.
            let argv = plain_argv(&mk_manifest("off"));
            assert_eq!(
                argv.last().map(String::as_str),
                Some("/var/tilo-apps/com.example.notes/bin/notes")
            );
            assert_eq!(setenv_value(&argv, APP_MODULE_ENV), None);
        }
    }

    tilo_test_env::scenario! {
        fn the_sandbox_argv_carries_only_validated_layout_and_metadata;

        phase hardening_policy_decides_allocator_and_shadow_stacks;
        {
            {
                let _env = tilo_test_env::lock();
                let argv = argv_with_hardening_policy("com.example.notes\n");

                let preload = argv
                    .windows(3)
                    .find(|w| w[0] == "--setenv" && w[1] == "LD_PRELOAD")
                    .map(|w| w[2].clone());

                // compiler-rt may be absent at test time; fall back to the system allocator rather than a
                // preload that can't resolve.
                match preload {
                    Some(path) => {
                        assert!(
                            path.ends_with("libclang_rt.scudo_standalone.so"),
                            "unexpected preload: {path}"
                        );
                        assert!(
                            argv.windows(3)
                                .any(|w| w[0] == "--ro-bind" && w[1] == path && w[2] == path),
                            "LD_PRELOAD is set to {path} but that path is not bound into \
                         the sandbox, so the loader would silently skip it: {argv:?}"
                        );
                    }
                    None => assert!(
                        crate::hardening::find_hardened_allocator().is_none(),
                        "an allocator is installed but no LD_PRELOAD was emitted"
                    ),
                }

                assert!(
                    argv.windows(3).any(|w| w[0] == "--setenv"
                        && w[1] == "GLIBC_TUNABLES"
                        && w[2] == crate::hardening::tunables(true)),
                    "shadow stacks were not requested for a listed package: {argv:?}"
                );
            }
            {
                let _env = tilo_test_env::lock();
                let argv = argv_with_hardening_policy("so.tilo.browser\n");
                assert!(
                    !argv.iter().any(|a| a == "LD_PRELOAD"),
                    "an ordinary app must not carry the hardened allocator: {argv:?}"
                );
                assert!(
                    !argv.iter().any(|a| a.contains("SHSTK")),
                    "an ordinary app must not carry the shadow-stack tunable: {argv:?}"
                );
                assert!(
                    argv.windows(3).any(|w| w[0] == "--setenv"
                        && w[1] == "GLIBC_TUNABLES"
                        && w[2] == crate::hardening::NO_HUGE_PAGES_TUNABLES),
                    "an app's heap must stay out of transparent huge pages: {argv:?}"
                );
            }
        }

        phase argv_layout_covers_namespaces_payload_runtime_profile_and_entrypoint;
        {
            {
                // A plain app: its own namespaces, its name, no icon, no bus, and its own binary
                // last. What it binds is `baseline_binds`, asserted as an equality in
                // `workspace_capabilities_decide_filesystem_and_camera_binds`.
                let argv = plain_argv(&mk_manifest("off"));
                assert!(argv.iter().any(|a| a == "--unshare-all"));
                assert!(argv.iter().any(|a| a == "--die-with-parent"));
                assert!(!argv.iter().any(|a| a == "--share-net"));
                assert!(argv.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == "/tmp"));
                assert!(argv.windows(2).any(|w| w[0] == "--proc" && w[1] == "/proc"));
                assert!(argv.windows(2).any(|w| w[0] == "--dev" && w[1] == "/dev"));
                assert_eq!(setenv_value(&argv, "TILO_APP_NAME"), Some("Notes"));
                // No icon art: no TILO_APP_ICON, so the toolkit draws its default.
                assert_eq!(setenv_value(&argv, "TILO_APP_ICON"), None);
                assert!(
                    !argv.iter().any(|a| a == "DBUS_SESSION_BUS_ADDRESS"),
                    "bus must not leak into sandbox without a plan: {argv:?}"
                );
                assert_eq!(
                    argv.last().map(String::as_str),
                    Some("/var/tilo-apps/com.example.notes/bin/notes")
                );
            }
            {
                // `host` outside the image is the most the app may ask for: it shares the network
                // once a person granted any host, not before.
                let m = mk_manifest("host");
                let argv = |plan: &CapabilityPlan| {
                    build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, plan)
                };
                assert!(!argv(&CapabilityPlan::empty()).iter().any(|a| a == "--share-net"));
                let granted = CapabilityPlan {
                    allow_network: true,
                    net_hosts: vec!["*".to_owned()],
                    ..CapabilityPlan::empty()
                };
                assert!(argv(&granted).iter().any(|a| a == "--share-net"));
            }
            {
                let m = mk_manifest("off");
                let payload = tempfile::tempdir().unwrap();
                let named = |argv: &[String], key: &str| {
                    argv.windows(3)
                        .find(|w| w[0] == "--setenv" && w[1] == key)
                        .map(|w| w[2].clone())
                };
                let without = build_bwrap_argv(
                    &m,
                    payload.path(),
                    None,
                    &[],
                    None,
                    &CapabilityPlan::empty());
                assert_eq!(
                    named(&without, "TILO_SURFACE"),
                    None,
                    "a package without a surface names none"
                );
                std::fs::write(
                payload.path().join("surface.json"),
                b"{\"resources\":[{\"kind\":\"com.example.notes\",\"label\":\"Notes\",\"version\":1}]}",
            )
            .unwrap();
                let with = build_bwrap_argv(
                    &m,
                    payload.path(),
                    None,
                    &[],
                    None,
                    &CapabilityPlan::empty());
                assert_eq!(
                    named(&with, "TILO_SURFACE").as_deref(),
                    Some("/var/tilo-apps/com.example.notes/surface.json"),
                    "the file in the sandbox's payload"
                );
                assert_eq!(
                    named(&with, "TILO_SURFACE_HASH"),
                    Some(blake3::hash(b"[[resources]]\n").to_hex().to_string())
                );
            }
            {
                let m = mk_manifest("off");
                let argv = build_bwrap_argv(
                    &m,
                    Path::new("/host/pkg"),
                    Some((Path::new("/host/runtime"), "1")),
                    &[],
                    None,
                    &CapabilityPlan::empty());
                let rt_pos = argv
                    .windows(3)
                    .position(|w| {
                        w[0] == "--ro-bind" && w[1] == "/host/runtime" && w[2] == "/run/tilo-runtime/1"
                    })
                    .expect("runtime ro-bind");
                let usr_lib_pos = argv
                    .windows(3)
                    .position(|w| w[0] == "--ro-bind-try" && w[1] == "/usr/lib" && w[2] == "/usr/lib")
                    .expect("system /usr/lib profile bind");
                assert!(
                    rt_pos < usr_lib_pos,
                    "the selected runtime and its canonical loader children must be mounted before the broad read-only library profile: {argv:?}"
                );
                assert!(
                    !argv.iter().any(|arg| arg.ends_with("/libgcc_s.so.1")),
                    "ordinary shared libraries must resolve through LD_LIBRARY_PATH, not symlink-hostile file rebinds: {argv:?}"
                );
                assert!(
                    argv.windows(3).any(|w| w[0] == "--setenv"
                        && w[1] == "LD_LIBRARY_PATH"
                        && w[2] == "/run/tilo-runtime/1/lib"),
                    "LD_LIBRARY_PATH setenv: {argv:?}"
                );
            }
            {
                let m = mk_manifest("off");
                let argv = build_bwrap_argv_for_entry(
                    &m,
                    Path::new("/host/pkg"),
                    None,
                    &[],
                    None,
                    "bin/notes-cli",
                    &CapabilityPlan::empty());
                assert_eq!(
                    argv.last().map(String::as_str),
                    Some("/var/tilo-apps/com.example.notes/bin/notes-cli")
                );
            }
            {
                // bwrap reads everything after the entrypoint token as the child's argv — a `--setenv`
                // spliced after it silently becomes a command-line arg instead of an env var.
                let m = mk_manifest("off");
                let mut argv = plain_argv(&m);
                let entry = argv.pop().expect("argv non-empty");
                push_workspace_id(&mut argv, "3f2b1c00-0000-4000-8000-000000000001");
                argv.push(entry);

                let setenv = argv
                    .windows(3)
                    .position(|w| w[0] == "--setenv" && w[1] == WORKSPACE_ID_ENV)
                    .unwrap_or_else(|| panic!("no {WORKSPACE_ID_ENV} setenv in {argv:?}"));
                assert_eq!(argv[setenv + 2], "3f2b1c00-0000-4000-8000-000000000001");
                assert!(
                    setenv + 3 < argv.len(),
                    "the setenv triple must come before the trailing entrypoint \
                 token, not after it: {argv:?}"
                );
            }
        }

        phase normal_sandbox_forwards_only_the_exact_presentation_observer_boolean;
        {
            for value in ["1", "0", "true", "1\n", "/private/log", ""] {
                let mut env = tilo_test_env::ScopedEnv::new();
                env.set("TILO_PROFILE_PRESENTATION", value)
                    .set("TILO_NATIVE_SCENARIO", "/private/input.json")
                    .set("TILO_NATIVE_INPUT_APPROVAL", "private-approval")
                    .set("TILO_PROFILE_OUT", "/private/log")
                    .set("VELLO_GPU_DISPATCH_TIMESTAMPS", value)
                    .set("VELLO_GPU_TIMESTAMPS", value)
                    .set("TILO_PROFILE_GPU_STAGES", value);
                let argv = plain_argv(&mk_manifest("off"));
                assert!(argv.iter().any(|arg| arg == "--clearenv"));
                assert_eq!(
                    setenv_value(&argv, "TILO_PROFILE_PRESENTATION"),
                    (value == "1").then_some("1")
                );
                for key in [
                    "VELLO_GPU_TIMESTAMPS",
                    "VELLO_GPU_DISPATCH_TIMESTAMPS",
                    "TILO_PROFILE_GPU_STAGES",
                ] {
                    assert_eq!(setenv_value(&argv, key), (value == "1").then_some("1"));
                }
                for key in [
                    "TILO_NATIVE_SCENARIO",
                    "TILO_NATIVE_INPUT_APPROVAL",
                    "TILO_PROFILE_OUT",
                ] {
                    assert_eq!(
                        setenv_value(&argv, key),
                        None,
                        "must not inject input or paths"
                    );
                }
            }
        }

        phase argv_exports_only_validated_launch_metadata;
        {
            {
                for (value, forwarded) in [
                    ("/data/out", true),
                    ("/data/../etc", false),
                    ("relative/out", false),
                    ("/data/out\n", false),
                ] {
                    let mut argv = Vec::new();
                    let mut env = tilo_test_env::ScopedEnv::new();
                    env.set(SAVE_DIRECTORY_ENV, value);
                    push_continuity_environment(&mut argv);
                    assert_eq!(
                        argv.windows(3)
                            .any(|w| w[0] == "--setenv" && w[1] == SAVE_DIRECTORY_ENV),
                        forwarded,
                        "{value:?}"
                    );
                }
            }
            {
                let mut env = tilo_test_env::ScopedEnv::new();
                env.set("TILO_CONTINUITY_INSTANCE", "42")
                    .set("TILO_CONTINUITY_RESUME_SCHEMA", "7")
                    .set("TILO_CONTINUITY_RESUME_HEX", "7b7d")
                    .set(tilo_process::environment::REVEAL_ENV, REVEAL);
                let argv = plain_argv(&mk_manifest("off"));
                drop(env);
                assert_eq!(setenv_value(&argv, "TILO_CONTINUITY_INSTANCE"), Some("42"));
                assert_eq!(
                    setenv_value(&argv, "TILO_CONTINUITY_RESUME_SCHEMA"),
                    Some("7")
                );
                assert_eq!(
                    setenv_value(&argv, "TILO_CONTINUITY_RESUME_HEX"),
                    Some("7b7d")
                );
                // A window started on another member's item opens on it.
                assert_eq!(
                    setenv_value(&argv, tilo_process::environment::REVEAL_ENV),
                    Some(REVEAL)
                );
            }
            {
                let mut env = tilo_test_env::ScopedEnv::new();
                env.set(
                    "TILO_STARTUP_METRICS_PATH",
                    "/run/tilo/app/.local/state/startup-42-dead_beef.json",
                );
                let argv = plain_argv(&mk_manifest("off"));
                assert_eq!(
                    setenv_value(&argv, "TILO_STARTUP_METRICS_PATH"),
                    Some("/run/tilo/app/.local/state/startup-42-dead_beef.json")
                );

                // The app's home itself is what Files lists; a trace there is refused.
                env.set(
                    "TILO_STARTUP_METRICS_PATH",
                    "/run/tilo/app/startup-42-dead_beef.json",
                );
                let argv = plain_argv(&mk_manifest("off"));
                assert_eq!(setenv_value(&argv, "TILO_STARTUP_METRICS_PATH"), None);

                env.set("TILO_STARTUP_METRICS_PATH", "/home/user/metrics.json");
                let argv = plain_argv(&mk_manifest("off"));
                drop(env);
                assert_eq!(setenv_value(&argv, "TILO_STARTUP_METRICS_PATH"), None);
            }
            {
                // A manifest with no `[package].name` exports nothing.
                let mut m = mk_manifest("off");
                m.name = None;
                let argv = plain_argv(&m);
                assert_eq!(setenv_value(&argv, "TILO_APP_NAME"), None);
            }
            {
                // Icon art named payload-relative; builder resolves it to the in-sandbox payload mount.
                let mut m = mk_manifest("off");
                m.icon = Some("assets/icon.png".to_string());
                let argv = plain_argv(&m);
                assert_eq!(
                    setenv_value(&argv, "TILO_APP_ICON"),
                    Some("/var/tilo-apps/com.example.notes/assets/icon.png"),
                );
            }
        }

        phase trusted_identity_projection_is_bound_read_only;
        {
            let mut argv = Vec::new();
            push_trusted_identities_bind(
                &mut argv,
                Path::new("/run/user/1000/tilo/identity-projections/workspace/trusted.json"),
            );
            assert_eq!(
                argv,
                vec![
                    "--ro-bind",
                    "/run/user/1000/tilo/identity-projections/workspace",
                    SANDBOX_TRUSTED_IDENTITIES_DIR,
                ]
            );
        }
    }

    tilo_test_env::scenario! {
        fn buses_and_service_sockets_reach_the_sandbox_only_as_planned;

        phase wayland_bind_exposes_only_the_normalised_public_socket;
        {
            {
                // Host is on wayland-4; the app must still see wayland-0.
                let bind = wayland_bind_for(Path::new("/run/user/1000/wayland-4"), 1000);
                assert_eq!(bind.display_name, "wayland-0");
                assert_eq!(bind.sandbox_socket, Path::new("/run/user/1000/wayland-0"));
                assert_eq!(bind.sandbox_runtime_dir, Path::new("/run/user/1000"));
                assert_eq!(bind.host_socket, Path::new("/run/user/1000/wayland-4"));
            }
            {
                let mut argv = Vec::new();
                let bind = wayland_bind_for(Path::new("/run/user/1000/wayland-4"), 1000);
                push_wayland_bind(&mut argv, &bind);

                let bound: Vec<&String> = argv
                    .windows(3)
                    .filter(|w| w[0] == "--ro-bind" || w[0] == "--bind")
                    .map(|w| &w[1])
                    .collect();
                assert_eq!(
                    bound,
                    vec!["/run/user/1000/wayland-4"],
                    "exactly one Wayland socket may be bound into a sandbox, and it must \
                 be the public one: {argv:?}"
                );
                assert!(
                    !argv.iter().any(|a| a.contains("tilo-privileged")),
                    "the privileged Wayland socket must never appear in a sandbox argv: {argv:?}"
                );
                assert!(
                    argv.windows(2)
                        .any(|w| w[0] == "--unsetenv" && w[1] == PRIVILEGED_WAYLAND_ENV),
                    "the privileged socket's env var must be cleared inside the sandbox: {argv:?}"
                );
            }
            {
                let mut argv = Vec::new();
                let bind = wayland_bind_for(Path::new("/run/user/1000/wayland-4"), 1000);
                push_wayland_bind(&mut argv, &bind);
                let pos = argv
                    .iter()
                    .position(|a| a == "/run/user/1000/wayland-4")
                    .expect("host socket in argv");
                assert_eq!(argv[pos - 1], "--ro-bind");
                assert_eq!(argv[pos + 1], "/run/user/1000/wayland-0");
                assert!(
                    argv.windows(3).any(|w| w[0] == "--setenv"
                        && w[1] == "WAYLAND_DISPLAY"
                        && w[2] == "wayland-0"),
                    "WAYLAND_DISPLAY setenv missing: {argv:?}"
                );
                assert!(
                    argv.windows(3).any(|w| w[0] == "--setenv"
                        && w[1] == "XDG_RUNTIME_DIR"
                        && w[2] == "/run/user/1000"),
                    "XDG_RUNTIME_DIR setenv missing: {argv:?}"
                );
            }
        }

        phase a_media_app_may_own_the_player_name_its_toolkit_requests;
        {
            // The app learns who it is from the `TILO_PACKAGE_ID` the sandbox sets; the toolkit names its
            // player from that identity.
            let mut m = mk_manifest("off");
            m.capabilities = vec![crate::dbus_proxy::MEDIA_SESSION_CAPABILITY.to_string()];
            let argv = plain_argv(&m);
            let app = tilo_process::environment::ProcessIdentity {
                app_id: setenv_value(&argv, "TILO_PACKAGE_ID").map(str::to_owned),
                ..Default::default()
            };
            let requested = app.media_player_bus_name("notes");
            assert_eq!(requested, "org.mpris.MediaPlayer2.com.example.notes");
            let granted = crate::dbus_proxy::media_player_for(&m);
            assert_eq!(granted.as_deref(), Some(requested.as_str()));
            assert!(
                crate::dbus_proxy::session_filter_args(&BTreeMap::new(), true, granted.as_deref())
                    .contains(&format!("--own={requested}"))
            );
            // Without the declaration there is no player to own.
            assert_eq!(
                crate::dbus_proxy::media_player_for(&mk_manifest("off")),
                None
            );
        }

        phase a_graphical_app_reaches_no_accessibility_bus;
        {
            let m = mk_manifest("off");
            let plan = DBusBindPlan {
                host_socket: PathBuf::from("/run/user/1234/tilo/dbus-proxy/9999.sock"),
                sandbox_path: PathBuf::from("/run/user/1234/bus"),
            };
            let argv = build_bwrap_argv(
                &m,
                Path::new("/host/pkg"),
                None,
                &[],
                Some(&plan),
                &CapabilityPlan::empty(),
            );
            // The session publishes the window's tree: the app holds no accessibility bus.
            assert!(
                !argv.iter().any(|arg| {
                    let arg = arg.to_ascii_lowercase();
                    arg.contains("atspi") || arg.contains("at-spi")
                }),
                "no accessibility bus or its read-only switch reaches a sandbox: {argv:?}"
            );
        }

        phase session_bus_reaches_the_sandbox_only_through_a_proxy_plan;
        {
            {
                let mut m = mk_manifest("off");
                m.sandbox
                    .portals
                    .insert("file-chooser".to_string(), PortalPolicy::ViaHost);
                let plan = DBusBindPlan {
                    host_socket: PathBuf::from("/run/user/1234/tilo/dbus-proxy/9999.sock"),
                    sandbox_path: PathBuf::from("/run/user/1234/bus"),
                };
                let argv = build_bwrap_argv(
                    &m,
                    Path::new("/host/pkg"),
                    None,
                    &[],
                    Some(&plan),
                    &CapabilityPlan::empty());
                // Proxy socket bound at /run/user/1234/bus, NOT the raw bus path.
                assert!(
                    argv.windows(3).any(|w| w[0] == "--ro-bind"
                        && w[1] == "/run/user/1234/tilo/dbus-proxy/9999.sock"
                        && w[2] == "/run/user/1234/bus"),
                    "proxy socket ro-bind missing from argv: {argv:?}"
                );
                // The sandbox sees the bus at /run/user/<uid>/bus, so the env var reflects that.
                assert!(
                    argv.windows(3).any(|w| w[0] == "--setenv"
                        && w[1] == "DBUS_SESSION_BUS_ADDRESS"
                        && w[2] == "unix:path=/run/user/1234/bus"),
                    "dbus setenv missing from argv: {argv:?}"
                );
                assert!(
                    !argv.iter().any(|a| a == "AT_SPI_BUS_ADDRESS"),
                    "no accessibility bus without its proxy: {argv:?}"
                );
            }
            {
                // `Deny`-only portals means the caller passes `None`, the same as no portals.
                let mut m = mk_manifest("off");
                m.sandbox
                    .portals
                    .insert("screencast".to_string(), PortalPolicy::Deny);
                let argv = plain_argv(&m);
                assert!(
                    !argv.iter().any(|a| a == "DBUS_SESSION_BUS_ADDRESS"),
                    "bus must stay out when caller passes no plan: {argv:?}"
                );
            }
        }

        phase service_sockets_bind_as_child_fds_only;
        {
            {
                let m = mk_manifest("off");
                let binds = vec![ServiceBind {
                    name: "collab".to_string(),
                    host_fd: 7,
                    child_fd: 17,
                }];
                let argv = build_bwrap_argv(
                    &m,
                    Path::new("/host/pkg"),
                    None,
                    &binds,
                    None,
                    &CapabilityPlan::empty());
                assert!(
                    argv.windows(3)
                        .any(|w| w[0] == "--setenv" && w[1] == "TILO_SVC_COLLAB_FD" && w[2] == "17"),
                    "the app must be told which descriptor its service is on: {argv:?}"
                );
            }
            {
                let m = mk_manifest("off");
                let binds = vec![
                    ServiceBind {
                        name: "collab".to_string(),
                        host_fd: 7,
                        child_fd: 17,
                    },
                    ServiceBind {
                        name: "fs".to_string(),
                        host_fd: 8,
                        child_fd: 18,
                    },
                ];
                let argv = build_bwrap_argv(
                    &m,
                    Path::new("/host/pkg"),
                    None,
                    &binds,
                    None,
                    &CapabilityPlan::empty());
                for copy_flag in ["--file", "--bind-data", "--ro-bind-data"] {
                    for w in argv.windows(2) {
                        assert_ne!(
                            (w[0].as_str(), w[1].as_str()),
                            (copy_flag, "7"),
                            "{copy_flag} on the collab socket fd makes bwrap block \
                         forever reading it: {argv:?}",
                        );
                        assert_ne!(
                            (w[0].as_str(), w[1].as_str()),
                            (copy_flag, "8"),
                            "{copy_flag} on the fs socket fd makes bwrap block \
                         forever reading it: {argv:?}",
                        );
                    }
                }
                // Nothing emits `--file` any more, so the statement is flat: not on these fds, ever.
                assert!(
                    !argv.iter().any(|a| a == "--file"),
                    "a service socket must never be handed to bwrap as a file: {argv:?}"
                );
                assert!(
                    !argv
                        .iter()
                        .any(|a| a.ends_with("/tilo/services/collab.sock")),
                    "the service socket has no in-sandbox path any more; the app \
                 gets a descriptor: {argv:?}",
                );
            }
        }
    }

    // Capability enforcement: assert on the production argv itself — a path missing from it is a path
    // the app cannot reach, whatever the UI says.

    use collab_types::caps::{Access, CapOverride, Scope};
    use tilo_action_policy::Grantee;

    /// Deterministic stand-in for the host's device nodes, so a camera assertion doesn't depend on the
    /// test machine owning one.
    fn fake_device_paths(device: collab_types::caps::DeviceCap) -> Vec<PathBuf> {
        match device {
            collab_types::caps::DeviceCap::Camera => vec![PathBuf::from("/dev/video0")],
            collab_types::caps::DeviceCap::Microphone => vec![PathBuf::from("/dev/snd")],
            _ => Vec::new(),
        }
    }

    /// Build the argv the launcher would really produce for `app_id`, through the same production path
    /// as `launch_with`; only the device-node lookup is substituted.
    fn argv_with_caps(app_id: &str, caps: &[CapOverride], now: u64) -> Vec<String> {
        let mut m = mk_manifest("host");
        m.capabilities.push("camera".to_owned());
        argv_declared(&m, app_id, caps, now)
    }

    /// [`argv_with_caps`] for an app whose manifest is `m`.
    fn argv_declared(
        m: &ManifestView,
        app_id: &str,
        caps: &[CapOverride],
        now: u64,
    ) -> Vec<String> {
        let caps = crate::capabilities::declared(m, caps.to_vec());
        let plan = CapabilityPlan::for_app_with(app_id, &caps, Some(now), fake_device_paths);
        build_bwrap_argv(m, Path::new("/host/pkg"), None, &[], None, &plan)
    }

    /// Every bind triple in an argv, as `(flag, src, dst)`.
    fn bind_triples(argv: &[String]) -> Vec<(String, String, String)> {
        argv.windows(3)
            .filter(|w| {
                matches!(
                    w[0].as_str(),
                    "--bind"
                        | "--bind-try"
                        | "--ro-bind"
                        | "--ro-bind-try"
                        | "--dev-bind"
                        | "--dev-bind-try"
                )
            })
            .map(|w| (w[0].clone(), w[1].clone(), w[2].clone()))
            .collect()
    }

    fn fs_grant(app: &str, path: &str, verb: Access) -> CapOverride {
        CapOverride::grant(
            Grantee::person("ada"),
            Scope::FsPath(path.to_string()),
            verb,
        )
        .through(app)
    }

    fn app_grant(app: &str, scope: Scope, access: Access) -> CapOverride {
        CapOverride::grant(Grantee::person("ada"), scope, access).through(app)
    }

    fn app_restrict(app: &str, scope: Scope, access: Access) -> CapOverride {
        CapOverride::new(
            Grantee::person("ada"),
            scope,
            collab_types::caps::Effect::Restrict,
            access,
        )
        .through(app)
    }

    /// Everything a launch with no grants binds, as an equality (not `any()` probes) so a new mount
    /// can't sneak into the default sandbox.
    fn baseline_binds() -> Vec<(String, String, String)> {
        let mut expected = vec![(
            "--ro-bind".to_string(),
            "/host/pkg".to_string(),
            "/var/tilo-apps/com.example.notes".to_string(),
        )];
        for p in crate::profile::SYSTEM_READ_PATHS {
            expected.push((
                "--ro-bind-try".to_string(),
                (*p).to_string(),
                (*p).to_string(),
            ));
        }
        // No GPU node: an app without `gpu` draws through its workspace's render service.
        expected
    }

    tilo_test_env::scenario! {
        fn filesystem_grants_decide_what_crosses_into_the_sandbox;

        phase workspace_roots_cross_only_where_granted;
        {
            {
                let plan = CapabilityPlan::default();
                assert_eq!(granted_workspace_roots("/var/workspace", &plan), None);
            }
            {
                let plan = CapabilityPlan {
                    ro_paths: vec![PathBuf::from("/var/workspace")],
                    ..CapabilityPlan::default()
                };
                assert_eq!(
                    granted_workspace_roots("/var/workspace", &plan).as_deref(),
                    Some("/var/workspace"),
                );
            }
            {
                let plan = CapabilityPlan {
                    ro_paths: vec![PathBuf::from("/var/workspace")],
                    rw_paths: vec![PathBuf::from("/var/tmp/tilo/workspace")],
                    ..CapabilityPlan::default()
                };
                assert_eq!(
                    granted_workspace_roots(
                        "/var/workspace:/srv/somebody-elses:/var/tmp/tilo/workspace",
                        &plan,
                    )
                    .as_deref(),
                    Some("/var/workspace:/var/tmp/tilo/workspace"),
                    "a root nobody granted must be dropped while the granted ones stay",
                );
            }
            {
                let plan = CapabilityPlan {
                    ro_paths: vec![PathBuf::from("/var/workspace")],
                    ..CapabilityPlan::default()
                };
                assert_eq!(granted_workspace_roots("/var/workspace-notes", &plan), None);
            }
        }

        phase user_data_hint_requires_an_existing_filesystem_grant;
        {
            let home = tilo_test_env::TestHome::new().unwrap();
            let data = home.data().to_path_buf();
            let mut argv = Vec::new();
            assert_eq!(push_user_data(&mut argv, &CapabilityPlan::default()), None);
            assert!(
                argv.is_empty(),
                "naming Trash must not mint a bind or grant"
            );
            let sibling = CapabilityPlan {
                rw_paths: vec![data.with_file_name("share-other")],
                ..Default::default()
            };
            assert_eq!(push_user_data(&mut argv, &sibling), None);
            assert!(argv.is_empty());
            for plan in [
                CapabilityPlan {
                    ro_paths: vec![data.clone()],
                    ..Default::default()
                },
                CapabilityPlan {
                    rw_paths: vec![home.home().to_path_buf()],
                    ..Default::default()
                },
            ] {
                let mut argv = Vec::new();
                assert_eq!(push_user_data(&mut argv, &plan), Some(data.clone()));
                assert_eq!(
                    argv,
                    vec!["--setenv", "TILO_USER_DATA_HOME", data.to_str().unwrap()]
                );
            }
        }

        phase private_stores_are_masked_after_every_grant_bind;
        // The argv masks the private store after every grant: a later bind would show it again.
        {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join("home");
            let store = home.join(".config/tilo");
            let notes = home.join(".config/notes");
            std::fs::create_dir_all(&store).unwrap();
            std::fs::create_dir_all(&notes).unwrap();
            // The home is writable; `notes` inside it is capped at Read, so it has its own read-only mount
            // the pin must not shadow.
            let caps = vec![
                fs_grant("so.tilo.files", &home.display().to_string(), Access::Write),
                fs_grant("so.tilo.files", &notes.display().to_string(), Access::Read),
                app_restrict(
                    "so.tilo.files",
                    Scope::FsPath(notes.display().to_string()),
                    Access::Read,
                ),
            ];
            let plan = CapabilityPlan::for_app_with("so.tilo.files", &caps, Some(1), fake_device_paths)
                .confine_private_stores(std::slice::from_ref(&store));
            let argv = build_bwrap_argv(
                &mk_manifest("off"),
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                &plan);
            let s = |p: &Path| p.display().to_string();
            let position = |pattern: &[&str]| {
                argv.windows(pattern.len())
                    .position(|w| w.iter().zip(pattern).all(|(a, b)| a == b))
                    .unwrap_or_else(|| panic!("{pattern:?} missing from {argv:?}"))
            };
            let config = home.join(".config");
            let home_bind = position(&["--bind-try", &s(&home), &s(&home)]);
            let pin = position(&["--bind", &s(&config), &s(&config)]);
            let tmpfs = position(&["--tmpfs", &s(&store), "--remount-ro", &s(&store)]);
            assert!(home_bind < pin && pin < tmpfs, "{argv:?}");
            let last_notes_bind = argv
                .windows(3)
                .rposition(|w| w[0] == "--ro-bind-try" && w[1] == s(&notes) && w[2] == s(&notes))
                .unwrap();
            assert!(
                pin < last_notes_bind && last_notes_bind < tmpfs,
                "the read-only grant under the pin must be bound again after it: {argv:?}"
            );
            assert!(
                !argv[tmpfs + 4..]
                    .windows(3)
                    .any(|w| w[0].contains("bind") && Path::new(&w[2]).starts_with(&home)),
                "nothing may be bound over the home after the store is masked: {argv:?}"
            );
        }

        phase workspace_capabilities_decide_filesystem_and_camera_binds;
        {
            {
                let _env = tilo_test_env::lock();
                let baseline = argv_with_caps("com.example.notes", &[], 1_000);
                let m = mk_manifest("off");
                let manifest_only = plain_argv(&m);
                assert_eq!(
                    baseline, manifest_only,
                    "an app granted nothing must get byte-identical argv to a manifest-only launch"
                );
                assert_eq!(
                    bind_triples(&baseline),
                    baseline_binds(),
                    "a capability-less launch binds the payload and the system profile, \
                 and nothing else: {baseline:?}"
                );
            }
            {
                let caps = vec![fs_grant(
                    "so.tilo.books",
                    "/var/workspace/books",
                    Access::Read,
                )];
                let argv = argv_with_caps("so.tilo.books", &caps, 1_000);
                assert!(
                    argv.windows(3).any(|w| w[0] == "--ro-bind-try"
                        && w[1] == "/var/workspace/books"
                        && w[2] == "/var/workspace/books"),
                    "a Read capability must produce a read-only bind of exactly that path: {argv:?}"
                );
                for (flag, src, _dst) in bind_triples(&argv) {
                    if src == "/var/workspace/books" {
                        assert!(
                            flag == "--ro-bind" || flag == "--ro-bind-try",
                            "a Read capability must never produce the writable bind `{flag}`: {argv:?}"
                        );
                    }
                }
                // The grant is for that path and nothing above it: the parent directory must not be
                // reachable.
                assert!(
                    !argv.iter().any(|a| a == "/var/workspace"),
                    "a grant on /var/workspace/books must not bind its parent: {argv:?}"
                );
            }
            {
                let caps = vec![fs_grant(
                    "so.tilo.books",
                    "/var/workspace/books",
                    Access::Write,
                )];
                let argv = argv_with_caps("so.tilo.books", &caps, 1_000);
                assert!(
                    argv.windows(3).any(|w| w[0] == "--bind-try"
                        && w[1] == "/var/workspace/books"
                        && w[2] == "/var/workspace/books"),
                    "a Write capability must produce a read-write bind: {argv:?}"
                );
                for (flag, src, _dst) in bind_triples(&argv) {
                    if src == "/var/workspace/books" {
                        assert!(
                            flag == "--bind" || flag == "--bind-try",
                            "a Write capability must not also emit the read-only bind `{flag}`: {argv:?}"
                        );
                    }
                }
            }
            {
                let without = argv_with_caps("so.tilo.meet", &[], 1_000);
                assert!(
                    !without.iter().any(|a| a.starts_with("/dev/video")),
                    "no camera capability must mean no camera device in the namespace: {without:?}"
                );

                let caps = vec![app_grant(
                    "so.tilo.meet",
                    Scope::Device(collab_types::caps::DeviceCap::Camera),
                    Access::Read,
                )];
                let with = argv_with_caps("so.tilo.meet", &caps, 1_000);
                assert!(
                    with.windows(3).any(|w| w[0] == "--dev-bind-try"
                        && w[1] == "/dev/video0"
                        && w[2] == "/dev/video0"),
                    "a camera capability must bind the device node: {with:?}"
                );
                let undeclared = argv_declared(&mk_manifest("host"), "so.tilo.meet", &caps, 1_000);
                assert!(
                    !undeclared.iter().any(|a| a.starts_with("/dev/video")),
                    "a camera the manifest does not declare is never bound: {undeclared:?}"
                );
            }
            {
                let caps = vec![
                    fs_grant("so.tilo.other", "/var/workspace/secrets", Access::Write),
                    app_grant(
                        "so.tilo.other",
                        Scope::Device(collab_types::caps::DeviceCap::Camera),
                        Access::Read,
                    ),
                    app_grant(
                        "so.tilo.other",
                        Scope::NetHost("*".to_string()),
                        Access::Write,
                    ),
                ];
                let argv = argv_with_caps("so.tilo.books", &caps, 1_000);
                assert!(
                    !argv.iter().any(|a| a == "/var/workspace/secrets"),
                    "another app's filesystem grant must not appear here: {argv:?}"
                );
                assert!(
                    !argv.iter().any(|a| a.starts_with("/dev/video")),
                    "another app's camera grant must not appear here: {argv:?}"
                );
                assert!(
                    !argv.iter().any(|a| a == "--share-net"),
                    "another app's network grant must not open this app's namespace: {argv:?}"
                );
                // Proof the fixture is capable of granting: the same list, read as the app it names, does
                // produce those binds.
                let theirs = argv_with_caps("so.tilo.other", &caps, 1_000);
                assert!(theirs.iter().any(|a| a == "/var/workspace/secrets"));
                assert!(theirs.iter().any(|a| a == "--share-net"));
            }
            {
                let _env = tilo_test_env::lock();
                let caps = vec![
                    fs_grant("so.tilo.books", "/var/workspace/books", Access::Write)
                        .lasting(tilo_action_policy::Lifetime::Until(500_000)),
                ];
                let expired = argv_with_caps("so.tilo.books", &caps, 1_000);
                assert!(
                    !expired.iter().any(|a| a == "/var/workspace/books"),
                    "a capability that expired at 500 must bind nothing at now=1000: {expired:?}"
                );
                assert_eq!(
                    bind_triples(&expired),
                    baseline_binds(),
                    "only the payload and the system profile survive an expired grant: {expired:?}"
                );
                // Same capability before its deadline does bind — so the emptiness above is the expiry,
                // not a broken fixture.
                let live = argv_with_caps("so.tilo.books", &caps, 499);
                assert!(
                    live.windows(3).any(|w| w[0] == "--bind-try"
                        && w[1] == "/var/workspace/books"
                        && w[2] == "/var/workspace/books"),
                    "the same capability evaluated before its deadline must bind: {live:?}"
                );
            }
        }
    }

    tilo_test_env::scenario! {
        fn devices_and_the_network_are_bound_as_selected_and_granted;

        phase the_session_pipeline_seed_is_bound_read_only;
        {
            let sel = crate::gpu::GpuSelection {
                node: PathBuf::from("/dev/dri/renderD128"),
                driver: Some("msm_dpu".to_string()),
                egl_vendor_json: None,
                vulkan_icd_json: None,
                vulkan_device: None,
                pipeline_seed: Some(PathBuf::from("/run/user/1000/tilo/pipeline-seed")),
                site_integration: None,
                unclaimed: false,
                notes: Vec::new(),
            };
            let mut argv = Vec::new();
            push_gpu_selection(&mut argv, &sel, true);
            assert!(
                argv.windows(3).any(|w| w[0] == "--ro-bind-try"
                    && w[1] == "/run/user/1000/tilo/pipeline-seed"
                    && w[2] == crate::gpu::SANDBOX_PIPELINE_SEED_DIR),
                "{argv:?}"
            );
            assert!(
                !argv
                    .windows(2)
                    .any(|w| w[0].starts_with("--bind") && w[1] == "/run/user/1000/tilo/pipeline-seed"),
                "an app must never be able to write the seed: {argv:?}"
            );
            assert!(
                argv.windows(3).any(|w| w[0] == "--setenv"
                    && w[1] == crate::gpu::PIPELINE_SEED_ENV
                    && w[2] == crate::gpu::SANDBOX_PIPELINE_SEED_DIR),
                "{argv:?}"
            );
        }

        phase gpu_selection_and_drm_grants_shape_device_binds;
        {
            {
                let sel = crate::gpu::GpuSelection {
                    node: PathBuf::from("/dev/dri/renderD128"),
                    driver: Some("amdgpu".to_string()),
                    egl_vendor_json: Some(PathBuf::from("/usr/share/glvnd/egl_vendor.d/50_mesa.json")),
                    vulkan_icd_json: Some(PathBuf::from(
                        "/usr/share/vulkan/icd.d/radeon_icd.x86_64.json",
                    )),
                    vulkan_device: None,
                    pipeline_seed: None,
                    site_integration: None,
                    unclaimed: false,
                    notes: Vec::new(),
                };
                let mut argv = Vec::new();
                push_gpu_selection(&mut argv, &sel, true);

                // The tmpfs must come before the node re-bind, or the bind is the thing that gets hidden.
                let tmpfs = argv
                    .windows(2)
                    .position(|w| w[0] == "--tmpfs" && w[1] == "/dev/dri")
                    .unwrap_or_else(|| panic!("device dir must be emptied: {argv:?}"));
                let node = argv
                    .windows(3)
                    .position(|w| {
                        w[0] == "--dev-bind-try"
                            && w[1] == "/dev/dri/renderD128"
                            && w[2] == "/dev/dri/renderD128"
                    })
                    .expect("the chosen node must be bound back");
                assert!(
                    tmpfs < node,
                    "the tmpfs must precede the node bind: {argv:?}"
                );
                assert!(
                    !argv.iter().any(|a| a == "/dev/dri/renderD129"),
                    "no other GPU may be re-exposed: {argv:?}"
                );

                for (var, file) in [
                    (
                        "__EGL_VENDOR_LIBRARY_FILENAMES",
                        "/usr/share/glvnd/egl_vendor.d/50_mesa.json",
                    ),
                    (
                        "VK_DRIVER_FILES",
                        "/usr/share/vulkan/icd.d/radeon_icd.x86_64.json",
                    ),
                    (
                        "VK_ICD_FILENAMES",
                        "/usr/share/vulkan/icd.d/radeon_icd.x86_64.json",
                    ),
                ] {
                    assert!(
                        argv.windows(3)
                            .any(|w| w[0] == "--setenv" && w[1] == var && w[2] == file),
                        "{var} setenv missing: {argv:?}"
                    );
                    assert!(
                        argv.windows(3)
                            .any(|w| w[0] == "--ro-bind-try" && w[1] == file && w[2] == file),
                        "{var} points at {file}, which is never bound into the sandbox: {argv:?}"
                    );
                }
            }
            {
                let sel = crate::gpu::GpuSelection {
                    node: PathBuf::from("/dev/dri/renderD128"),
                    driver: Some("amdgpu".to_string()),
                    egl_vendor_json: Some(PathBuf::from("/usr/share/glvnd/egl_vendor.d/50_mesa.json")),
                    vulkan_icd_json: None,
                    vulkan_device: None,
                    pipeline_seed: None,
                    site_integration: None,
                    unclaimed: false,
                    notes: Vec::new(),
                };
                let mut argv = Vec::new();
                push_gpu_selection(&mut argv, &sel, false);
                assert!(
                    !argv.iter().any(|a| a.starts_with("/dev/dri")),
                    "no device path may appear for an app with no DRM grant: {argv:?}"
                );
                // An underived loader stays underived — no empty variable.
                assert!(
                    !argv.iter().any(|a| a == "VK_DRIVER_FILES"),
                    "an unmatched Vulkan ICD must not be pinned to nothing: {argv:?}"
                );
                assert!(
                    argv.iter().any(|a| a == "__EGL_VENDOR_LIBRARY_FILENAMES"),
                    "the loader that did resolve must still be pinned: {argv:?}"
                );
            }
            {
                let none = CapabilityPlan::empty();
                let off = mk_manifest("off");
                assert!(
                    !grants_drm(crate::gpu::granted(&off, &none, true), &none),
                    "an app without `gpu` has no /dev/dri at all"
                );
                let mut gpu = mk_manifest("off");
                gpu.capabilities.push("gpu".to_owned());
                assert!(
                    grants_drm(crate::gpu::granted(&gpu, &none, true), &none),
                    "an app the image ships declaring `gpu` gets the device, narrowed"
                );
                assert!(
                    !crate::gpu::granted(&gpu, &none, false),
                    "an app the person installed declaring `gpu` waits for their grant"
                );
                let mut person = CapabilityPlan::empty();
                person.devices.push(crate::gpu::CAPABILITY);
                assert!(crate::gpu::granted(&gpu, &person, false));
                assert!(
                    !crate::gpu::granted(&off, &person, false),
                    "a grant does not reach past the manifest"
                );

                let mut granted = CapabilityPlan::empty();
                granted.rw_paths.push(PathBuf::from("/dev/dri/renderD129"));
                assert!(
                    grants_drm(false, &granted),
                    "a user-granted node must be narrowed too"
                );
            }
        }

        phase network_capabilities_decide_namespace_and_egress_proxy;
        {
            // An app outside the image that may ask for any host (`host`) reaches what its grants
            // name, and nothing before a person granted it.
            let asks_anything = mk_manifest("host");
            let argv = |caps: &[CapOverride]| argv_with_caps("so.tilo.mail", caps, 1_000);
            {
                let isolated = argv(&[]);
                assert!(
                    !isolated.iter().any(|a| a == "--share-net"),
                    "no network grant must leave the netns unshared: {isolated:?}"
                );
                let caps = [app_grant(
                    "so.tilo.mail",
                    Scope::NetHost("*".to_string()),
                    Access::Write,
                )];
                let granted = argv(&caps);
                assert!(
                    granted.iter().any(|a| a == "--share-net"),
                    "a grant for `*` is unrestricted access and shares the namespace: {granted:?}"
                );
            }
            {
                let caps = [app_grant(
                    "so.tilo.mail",
                    Scope::NetHost("github.com".to_string()),
                    Access::Write,
                )];
                let argv = argv(&caps);
                assert!(
                    !argv.iter().any(|a| a == "--share-net"),
                    "a grant naming one host must NOT hand the app the host's whole network: {argv:?}"
                );

                // The check decision comes from the same manifest and capability pair that built
                // the argv above.
                let plan =
                    CapabilityPlan::for_app_with("so.tilo.mail", &caps, Some(1_000), |_| Vec::new());
                let decision = crate::egress_proxy::egress_decision(&asks_anything, &plan, Path::new("/host/pkg"));
                let policy = decision.policy().unwrap_or_else(|| {
                    panic!("a named host must resolve to a filtered egress policy, got {decision:?}")
                });
                assert!(policy.permits("github.com"));
                assert!(
                    !policy.permits("evil.example.com"),
                    "the allowlist must exclude what it does not name: {policy:?}"
                );
                // A manifest that asks for no network is not given any by a grant.
                assert_eq!(
                    crate::egress_proxy::egress_decision(&mk_manifest("off"), &plan, Path::new("/host/pkg")),
                    crate::egress_proxy::EgressDecision::Blocked
                );
            }
            {
                let bind = EgressProxyBind {
                    host_socket: PathBuf::from("/run/user/1000/tilo/net/7.sock"),
                    sandbox_socket: PathBuf::from(crate::egress_proxy::SANDBOX_PROXY_PATH),
                    port: crate::egress_proxy::RELAY_PORT,
                    udp_client: None,
                };
                let mut argv = Vec::new();
                push_egress_proxy(&mut argv, &bind);

                assert!(
                    argv.windows(3).any(|w| w[0] == "--bind"
                        && w[1] == "/run/user/1000/tilo/net/7.sock"
                        && w[2] == crate::egress_proxy::SANDBOX_PROXY_PATH),
                    "the check socket must be bound read-write: connect(2) on a pathname socket is a \
                 write and a --ro-bind surfaces as EACCES: {argv:?}"
                );
                // socks5h, not socks5: the `h` sends the hostname to the check instead of resolving it
                // locally, which the sandbox cannot do.
                let socks = format!("socks5h://127.0.0.1:{}", crate::egress_proxy::RELAY_PORT);
                for var in ["ALL_PROXY", "all_proxy"] {
                    assert!(
                        argv.windows(3)
                            .any(|w| w[0] == "--setenv" && w[1] == var && w[2] == socks),
                        "{var} must point at the relay as socks5h: {argv:?}"
                    );
                }
                // And the HTTP form works without an opt-in feature — an app whose HTTP library lacks
                // SOCKS support reads only these.
                let http = format!("http://127.0.0.1:{}", crate::egress_proxy::RELAY_PORT);
                for var in ["HTTP_PROXY", "http_proxy", "HTTPS_PROXY", "https_proxy"] {
                    assert!(
                        argv.windows(3)
                            .any(|w| w[0] == "--setenv" && w[1] == var && w[2] == http),
                        "{var} must point at the relay as an HTTP proxy: {argv:?}"
                    );
                }
            }
        }
    }
}
