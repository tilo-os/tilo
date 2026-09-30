//! Bubblewrap argv construction from a [`MinimalManifest`].
//!
//! Pure function: given a manifest, payload root, optional runtime root,
//! and resolved service fds, returns the argv for `bwrap`. No I/O — fd
//! plumbing is `run()`'s job in `lib.rs`.

use std::path::{Path, PathBuf};

use crate::capabilities::CapabilityPlan;
use crate::manifest::{MinimalManifest, PortalPolicy};

/// In-sandbox mount root for the package payload.
pub fn payload_mount_for(package_id: &str) -> PathBuf {
    PathBuf::from("/var/tilo-apps").join(package_id)
}

/// In-sandbox mount root for the runtime (`tilo-runtime/<version>/`).
pub fn runtime_mount_for(version: &str) -> PathBuf {
    PathBuf::from("/run/tilo-runtime").join(version)
}

/// Canonical absolute ELF interpreter paths re-exposed from the runtime
/// bundle. Ordinary shared libraries are found through the bundle's
/// `LD_LIBRARY_PATH`; rebinding them here can target host symlinks (for
/// example `/lib64/libgcc_s.so.1`), which bubblewrap deliberately refuses.
pub const CANONICAL_LOADER_PATHS: &[&str] = &[
    "/lib64/ld-linux-x86-64.so.2",
    "/lib/ld-linux-x86-64.so.2",
    "/lib/ld-linux.so.2",
    "/usr/lib64/ld-linux-x86-64.so.2",
    "/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
    "/lib/ld-linux-aarch64.so.1",
    "/lib64/ld-linux-aarch64.so.1",
    "/usr/lib64/ld-linux-aarch64.so.1",
    "/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1",
];

/// Plan for exposing a filtered D-Bus proxy into the sandbox. The production
/// runner leaves D-Bus unavailable when that proxy cannot start; there is no
/// raw-session-bus plan or fallback.
#[derive(Debug, Clone)]
pub struct DBusBindPlan {
    pub host_socket: PathBuf,
    /// In-sandbox path, read via `DBUS_SESSION_BUS_ADDRESS=unix:path=<sandbox_path>`.
    pub sandbox_path: PathBuf,
}

/// Where a GUI app's Wayland socket appears inside the sandbox.
/// Normalised to `wayland-0` regardless of host display name — the app
/// has no business knowing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaylandBind {
    pub host_socket: PathBuf,
    pub sandbox_socket: PathBuf,
    /// In-sandbox `XDG_RUNTIME_DIR`.
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

/// Splice a Wayland display into an argv built by [`build_bwrap_argv`],
/// before the trailing entrypoint token.
///
/// `--ro-bind`, matching Flatpak: blocks the app from
/// unlinking/replacing the compositor's socket while still allowing
/// `connect(2)`.
///
/// Exactly **one** socket is bound — the public one. The compositor's
/// privileged socket carries the clipboard-manager protocols (which
/// hand their holder every selection with no focus check) and is never
/// bound or named here.
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

/// The `PATH` a sandboxed app runs with, fixed rather than inherited —
/// a writable dir on the session's PATH would decide which binary
/// `exec` finds.
pub const SANDBOX_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// The whole of what a sandboxed app inherits from the launching
/// session. An allowlist, not a filter: nothing here names a socket,
/// room, identity or path — those reach the app only via an explicit
/// `--setenv` beside the bind that makes them reachable.
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
];

/// User-owned type-only accessibility preferences allowed into applications.
/// Values are parsed and canonicalised before crossing the sandbox boundary;
/// keeping these out of [`INHERITED_ENV`] prevents arbitrary strings from
/// becoming app input merely because their variable name is recognised.
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
///
/// `bwrap` passes the parent's environment straight through unless
/// told not to, and the parent here is the compositor — its Wayland
/// socket, broker and `TILO_*` confinement variables. So the
/// environment is *built*: `--clearenv` first, then `PATH`, then
/// [`INHERITED_ENV`], with everything else set beside the bind that
/// makes it mean something.
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
    push_accessibility_environment(argv);
}

/// Env var publishing the compositor's privileged Wayland socket.
/// Mirrors `shell::compositor::native::PRIVILEGED_SOCKET_ENV`,
/// duplicated since the launcher has no compositor dep.
pub const PRIVILEGED_WAYLAND_ENV: &str = "TILO_WAYLAND_PRIVILEGED";

/// Env var naming the workspace an app launched into —
/// `ui::collaboration` uses it as the **collaboration cluster id** CRDT
/// deltas are addressed to, so apps in different workspaces never
/// share traffic.
pub const WORKSPACE_ID_ENV: &str = "TILO_WORKSPACE_ID";

/// Attach [`crate::hardening::RuntimeHardening`] to a sandbox: the
/// preload library must also be bound, since an unbound `LD_PRELOAD`
/// path just falls back to the ordinary allocator with a stderr
/// warning, not an error.
///
/// Assumes the preload is linked against the host's libc, true for a
/// runtime bundle only because it's baked from the same rootfs in the
/// same image build.
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
        // Marking the binary isn't enough — glibc also needs this tunable for
        // a shadow stack request.
        argv.push("--setenv".to_string());
        argv.push("GLIBC_TUNABLES".to_string());
        argv.push("glibc.cpu.hwcaps=SHSTK".to_string());
    }
}

pub fn push_workspace_id(argv: &mut Vec<String>, workspace_id: &str) {
    argv.push("--setenv".to_string());
    argv.push(WORKSPACE_ID_ENV.to_string());
    argv.push(workspace_id.to_string());
}

/// The DRM device directory: the system profile binds it (every GPU on
/// a multi-GPU host); [`push_gpu_selection`] narrows to one.
const DRM_DIR: &str = "/dev/dri";

/// Where a shell-issued startup trace may land: the app's state directory,
/// never its home.
const STARTUP_METRICS_PREFIX: &str = "/run/tilo/app/.local/state/startup-";

/// Splice the compositor's GPU choice into the argv, before the
/// entrypoint token. Three things: (1) a tmpfs over `/dev/dri` hides
/// the manifest's grant, then only the chosen render node is bound
/// back — the only way to stop Mesa/EGL enumeration picking a GPU the
/// compositor can't import buffers from; (2) loader manifests stay
/// readable; (3) variables are set explicitly so the choice is
/// assertable.
///
/// `grants_drm` says whether the manifest gave this app `/dev/dri` at all.
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
}

/// Whether this launch exposes `/dev/dri` at all — consults both
/// sources rather than a constant, since [`push_gpu_selection`]'s
/// narrowing needs something actually mounted regardless of a future
/// profile change.
pub fn grants_drm(_manifest: &MinimalManifest, capabilities: &CapabilityPlan) -> bool {
    let from_profile = crate::profile::SYSTEM_DEVICE_PATHS
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

/// Host-governed human identity directory exposed to Files.  Unlike the
/// app's writable data directory, this individual mount is read-only.
pub const SANDBOX_TRUSTED_IDENTITIES_DIR: &str = "/run/tilo/identity-projection";
/// Stable JSON path Files reloads inside that directory.
pub const SANDBOX_TRUSTED_IDENTITIES_PATH: &str =
    "/run/tilo/identity-projection/trusted-identities.json";

/// Bind one already-staged identity projection into Files.
///
/// The caller restricts this to the Files package.  Keeping the bind helper
/// dumb makes its exact mount shape independently testable.
pub fn push_trusted_identities_bind(argv: &mut Vec<String>, host_path: &Path) {
    let host_directory = host_path.parent().unwrap_or(host_path);
    push_bind(
        argv,
        "--ro-bind",
        host_directory,
        Path::new(SANDBOX_TRUSTED_IDENTITIES_DIR),
    );
}
/// Build the bwrap argv from a manifest plus the launcher's resolved
/// inputs: `runtime_root`, `service_binds` (fds reach the child by
/// inheritance, not argv), `dbus_bind`, and whether an identity broker fd is
/// available (`identity_host_path` is a host-only presence marker).
///
/// `capabilities` only ever adds binds — [`CapabilityPlan::empty`]
/// renders exactly the sandbox the manifest alone describes, the whole
/// enforcement story for a revoked or never-granted permission.
pub fn build_bwrap_argv(
    manifest: &MinimalManifest,
    payload_root: &Path,
    runtime: Option<(&Path, &str)>,
    service_binds: &[ServiceBind],
    dbus_bind: Option<&DBusBindPlan>,
    identity_host_path: Option<&Path>,
    capabilities: &CapabilityPlan,
) -> Vec<String> {
    // GUI launch: pick the GUI-flavour bin, falling back to the legacy
    // top-level `bin`.
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
        identity_host_path,
        entry,
        capabilities,
    )
}

fn push_continuity_environment(argv: &mut Vec<String>) {
    for key in [
        "TILO_CONTINUITY_INSTANCE",
        "TILO_CONTINUITY_RESUME_SCHEMA",
        "TILO_CONTINUITY_RESUME_HEX",
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
            && (key != "TILO_CONTINUITY_RESUME_HEX"
                || (value.len() <= 64 * 1024 && value.bytes().all(|byte| byte.is_ascii_hexdigit())))
        {
            argv.extend(["--setenv".to_string(), key.to_string(), value]);
        }
    }
    // The shell owns this path and always points it at the app's state
    // directory inside its one writable bind — not the directory itself,
    // which is the app's home and what Files shows. Preserve it across
    // `--clearenv` so startup checkpoints survive a compositor crash or
    // forced reboot. Refuse arbitrary paths: this diagnostic must never turn
    // into a sandbox write primitive.
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
    // Where an unattended session's Save chooser answers. It only names a
    // destination: whether the app may write there is still its file grants'.
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

/// An absolute path of ordinary components, bounded and free of control
/// characters.
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

fn push_app_metadata(argv: &mut Vec<String>, manifest: &MinimalManifest, payload_mount: &Path) {
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

fn push_runtime_bundle(argv: &mut Vec<String>, runtime: Option<(&Path, &str)>) {
    let Some((runtime_root, runtime_version)) = runtime else {
        return;
    };
    let runtime_mount = runtime_mount_for(runtime_version);
    push_bind(argv, "--ro-bind", runtime_root, &runtime_mount);
    argv.extend([
        "--setenv".to_string(),
        "LD_LIBRARY_PATH".to_string(),
        runtime_mount.join("lib").display().to_string(),
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
    manifest: &MinimalManifest,
    payload_root: &Path,
    runtime: Option<(&Path, &str)>,
    service_binds: &[ServiceBind],
    dbus_bind: Option<&DBusBindPlan>,
    identity_host_path: Option<&Path>,
    entry_bin: &str,
    capabilities: &CapabilityPlan,
) -> Vec<String> {
    let mut argv: Vec<String> = Vec::new();

    // First, because `--clearenv` keeps only what a *later* `--setenv` puts
    // back — including the ones `run.rs` splices in after this returns.
    push_clean_environment(&mut argv);

    // Network namespace is shared ONLY for unrestricted access (host
    // network, or a `*` grant) — anything more specific keeps the empty
    // namespace and uses the egress gate, since `--share-net` is
    // all-or-nothing.
    argv.push("--unshare-all".to_string());
    if egress_decision(manifest, capabilities).shares_host_network() {
        argv.push("--share-net".to_string());
    }
    argv.push("--die-with-parent".to_string());

    let payload_mount = payload_mount_for(&manifest.package_id);
    push_bind(&mut argv, "--ro-bind", payload_root, &payload_mount);

    // TILO_PACKAGE_ID picks a per-app fallback state dir in standalone mode.
    argv.push("--setenv".to_string());
    argv.push("TILO_PACKAGE_ID".to_string());
    argv.push(manifest.package_id.clone());

    // Same-identity continuation is launcher-authored metadata. bwrap clears
    // the parent environment, so forward only the compact identifiers/payload
    // after rejecting control characters and an oversized resume blob.
    push_continuity_environment(&mut argv);

    // The human-readable app name from `[package].name`; omitted when
    // unset, and the toolkit derives one from the package id.
    push_app_metadata(&mut argv, manifest, &payload_mount);

    // Hardened allocator + shadow stacks for packages the image lists as
    // handling untrusted bytes — decided against an image-owned policy,
    // never something the manifest can ask for.
    push_runtime_hardening(
        &mut argv,
        &crate::hardening::for_package(&manifest.package_id),
    );

    // Standard mounts, before the system profile so its device entries
    // land on top of the `--dev` tmpfs instead of under it.
    argv.push("--tmpfs".to_string());
    argv.push("/tmp".to_string());
    argv.push("--proc".to_string());
    argv.push("/proc".to_string());
    argv.push("--dev".to_string());
    argv.push("/dev".to_string());

    // Bind the runtime and its canonical ELF interpreter paths before the
    // broad read-only `/lib` and `/lib64` profile mounts below. Bubblewrap
    // processes these actions in order: attempting to create
    // `/lib/ld-linux.so.2` beneath an already-mounted read-only `/lib` fails
    // before the app can exec. The later parent bind leaves the existing
    // child mount visible, while the selected bundle itself lives under the
    // sandbox-private `/run` tree.
    push_runtime_bundle(&mut argv, runtime);

    // The system read profile: fixed, identical for every package, fed by
    // no manifest field.
    crate::profile::push_system_profile(
        &mut argv,
        egress_decision(manifest, capabilities).shares_host_network(),
    );

    // User-granted capabilities after the system profile (read-write wins
    // over read-only). `-try` variants throughout: a stale grant (deleted
    // dir, unplugged webcam) must not fail the launch — bwrap just skips
    // it, never widens.
    push_capability_binds(&mut argv, capabilities);

    // XDG portals: wire in the bind plan the caller supplied. No plan
    // means the bus is NOT exposed at all.
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

    // Participation: tell the app which shape it is in. Information, not
    // enforcement — the enforcement is the identity about to not be bound.
    if let Some(mode) = capabilities.participation.env_value() {
        argv.push("--setenv".to_string());
        argv.push("TILO_PARTICIPATION".to_string());
        argv.push(mode.to_string());
    }

    // User identity: the path is a host-side presence marker only. Private
    // material is never mounted. run.rs puts one connected broker socket on
    // BROKER_CHILD_FD and the app receives only that least-privilege signing
    // capability.
    //
    // A **read-only participant gets no identity at all** — without a
    // signing key the app can't produce an authored change for anything
    // downstream to reject.
    if let Some(host_path) =
        identity_host_path.filter(|_| capabilities.participation.carries_identity())
    {
        let _ = host_path;
        argv.push("--setenv".to_string());
        argv.push(runtime_identity::broker::BROKER_ENV_VAR.to_string());
        argv.push(runtime_identity::broker::BROKER_CHILD_FD.to_string());
    }

    // Service sockets reach the app as inherited descriptors: dup2'd onto
    // `child_fd` in `pre_exec`, surviving exec at the same number the app
    // reads from `TILO_SVC_<NAME>_FD`.
    //
    // Deliberately **no bwrap flag**: `--file FD DEST` copies the fd's
    // *contents* to a file, so pointing it at a connected socket blocks
    // bwrap on an EOF that never comes, hanging the launch before `exec`.
    for sb in service_binds {
        argv.push("--setenv".to_string());
        argv.push(svc_env_var(&sb.name));
        argv.push(sb.child_fd.to_string());
    }

    let entry_abs = payload_mount.join(entry_bin);
    argv.push(entry_abs.display().to_string());

    argv
}

/// The outbound access this launch resolves to. One function so the
/// argv layer, Landlock layer and gate spawn can't disagree — the
/// alternative is an empty namespace with no gate (silently offline)
/// or a gate plus `--share-net` (a hole beside the filter).
pub fn egress_decision(
    manifest: &MinimalManifest,
    capabilities: &CapabilityPlan,
) -> crate::netgate::EgressDecision {
    let decision = crate::netgate::EgressDecision::resolve(
        &manifest.sandbox.net.mode,
        &manifest.sandbox.net.hosts,
        &capabilities.net_hosts,
    );
    // Applied here, not at the gate: the namespace decision and the gate
    // must come from the same answer, or the app gets an empty namespace
    // with nothing to reach through it.
    if matches!(decision, crate::netgate::EgressDecision::Filtered(_))
        && crate::dev_env::dev_override(crate::dev_env::NET_GATE_ENV).is_some_and(|v| v == "off")
    {
        tracing::warn!(
            hosts = ?decision.policy().map(super::netgate::EgressPolicy::hosts),
            "{}=off: launching with UNRESTRICTED outbound network access even though this app \
             was granted specific hosts. Per-host filtering is NOT enforced for this launch.",
            crate::run::NET_GATE_ENV,
        );
        return crate::netgate::EgressDecision::Unrestricted;
    }
    decision
}

/// Where the egress gate socket is on each side of the sandbox
/// boundary, and the loopback port the in-sandbox relay serves.
#[derive(Debug, Clone)]
pub struct NetGateBind {
    pub host_socket: PathBuf,
    pub sandbox_socket: PathBuf,
    /// Loopback port the relay listens on inside the sandbox.
    pub port: u16,
}

/// Wire the egress gate into the sandbox: socket + env vars for an
/// ordinary HTTP client ([`crate::netgate::proxy_env`]), pointed at the
/// in-sandbox relay.
///
/// `--bind`, not `--ro-bind`: `connect(2)` on a pathname socket needs
/// write access.
pub fn push_net_gate(argv: &mut Vec<String>, bind: &NetGateBind) {
    push_bind(argv, "--bind", &bind.host_socket, &bind.sandbox_socket);
    for (var, value) in crate::netgate::proxy_env(bind.port) {
        argv.push("--setenv".to_string());
        argv.push(var.to_string());
        argv.push(value);
    }
    // Convenience for an app that speaks to the gate directly instead of
    // via the loopback relay.
    argv.push("--setenv".to_string());
    argv.push("TILO_NET_GATE".to_string());
    argv.push(bind.sandbox_socket.display().to_string());
}

/// One service-socket triple resolved by the launcher.
#[derive(Debug, Clone)]
pub struct ServiceBind {
    /// Service name as it appeared in the manifest (e.g. "collab").
    pub name: String,
    /// Host-side fd we keep open across exec for bwrap to read from.
    pub host_fd: i32,
    /// Child-side fd number (the slot the app will see).
    pub child_fd: i32,
}

/// The env var name the entrypoint reads to find an opened service fd.
pub fn svc_env_var(name: &str) -> String {
    format!("TILO_SVC_{}_FD", name.to_uppercase())
}

/// Render a [`CapabilityPlan`]'s paths as bwrap binds. Read grants get
/// `--ro-bind-try` even for device nodes (a read cap conferring write
/// would be the model lying); write grants under `/dev` get
/// `--dev-bind-try`.
pub fn push_capability_binds(argv: &mut Vec<String>, capabilities: &CapabilityPlan) {
    for path in &capabilities.ro_paths {
        push_bind(argv, "--ro-bind-try", path, path);
    }
    for path in &capabilities.rw_paths {
        let flag = if path.starts_with("/dev") {
            "--dev-bind-try"
        } else {
            "--bind-try"
        };
        push_bind(argv, flag, path, path);
    }
}

/// Tell the app where the **user's** home is, when a capability binds
/// it — inside the sandbox `HOME` is the app's own per-room directory,
/// wrong for an app whose job *is* the user's files. Only set when the
/// capability plan has bound a path containing it, so the variable can
/// never name a directory the app can't read.
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

/// Pass on the session's workspace roots this app's capability plan
/// has bound. Order preserved — the first root is the one a file
/// manager cold-starts into.
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
/// `None` (not empty) when nothing survives — "no workspace" and "a
/// workspace with no roots" are different states.
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

/// If the manifest declared at least one portal as `ViaHost`, return the
/// current `$DBUS_SESSION_BUS_ADDRESS` — else `None`.
pub fn host_dbus_bus_address_if_needed(
    portals: &std::collections::BTreeMap<String, PortalPolicy>,
) -> Option<String> {
    let wants_bus = portals.values().any(|p| matches!(p, PortalPolicy::ViaHost));
    if !wants_bus {
        return None;
    }
    std::env::var("DBUS_SESSION_BUS_ADDRESS").ok()
}

/// Extract the `unix:path=<path>` from a D-Bus bus address, or `None`
/// otherwise. Abstract sockets can't be bind-mounted, so callers
/// surface that as "portal unavailable".
pub fn parse_unix_path_bus_address(addr: &str) -> Option<String> {
    for entry in addr.split(';') {
        let Some(rest) = entry.strip_prefix("unix:") else {
            continue;
        };
        for kv in rest.split(',') {
            if let Some(path) = kv.strip_prefix("path=")
                && !path.is_empty()
            {
                return Some(path.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Entrypoint;
    use std::collections::BTreeMap;

    #[test]
    fn workspace_roots_cross_only_where_granted() {
        // Phase `an_ungranted_workspace_root_is_not_named_to_the_app`.
        // Naming an ungranted root would be worse than useless: the app opens
        // on a directory that isn't there and reports an empty folder,
        // indistinguishable from a shared workspace with nothing in it.
        {
            let plan = CapabilityPlan::default();
            assert_eq!(granted_workspace_roots("/var/workspace", &plan), None);
        }
        // Phase `a_granted_workspace_root_reaches_the_app`.
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
        // Phase `only_the_granted_roots_cross`.
        // Narrowed per root, not all-or-nothing.
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
        // Phase `a_sibling_directory_is_not_covered_by_the_grant`.
        // Matching is by path component, not string prefix: a grant on
        // `/var/workspace` must not carry `/var/workspace-notes` across.
        {
            let plan = CapabilityPlan {
                ro_paths: vec![PathBuf::from("/var/workspace")],
                ..CapabilityPlan::default()
            };
            assert_eq!(granted_workspace_roots("/var/workspace-notes", &plan), None);
        }
    }

    fn mk_manifest(net_mode: &str) -> MinimalManifest {
        MinimalManifest {
            package_id: "com.example.notes".to_string(),
            name: Some("Notes".to_string()),
            localized_names: BTreeMap::default(),
            category: Some("Office".to_string()),
            icon: None,
            version: "0.1.0".to_string(),
            kind: crate::manifest::KIND_APP.to_string(),
            runtime: None,
            entrypoint: Entrypoint {
                bin: "bin/notes".to_string(),
                r#type: crate::manifest::EntrypointType::Gui,
                gui_bin: None,
                cli_bin: None,
            },
            sandbox: crate::manifest::Sandbox {
                net: crate::manifest::Net {
                    mode: net_mode.to_string(),
                    hosts: Vec::new(),
                },
                // A manifest names no path here — one that did would be refused before
                // reaching this builder; see `manifest::enforce_fs_policy`.
                fs: crate::manifest::Fs::default(),
                services: BTreeMap::new(),
                portals: BTreeMap::new(),
                ..Default::default()
            },
            cli_commands: Vec::new(),
            opens_ext: Vec::new(),
            opens_mime: Vec::new(),
            opens_schemes: Vec::new(),
            continuity: crate::manifest::ContinuityContract::default(),
        }
    }

    /// Build argv for `mk_manifest`'s package id with `policy`, exercising
    /// the real `build_bwrap_argv` → `hardening::for_package` path.
    fn argv_with_hardening_policy(policy: &str) -> Vec<String> {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("untrusted-parsers.conf");
        std::fs::write(&file, policy).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(crate::hardening::POLICY_PATH_ENV, &file);
        let m = mk_manifest("off");
        build_bwrap_argv(
            &m,
            Path::new("/host/pkg"),
            None,
            &[],
            None,
            None,
            &CapabilityPlan::empty(),
        )
    }

    #[test]
    fn hardening_policy_decides_allocator_and_shadow_stacks() {
        // Phase `a_listed_package_gets_the_allocator_bound_as_well_as_preloaded`.
        {
            // The argv builder reads the session environment.
            let _env = tilo_test_env::lock();
            let argv = argv_with_hardening_policy("com.example.notes\n");

            let preload = argv
                .windows(3)
                .find(|w| w[0] == "--setenv" && w[1] == "LD_PRELOAD")
                .map(|w| w[2].clone());

            // compiler-rt may be absent at test time; fall back to the system
            // allocator rather than a preload that can't resolve.
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
                    && w[2] == "glibc.cpu.hwcaps=SHSTK"),
                "shadow stacks were not requested for a listed package: {argv:?}"
            );
        }
        // Phase `an_unlisted_package_pays_for_neither_protection`.
        {
            // The argv builder reads the session environment.
            let _env = tilo_test_env::lock();
            let argv = argv_with_hardening_policy("so.tilo.browser\n");
            assert!(
                !argv.iter().any(|a| a == "LD_PRELOAD"),
                "an ordinary app must not carry the hardened allocator: {argv:?}"
            );
            assert!(
                !argv.iter().any(|a| a == "GLIBC_TUNABLES"),
                "an ordinary app must not carry the shadow-stack tunable: {argv:?}"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn argv_layout_covers_namespaces_payload_runtime_profile_and_entrypoint() {
        // Phase `argv_includes_namespace_and_lifetime_flags`.
        {
            let m = mk_manifest("off");
            let argv = build_bwrap_argv(
                &m,
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                None,
                &CapabilityPlan::empty(),
            );
            assert!(argv.iter().any(|a| a == "--unshare-all"));
            assert!(argv.iter().any(|a| a == "--die-with-parent"));
            assert!(!argv.iter().any(|a| a == "--share-net"));
            assert!(argv.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == "/tmp"));
            assert!(argv.windows(2).any(|w| w[0] == "--proc" && w[1] == "/proc"));
            assert!(argv.windows(2).any(|w| w[0] == "--dev" && w[1] == "/dev"));
        }
        // Phase `argv_shares_net_when_mode_not_off`.
        {
            let m = mk_manifest("on");
            let argv = build_bwrap_argv(
                &m,
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                None,
                &CapabilityPlan::empty(),
            );
            assert!(argv.iter().any(|a| a == "--share-net"));
        }
        // Phase `argv_binds_payload_root_to_tilo_apps_path`.
        {
            let m = mk_manifest("off");
            let argv = build_bwrap_argv(
                &m,
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                None,
                &CapabilityPlan::empty(),
            );
            let pos = argv
                .iter()
                .position(|a| a == "/host/pkg")
                .expect("payload host path in argv");
            assert_eq!(argv[pos - 1], "--ro-bind");
            assert_eq!(argv[pos + 1], "/var/tilo-apps/com.example.notes");
        }
        // Phase `argv_reads_come_from_the_system_profile_not_the_manifest`.
        // The reads a launch gets are the system profile's; a manifest naming
        // a path never reaches this builder.
        {
            // The argv builder reads the session environment.
            let _env = tilo_test_env::lock();
            let m = mk_manifest("off");
            let argv = build_bwrap_argv(
                &m,
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                None,
                &CapabilityPlan::empty(),
            );
            for entry in crate::profile::SYSTEM_READ_PATHS {
                assert!(
                    argv.windows(3)
                        .any(|w| w[0] == "--ro-bind-try" && w[1] == *entry && w[2] == *entry),
                    "profile entry {entry} missing from argv: {argv:?}"
                );
            }
            // A manifest that still carries `fs` fields changes nothing — the
            // builder doesn't read them.
            let mut hostile = mk_manifest("off");
            hostile.sandbox.fs.write.push("/".to_string());
            hostile.sandbox.fs.read.push("/home/alice/.ssh".to_string());
            let hostile_argv = build_bwrap_argv(
                &hostile,
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                None,
                &CapabilityPlan::empty(),
            );
            assert_eq!(
                argv, hostile_argv,
                "a manifest naming paths must produce byte-identical argv to one that names none"
            );
            // The payload mount stays a hard `--ro-bind`: a package whose
            // own payload is missing genuinely cannot run.
            assert!(
                argv.windows(3)
                    .any(|w| w[0] == "--ro-bind" && w[1] == "/host/pkg"),
                "payload bind must stay non-try: {argv:?}"
            );
            // Nothing writable comes out of this builder. The app's one writable
            // directory is spliced by `run`, from the room it was launched into.
            assert!(
                !argv.iter().any(|a| a == "--bind" || a == "--bind-try"),
                "the manifest layer must produce no writable bind: {argv:?}"
            );
        }
        // Phase `argv_includes_runtime_bind_and_ld_library_path`.
        {
            let m = mk_manifest("off");
            let argv = build_bwrap_argv(
                &m,
                Path::new("/host/pkg"),
                Some((Path::new("/host/runtime"), "1")),
                &[],
                None,
                None,
                &CapabilityPlan::empty(),
            );
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
            let ld_pos = argv
                .windows(3)
                .position(|w| {
                    w[0] == "--setenv"
                        && w[1] == "LD_LIBRARY_PATH"
                        && w[2] == "/run/tilo-runtime/1/lib"
                })
                .expect("LD_LIBRARY_PATH setenv");
            let _ = ld_pos;
        }
        // Phase `argv_ends_with_entrypoint_path`.
        {
            let m = mk_manifest("off");
            let argv = build_bwrap_argv(
                &m,
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                None,
                &CapabilityPlan::empty(),
            );
            assert_eq!(
                argv.last().map(String::as_str),
                Some("/var/tilo-apps/com.example.notes/bin/notes")
            );
        }
        // Phase `build_bwrap_argv_for_entry_uses_provided_bin`.
        {
            let m = mk_manifest("off");
            let argv = build_bwrap_argv_for_entry(
                &m,
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                None,
                "bin/notes-cli",
                &CapabilityPlan::empty(),
            );
            assert_eq!(
                argv.last().map(String::as_str),
                Some("/var/tilo-apps/com.example.notes/bin/notes-cli")
            );
        }
        // Phase `workspace_id_is_setenv_before_the_entrypoint`.
        {
            // bwrap reads everything after the entrypoint token as the child's
            // argv — a `--setenv` spliced after it silently becomes a
            // command-line arg instead of an env var. Pin the ordering.
            let m = mk_manifest("off");
            let mut argv = build_bwrap_argv(
                &m,
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                None,
                &CapabilityPlan::empty(),
            );
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

    #[test]
    fn wayland_bind_exposes_only_the_normalised_public_socket() {
        // Phase `wayland_bind_normalises_the_in_sandbox_display_name`.
        {
            // Host is on wayland-4; the app must still see wayland-0.
            let bind = wayland_bind_for(Path::new("/run/user/1000/wayland-4"), 1000);
            assert_eq!(bind.display_name, "wayland-0");
            assert_eq!(bind.sandbox_socket, Path::new("/run/user/1000/wayland-0"));
            assert_eq!(bind.sandbox_runtime_dir, Path::new("/run/user/1000"));
            assert_eq!(bind.host_socket, Path::new("/run/user/1000/wayland-4"));
        }
        // Phase `sandbox_binds_only_the_public_wayland_socket`.
        // The sandbox must bind exactly one Wayland socket — the public one
        // — never the privileged one, which is how the clipboard-manager gate
        // in `crates/session/shell/src/compositor/selection.rs` stays closed to a sandboxed app.
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
        // Phase `wayland_bind_argv_carries_socket_and_env`.
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

    /// Value of `--setenv <key> <value>` for `key`, or `None` if absent.
    fn setenv_value<'a>(argv: &'a [String], key: &str) -> Option<&'a str> {
        argv.windows(3)
            .find(|w| w[0] == "--setenv" && w[1] == key)
            .map(|w| w[2].as_str())
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn argv_exports_only_validated_launch_metadata() {
        // Phase `argv_forwards_only_a_plain_save_directory`.
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
        // Phase `argv_forwards_the_complete_validated_continuation_envelope`.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_CONTINUITY_INSTANCE", "42")
                .set("TILO_CONTINUITY_RESUME_SCHEMA", "7")
                .set("TILO_CONTINUITY_RESUME_HEX", "7b7d");
            let argv = build_bwrap_argv(
                &mk_manifest("off"),
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                None,
                &CapabilityPlan::empty(),
            );
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
        }
        // Phase `argv_forwards_only_shell_owned_startup_metrics_paths`.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(
                "TILO_STARTUP_METRICS_PATH",
                "/run/tilo/app/.local/state/startup-42-dead_beef.json",
            );
            let argv = build_bwrap_argv(
                &mk_manifest("off"),
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                None,
                &CapabilityPlan::empty(),
            );
            assert_eq!(
                setenv_value(&argv, "TILO_STARTUP_METRICS_PATH"),
                Some("/run/tilo/app/.local/state/startup-42-dead_beef.json")
            );

            // The app's home itself is what Files lists; a trace there is refused.
            env.set(
                "TILO_STARTUP_METRICS_PATH",
                "/run/tilo/app/startup-42-dead_beef.json",
            );
            let argv = build_bwrap_argv(
                &mk_manifest("off"),
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                None,
                &CapabilityPlan::empty(),
            );
            assert_eq!(setenv_value(&argv, "TILO_STARTUP_METRICS_PATH"), None);

            env.set("TILO_STARTUP_METRICS_PATH", "/home/user/metrics.json");
            let argv = build_bwrap_argv(
                &mk_manifest("off"),
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                None,
                &CapabilityPlan::empty(),
            );
            drop(env);
            assert_eq!(setenv_value(&argv, "TILO_STARTUP_METRICS_PATH"), None);
        }
        // Phase `argv_exports_the_app_name_from_the_manifest`.
        {
            // TILO_APP_NAME exported alongside TILO_PACKAGE_ID for the window title.
            let m = mk_manifest("off");
            let argv = build_bwrap_argv(
                &m,
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                None,
                &CapabilityPlan::empty(),
            );
            assert_eq!(setenv_value(&argv, "TILO_APP_NAME"), Some("Notes"));
        }
        // Phase `argv_omits_the_app_name_when_the_manifest_has_none`.
        {
            // A manifest with no `[package].name` exports nothing.
            let mut m = mk_manifest("off");
            m.name = None;
            let argv = build_bwrap_argv(
                &m,
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                None,
                &CapabilityPlan::empty(),
            );
            assert_eq!(setenv_value(&argv, "TILO_APP_NAME"), None);
        }
        // Phase `argv_exports_the_icon_path_under_the_payload_mount`.
        {
            // Icon art named payload-relative; builder resolves it to the
            // in-sandbox payload mount.
            let mut m = mk_manifest("off");
            m.icon = Some("assets/icon.png".to_string());
            let argv = build_bwrap_argv(
                &m,
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                None,
                &CapabilityPlan::empty(),
            );
            assert_eq!(
                setenv_value(&argv, "TILO_APP_ICON"),
                Some("/var/tilo-apps/com.example.notes/assets/icon.png"),
            );
        }
        // Phase `argv_omits_the_icon_when_the_manifest_has_none`.
        {
            // No icon art: no TILO_APP_ICON, so the toolkit draws its default.
            let m = mk_manifest("off");
            let argv = build_bwrap_argv(
                &m,
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                None,
                &CapabilityPlan::empty(),
            );
            assert_eq!(setenv_value(&argv, "TILO_APP_ICON"), None);
        }
    }

    #[test]
    fn session_bus_reaches_the_sandbox_only_through_a_proxy_plan() {
        // Phase `parse_unix_path_bus_address_extracts_path`.
        {
            assert_eq!(
                parse_unix_path_bus_address("unix:path=/run/user/1000/bus"),
                Some("/run/user/1000/bus".to_string())
            );
            assert_eq!(
                parse_unix_path_bus_address("unix:path=/run/user/1000/bus,guid=deadbeef"),
                Some("/run/user/1000/bus".to_string())
            );
            // Abstract socket → None (can't be bind-mounted).
            assert_eq!(
                parse_unix_path_bus_address("unix:abstract=/tmp/dbus-xyz"),
                None
            );
            // No unix transport.
            assert_eq!(parse_unix_path_bus_address("tcp:host=127.0.0.1"), None);
        }
        // Phase `argv_binds_dbus_proxy_socket_when_plan_is_supplied`.
        {
            let mut m = mk_manifest("off");
            m.sandbox
                .portals
                .insert("notifications".to_string(), PortalPolicy::ViaHost);
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
                None,
                &CapabilityPlan::empty(),
            );
            // Proxy socket bound at /run/user/1234/bus, NOT the raw bus path.
            assert!(
                argv.windows(3).any(|w| w[0] == "--ro-bind"
                    && w[1] == "/run/user/1234/tilo/dbus-proxy/9999.sock"
                    && w[2] == "/run/user/1234/bus"),
                "proxy socket ro-bind missing from argv: {argv:?}"
            );
            // The sandbox sees the bus at /run/user/<uid>/bus, so the env var
            // reflects that.
            assert!(
                argv.windows(3).any(|w| w[0] == "--setenv"
                    && w[1] == "DBUS_SESSION_BUS_ADDRESS"
                    && w[2] == "unix:path=/run/user/1234/bus"),
                "dbus setenv missing from argv: {argv:?}"
            );
        }
        // Phase `argv_does_not_bind_dbus_without_plan`.
        {
            let m = mk_manifest("off");
            let argv = build_bwrap_argv(
                &m,
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                None,
                &CapabilityPlan::empty(),
            );
            assert!(
                !argv.iter().any(|a| a == "DBUS_SESSION_BUS_ADDRESS"),
                "bus must not leak into sandbox without a plan: {argv:?}"
            );
        }
        // Phase `argv_does_not_bind_dbus_when_only_deny_portals`.
        {
            // With the plan-based API, `Deny`-only portals means the caller passes
            // `None` — same as no portals at all.
            let mut m = mk_manifest("off");
            m.sandbox
                .portals
                .insert("screencast".to_string(), PortalPolicy::Deny);
            let argv = build_bwrap_argv(
                &m,
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                None,
                &CapabilityPlan::empty(),
            );
            assert!(
                !argv.iter().any(|a| a == "DBUS_SESSION_BUS_ADDRESS"),
                "bus must stay out when caller passes no plan: {argv:?}"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn identity_handoff_follows_participation() {
        // Phase `identity_handoff_sets_broker_fd_without_mounting_a_key`.
        {
            let m = mk_manifest("off");
            let argv = build_bwrap_argv(
                &m,
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                Some(Path::new("/home/alice/.config/tilo/identity/primary.json")),
                &CapabilityPlan::empty(),
            );
            assert!(
                argv.windows(3).any(|w| w[0] == "--setenv"
                    && w[1] == runtime_identity::broker::BROKER_ENV_VAR
                    && w[2] == runtime_identity::broker::BROKER_CHILD_FD.to_string()),
                "identity broker setenv missing: {argv:?}"
            );
            assert!(
                !argv.iter().any(|arg| arg.contains("identity/primary.json")),
                "private identity path reached the sandbox argv: {argv:?}"
            );
        }
        // Phase `trusted_identity_projection_is_one_live_read_only_directory`.
        {
            let mut argv = Vec::new();
            push_trusted_identities_bind(
                &mut argv,
                Path::new("/run/user/1000/tilo/identity-projections/room/trusted.json"),
            );
            assert_eq!(
                argv,
                vec![
                    "--ro-bind",
                    "/run/user/1000/tilo/identity-projections/room",
                    SANDBOX_TRUSTED_IDENTITIES_DIR,
                ]
            );
        }
        // Phase `a_read_only_participant_keeps_only_the_public_identity_projection`.
        // A read-only participant gets public identity metadata for attribution,
        // but the launcher has already stripped the signing key.
        {
            use crate::capabilities::Participation;

            let m = mk_manifest("off");
            let host = Path::new("/home/alice/.config/tilo/identity/primary.json");
            let read_only = CapabilityPlan {
                participation: Participation::ReadOnly,
                ..CapabilityPlan::empty()
            };
            let argv = build_bwrap_argv(
                &m,
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                Some(host),
                &read_only,
            );
            assert!(
                !argv
                    .iter()
                    .any(|a| a == runtime_identity::broker::BROKER_ENV_VAR),
                "a read-only participant must not receive a signing capability: {argv:?}"
            );
            assert!(
                !argv.iter().any(|a| a == host.to_str().unwrap()),
                "nor may the private identity path enter its namespace: {argv:?}"
            );
            // …and it is told which shape it is in, so chrome can present a
            // viewer rather than an editor whose saves vanish.
            assert!(
                argv.windows(3).any(|w| w[0] == "--setenv"
                    && w[1] == "TILO_PARTICIPATION"
                    && w[2] == "read-only"),
                "the app must be able to see that it is a viewer: {argv:?}"
            );

            let authoring = CapabilityPlan {
                participation: Participation::Author,
                ..CapabilityPlan::empty()
            };
            let argv = build_bwrap_argv(
                &m,
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                Some(host),
                &authoring,
            );
            assert!(
                argv.windows(3).any(|w| w[0] == "--setenv"
                    && w[1] == runtime_identity::broker::BROKER_ENV_VAR
                    && w[2] == runtime_identity::broker::BROKER_CHILD_FD.to_string()),
                "an authoring participant gets a signing broker: {argv:?}"
            );
            assert!(
                argv.windows(3).any(|w| w[0] == "--setenv"
                    && w[1] == "TILO_PARTICIPATION"
                    && w[2] == "author"),
                "and is told so: {argv:?}"
            );
        }
        // Phase `an_unscoped_launch_is_unchanged`.
        // An app no workspace capability names keeps the launch it always had:
        // brokered identity, and no participation claim made on its behalf.
        {
            let m = mk_manifest("off");
            let host = Path::new("/home/alice/.config/tilo/identity/primary.json");
            let argv = build_bwrap_argv(
                &m,
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                Some(host),
                &CapabilityPlan::empty(),
            );
            assert!(
                argv.windows(3).any(|w| w[0] == "--setenv"
                    && w[1] == runtime_identity::broker::BROKER_ENV_VAR
                    && w[2] == runtime_identity::broker::BROKER_CHILD_FD.to_string()),
                "an unscoped launch keeps brokered identity: {argv:?}"
            );
            assert!(
                !argv.iter().any(|a| a == "TILO_PARTICIPATION"),
                "and no participation is claimed on its behalf: {argv:?}"
            );
        }
        // Phase `identity_bind_absent_when_host_path_is_none`.
        {
            let m = mk_manifest("off");
            let argv = build_bwrap_argv(
                &m,
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                None,
                &CapabilityPlan::empty(),
            );
            assert!(
                !argv
                    .iter()
                    .any(|a| a == runtime_identity::broker::BROKER_ENV_VAR),
                "identity broker fd must not appear without a host identity: {argv:?}"
            );
            assert!(
                !argv.iter().any(|a| a == "TILO_USER_IDENTITY"),
                "legacy identity path env must never appear: {argv:?}"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn gpu_selection_and_drm_grants_shape_device_binds() {
        // Phase `gpu_selection_narrows_the_device_and_pins_the_loaders`.
        // A selection with both loaders resolved must hide every GPU except
        // the compositor's, bind the manifests its variables name, and set
        // those variables.
        {
            let sel = crate::gpu::GpuSelection {
                node: PathBuf::from("/dev/dri/renderD128"),
                driver: Some("amdgpu".to_string()),
                egl_vendor_json: Some(PathBuf::from("/usr/share/glvnd/egl_vendor.d/50_mesa.json")),
                vulkan_icd_json: Some(PathBuf::from(
                    "/usr/share/vulkan/icd.d/radeon_icd.x86_64.json",
                )),
                notes: Vec::new(),
            };
            let mut argv = Vec::new();
            push_gpu_selection(&mut argv, &sel, true);

            // The tmpfs must come before the node re-bind, or the bind is
            // the thing that gets hidden.
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
        // Phase `gpu_selection_does_not_mount_a_device_the_manifest_withheld`.
        // An app the manifest gave no DRM access to gets no device mount —
        // the GPU plumbing must not hand out access nobody asked for.
        {
            let sel = crate::gpu::GpuSelection {
                node: PathBuf::from("/dev/dri/renderD128"),
                driver: Some("amdgpu".to_string()),
                egl_vendor_json: Some(PathBuf::from("/usr/share/glvnd/egl_vendor.d/50_mesa.json")),
                vulkan_icd_json: None,
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
        // Phase `drm_grant_detection_follows_the_profile_and_capability_grants`.
        // The device is in the namespace because the *system* put it there,
        // so the narrowing must fire for every app — including one whose
        // manifest says nothing. A user-granted node counts too.
        {
            let none = CapabilityPlan::empty();
            assert!(
                grants_drm(&mk_manifest("off"), &none),
                "the system profile binds /dev/dri for every app, so every app \
                 must have its device view narrowed"
            );

            let mut granted = CapabilityPlan::empty();
            granted.rw_paths.push(PathBuf::from("/dev/dri/renderD129"));
            assert!(
                grants_drm(&mk_manifest("off"), &granted),
                "a user-granted node must be narrowed too"
            );

            // The manifest is not consulted. A package that writes
            // `/dev/dri` into its (retired) fs block changes nothing.
            let mut hostile = mk_manifest("off");
            hostile.sandbox.fs.write.push("/dev/dri".to_string());
            assert_eq!(
                grants_drm(&hostile, &none),
                grants_drm(&mk_manifest("off"), &none),
                "a manifest entry must not change the GPU decision"
            );
        }
    }

    #[test]
    fn service_sockets_bind_as_child_fds_only() {
        // Phase `service_binds_name_their_child_fd`.
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
                None,
                &CapabilityPlan::empty(),
            );
            assert!(
                argv.windows(3)
                    .any(|w| w[0] == "--setenv" && w[1] == "TILO_SVC_COLLAB_FD" && w[2] == "17"),
                "the app must be told which descriptor its service is on: {argv:?}"
            );
        }
        // Phase `service_sockets_are_never_handed_to_bwrap_as_files`.
        // `--file FD DEST` copies FD's contents into a file; pointed at a
        // connected socket it blocks bwrap on an EOF the broker never sends.
        // The descriptor reaches the child by inheritance instead.
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
                None,
                &CapabilityPlan::empty(),
            );
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
            // Nothing emits `--file` any more, so the statement is flat: not on
            // these fds, ever.
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

    // Capability enforcement: assert on the production argv itself — a path
    // missing from it is a path the app cannot reach, whatever the UI says.

    use collab_types::caps::{CapOverride, Expiry, Scope, Subject, Verb};

    /// Deterministic stand-in for the host's device nodes, so a camera
    /// assertion doesn't depend on the test machine owning one.
    fn fake_device_paths(device: &str) -> Vec<PathBuf> {
        match device {
            "camera" => vec![PathBuf::from("/dev/video0")],
            "microphone" => vec![PathBuf::from("/dev/snd")],
            _ => Vec::new(),
        }
    }

    /// Build the argv the launcher would really produce for `app_id`,
    /// through the same production path as `run_with`; only the
    /// device-node lookup is substituted.
    fn argv_with_caps(app_id: &str, caps: &[CapOverride], now: u64) -> Vec<String> {
        let m = mk_manifest("off");
        let plan = CapabilityPlan::for_app_with(app_id, caps, Some(now), fake_device_paths);
        build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None, &plan)
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

    fn fs_grant(app: &str, path: &str, verb: Verb) -> CapOverride {
        CapOverride::grant(Subject::app(app), Scope::FsPath(path.to_string()), verb)
    }

    /// Everything a launch with no grants binds, as an equality (not
    /// `any()` probes) so a new mount can't sneak into the default sandbox.
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
        for p in crate::profile::SYSTEM_DEVICE_PATHS {
            expected.push((
                "--dev-bind-try".to_string(),
                (*p).to_string(),
                (*p).to_string(),
            ));
        }
        expected
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn workspace_capabilities_decide_filesystem_and_camera_binds() {
        // Phase `no_capabilities_adds_no_binds_beyond_the_system_profile`.
        {
            // The argv builder reads the session environment.
            let _env = tilo_test_env::lock();
            let baseline = argv_with_caps("com.example.notes", &[], 1_000);
            let m = mk_manifest("off");
            let manifest_only = build_bwrap_argv(
                &m,
                Path::new("/host/pkg"),
                None,
                &[],
                None,
                None,
                &CapabilityPlan::empty(),
            );
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
        // Phase `read_capability_binds_the_path_read_only_and_never_writable`.
        {
            let caps = vec![fs_grant(
                "so.tilo.books",
                "/var/workspace/books",
                Verb::Read,
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
            // The grant is for that path and nothing above it: the parent
            // directory must not be reachable.
            assert!(
                !argv.iter().any(|a| a == "/var/workspace"),
                "a grant on /var/workspace/books must not bind its parent: {argv:?}"
            );
        }
        // Phase `write_capability_binds_the_path_read_write`.
        {
            let caps = vec![fs_grant(
                "so.tilo.books",
                "/var/workspace/books",
                Verb::Write,
            )];
            let argv = argv_with_caps("so.tilo.books", &caps, 1_000);
            assert!(
                argv.windows(3).any(|w| w[0] == "--bind-try"
                    && w[1] == "/var/workspace/books"
                    && w[2] == "/var/workspace/books"),
                "a Write capability must produce a read-write bind: {argv:?}"
            );
            // Scoped to the granted path: the manifest's own `fs.read`
            // declarations legitimately emit read-only binds of their own.
            for (flag, src, _dst) in bind_triples(&argv) {
                if src == "/var/workspace/books" {
                    assert!(
                        flag == "--bind" || flag == "--bind-try",
                        "a Write capability must not also emit the read-only bind `{flag}`: {argv:?}"
                    );
                }
            }
        }
        // Phase `camera_bind_appears_only_with_a_camera_capability`.
        {
            let without = argv_with_caps("so.tilo.meet", &[], 1_000);
            assert!(
                !without.iter().any(|a| a.starts_with("/dev/video")),
                "no camera capability must mean no camera device in the namespace: {without:?}"
            );

            let caps = vec![CapOverride::grant(
                Subject::app("so.tilo.meet"),
                Scope::Device("camera".to_string()),
                Verb::Read,
            )];
            let with = argv_with_caps("so.tilo.meet", &caps, 1_000);
            assert!(
                with.windows(3).any(|w| w[0] == "--dev-bind-try"
                    && w[1] == "/dev/video0"
                    && w[2] == "/dev/video0"),
                "a camera capability must bind the device node: {with:?}"
            );
        }
        // Phase `a_capability_for_another_app_never_reaches_this_apps_argv`.
        {
            let caps = vec![
                fs_grant("so.tilo.other", "/var/workspace/secrets", Verb::Write),
                CapOverride::grant(
                    Subject::app("so.tilo.other"),
                    Scope::Device("camera".to_string()),
                    Verb::Read,
                ),
                CapOverride::grant(
                    Subject::app("so.tilo.other"),
                    Scope::NetHost("*".to_string()),
                    Verb::Write,
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
            // Proof the fixture is capable of granting: the same list, read
            // as the app it names, does produce those binds.
            let theirs = argv_with_caps("so.tilo.other", &caps, 1_000);
            assert!(theirs.iter().any(|a| a == "/var/workspace/secrets"));
            assert!(theirs.iter().any(|a| a == "--share-net"));
        }
        // Phase `an_expired_capability_produces_no_bind`.
        {
            // The argv builder reads the session environment.
            let _env = tilo_test_env::lock();
            let caps = vec![
                fs_grant("so.tilo.books", "/var/workspace/books", Verb::Write)
                    .expiring(Expiry::AtUnix(500)),
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
            // Same capability before its deadline does bind — so the
            // emptiness above is the expiry, not a broken fixture.
            let live = argv_with_caps("so.tilo.books", &caps, 499);
            assert!(
                live.windows(3).any(|w| w[0] == "--bind-try"
                    && w[1] == "/var/workspace/books"
                    && w[2] == "/var/workspace/books"),
                "the same capability evaluated before its deadline must bind: {live:?}"
            );
        }
    }

    #[test]
    fn network_capabilities_decide_namespace_and_egress_gate() {
        // Phase `network_capability_is_what_shares_the_namespace`.
        {
            // The manifest says net.mode = "off": without a capability the sandbox
            // keeps the empty network namespace, and no packet can leave.
            let isolated = argv_with_caps("so.tilo.mail", &[], 1_000);
            assert!(
                !isolated.iter().any(|a| a == "--share-net"),
                "no network capability must leave the netns unshared: {isolated:?}"
            );

            let caps = vec![CapOverride::grant(
                Subject::app("so.tilo.mail"),
                Scope::NetHost("*".to_string()),
                Verb::Write,
            )];
            let granted = argv_with_caps("so.tilo.mail", &caps, 1_000);
            assert!(
                granted.iter().any(|a| a == "--share-net"),
                "a grant for `*` is unrestricted access and shares the namespace: {granted:?}"
            );
        }
        // Phase `a_grant_naming_a_host_no_longer_shares_the_whole_network`.
        // A named host must not share the whole network — the grant must
        // reach a real mechanism (the gate socket the empty namespace leaves
        // as the only way out), not just an argv string. Packets asserted in
        // `tests/netgate_egress.rs`.
        {
            let caps = vec![CapOverride::grant(
                Subject::app("so.tilo.mail"),
                Scope::NetHost("github.com".to_string()),
                Verb::Write,
            )];
            let argv = argv_with_caps("so.tilo.mail", &caps, 1_000);

            assert!(
                !argv.iter().any(|a| a == "--share-net"),
                "a grant naming one host must NOT hand the app the host's whole network — that \
                 equivalence between `github.com` and `*` is the bug this closes: {argv:?}"
            );

            // And the grant does reach a mechanism now: the gate decision comes from
            // the same manifest + capability pair that built the argv above.
            let plan =
                CapabilityPlan::for_app_with("so.tilo.mail", &caps, Some(1_000), |_| Vec::new());
            let decision = egress_decision(&mk_manifest("off"), &plan);
            let policy = decision.policy().unwrap_or_else(|| {
                panic!("a named host must resolve to a filtered egress policy, got {decision:?}")
            });
            assert!(policy.permits("github.com"));
            assert!(
                !policy.permits("evil.example.com"),
                "the allowlist must exclude what it does not name: {policy:?}"
            );
        }
        // Phase `the_gate_bind_is_writable_and_points_the_app_at_the_relay`.
        {
            let bind = NetGateBind {
                host_socket: PathBuf::from("/run/user/1000/tilo/net/7.sock"),
                sandbox_socket: PathBuf::from(crate::netgate::SANDBOX_GATE_PATH),
                port: crate::netgate::RELAY_PORT,
            };
            let mut argv = Vec::new();
            push_net_gate(&mut argv, &bind);

            assert!(
                argv.windows(3).any(|w| w[0] == "--bind"
                    && w[1] == "/run/user/1000/tilo/net/7.sock"
                    && w[2] == crate::netgate::SANDBOX_GATE_PATH),
                "the gate socket must be bound read-write: connect(2) on a pathname socket is a \
                 write and a --ro-bind surfaces as EACCES: {argv:?}"
            );
            // socks5h, not socks5: the `h` sends the hostname to the gate instead of
            // resolving it locally, which the sandbox cannot do.
            let socks = format!("socks5h://127.0.0.1:{}", crate::netgate::RELAY_PORT);
            for var in ["ALL_PROXY", "all_proxy"] {
                assert!(
                    argv.windows(3)
                        .any(|w| w[0] == "--setenv" && w[1] == var && w[2] == socks),
                    "{var} must point at the relay as socks5h: {argv:?}"
                );
            }
            // And the HTTP form works without an opt-in feature — an app whose
            // HTTP library lacks SOCKS support reads only these.
            let http = format!("http://127.0.0.1:{}", crate::netgate::RELAY_PORT);
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
