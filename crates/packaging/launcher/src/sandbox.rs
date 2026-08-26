//! Bubblewrap argv construction from a [`MinimalManifest`].
//!
//! Pure function: given a manifest, the payload root, an optional
//! runtime root, and the resolved service fd map, return the argv
//! list the launcher will hand to `bwrap`. No I/O, no side effects —
//! the `run()` path in `lib.rs` is responsible for fd plumbing.
//!
//! The shape mirrors the conventions in `shell/src/sandbox/` (the
//! per-cluster sandbox profile): `--ro-bind <src> <dst>` for read-only
//! mounts, `--bind` for read-write, `--unshare-all`, optional
//! `--share-net`, `--tmpfs /tmp`, `--proc /proc`, `--dev /dev`,
//! `--die-with-parent`.

use std::path::{Path, PathBuf};

use crate::capabilities::CapabilityPlan;
use crate::manifest::{MinimalManifest, PortalPolicy};

/// In-sandbox mount root for the package payload.
pub fn payload_mount_for(package_id: &str) -> PathBuf {
    PathBuf::from("/var/qos-apps").join(package_id)
}

/// In-sandbox mount root for the runtime (qos-runtime/<version>/).
/// Returns `None` when the caller did not supply a runtime root.
pub fn runtime_mount_for(version: &str) -> PathBuf {
    PathBuf::from("/usr/lib/qos-runtime").join(version)
}

/// Canonical absolute paths we re-expose out of the runtime bundle so
/// dynamically-linked ELFs (both the launcher-shim and the app entry
/// binary) can find their loader + libc where their `INTERP` header
/// points. Superset across supported archs — the runtime-bind loop
/// only re-binds paths whose corresponding file actually exists in the
/// bundle, so listing both x86_64 and aarch64 entries here is safe.
/// Kept in sync with `runtime_bundle::host_minimal_paths_for`.
pub const CANONICAL_LOADER_PATHS: &[&str] = &[
    // x86_64 loader shapes.
    "/lib64/ld-linux-x86-64.so.2",
    "/lib/ld-linux-x86-64.so.2",
    "/lib/ld-linux.so.2",
    "/usr/lib64/ld-linux-x86-64.so.2",
    "/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
    // aarch64 loader shapes.
    "/lib/ld-linux-aarch64.so.1",
    "/lib64/ld-linux-aarch64.so.1",
    "/usr/lib64/ld-linux-aarch64.so.1",
    "/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1",
    // Common libc + friends (Fedora-ish /lib64).
    "/lib64/libc.so.6",
    "/lib64/libgcc_s.so.1",
    "/lib64/libm.so.6",
    "/lib64/libdl.so.2",
    "/lib64/libpthread.so.0",
    "/lib64/librt.so.1",
    "/lib64/libutil.so.1",
    "/usr/lib64/libc.so.6",
    "/usr/lib64/libgcc_s.so.1",
    "/usr/lib64/libm.so.6",
    "/usr/lib64/libdl.so.2",
    "/usr/lib64/libpthread.so.0",
    "/usr/lib64/librt.so.1",
    "/usr/lib64/libutil.so.1",
    // Debian-ish multiarch layouts.
    "/lib/x86_64-linux-gnu/libc.so.6",
    "/lib/x86_64-linux-gnu/libgcc_s.so.1",
    "/lib/x86_64-linux-gnu/libm.so.6",
    "/lib/x86_64-linux-gnu/libdl.so.2",
    "/lib/x86_64-linux-gnu/libpthread.so.0",
    "/lib/x86_64-linux-gnu/librt.so.1",
    "/lib/x86_64-linux-gnu/libutil.so.1",
    "/lib/aarch64-linux-gnu/libc.so.6",
    "/lib/aarch64-linux-gnu/libgcc_s.so.1",
    "/lib/aarch64-linux-gnu/libm.so.6",
    "/lib/aarch64-linux-gnu/libdl.so.2",
    "/lib/aarch64-linux-gnu/libpthread.so.0",
    "/lib/aarch64-linux-gnu/librt.so.1",
    "/lib/aarch64-linux-gnu/libutil.so.1",
];

/// Explicit plan for exposing D-Bus into the sandbox.
///
/// Constructed by the orchestrator (`run.rs`) and threaded through
/// [`build_bwrap_argv`] so the argv-building layer stays pure. Two
/// shapes:
/// - *Proxy*: the launcher has spawned `xdg-dbus-proxy` upstream of
///   the session bus with an explicit allowlist; `host_socket`
///   points at the proxy's listener.
/// - *Direct*: `xdg-dbus-proxy` is unavailable; `host_socket` points
///   at the raw session bus socket (path only — abstract sockets
///   don't bind-mount, which is exactly the case a proxy would
///   handle).
///
/// From bwrap's point of view both shapes look identical: one
/// `--ro-bind` triple and one `DBUS_SESSION_BUS_ADDRESS` setenv. The
/// distinction is documented so consumers can log the fallback and
/// audit tests can assert on it.
#[derive(Debug, Clone)]
pub struct DBusBindPlan {
    /// Host-side path we bind into the sandbox.
    pub host_socket: PathBuf,
    /// In-sandbox path the socket appears at. Apps read
    /// `DBUS_SESSION_BUS_ADDRESS=unix:path=<sandbox_path>`.
    pub sandbox_path: PathBuf,
    /// Which shape this plan carries — for logging + tests. Does
    /// not change the argv shape.
    pub kind: DBusBindKind,
}

/// Which flavour of D-Bus bind a [`DBusBindPlan`] describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DBusBindKind {
    /// Filtered via `xdg-dbus-proxy` — the preferred path.
    Proxy,
    /// Raw session-bus socket bind — degraded fallback used when
    /// `xdg-dbus-proxy` isn't installed.
    Direct,
}

/// Where a GUI app's Wayland socket appears inside the sandbox, and the
/// env it needs to find it.
///
/// The in-sandbox name is normalised to `wayland-0` no matter which
/// display the compositor actually bound on the host (`wayland-1`,
/// `wayland-4`, …): the sandbox has its own mount namespace, so there is
/// exactly one socket in it and the app has no business knowing the
/// host-side name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaylandBind {
    /// Host-side socket path (the compositor's listening socket).
    pub host_socket: PathBuf,
    /// In-sandbox absolute path of the socket.
    pub sandbox_socket: PathBuf,
    /// In-sandbox `XDG_RUNTIME_DIR`.
    pub sandbox_runtime_dir: PathBuf,
    /// Value of `WAYLAND_DISPLAY` inside the sandbox.
    pub display_name: String,
}

/// Build the [`WaylandBind`] plan for `host_socket`, exposing it under
/// `/run/user/<uid>` inside the sandbox.
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

/// Splice a Wayland display into an argv built by [`build_bwrap_argv`].
///
/// `--ro-bind` (not `--bind`) matches what Flatpak does for
/// `$WAYLAND_DISPLAY`: `connect(2)` on a Unix socket needs write
/// permission on the *inode*, which a read-only bind mount does not take
/// away, while the read-only mount does stop the app from unlinking or
/// replacing the compositor's socket.
///
/// Callers must splice this in *before* the trailing entrypoint token —
/// bwrap treats everything after it as the child's argv.
///
/// Exactly **one** Wayland socket is bound: the session's public one. The
/// compositor also listens on a privileged socket that carries the
/// clipboard-manager protocols (`zwlr_data_control_manager_v1` and
/// `ext_data_control_manager_v1`), which hand their holder every selection on
/// the seat with no focus requirement — see
/// `shell/src/compositor/selection.rs`. That socket is deliberately not bound
/// here, so a sandboxed app has no path to it. The name is also unset from
/// the environment: not because knowing it would help without the mount, but
/// because an app should not be told about a door it is not allowed through.
pub fn push_wayland_bind(argv: &mut Vec<String>, bind: &WaylandBind) {
    push_bind(argv, "--ro-bind", &bind.host_socket, &bind.sandbox_socket);
    argv.push("--setenv".to_string());
    argv.push("WAYLAND_DISPLAY".to_string());
    argv.push(bind.display_name.clone());
    argv.push("--setenv".to_string());
    argv.push("XDG_RUNTIME_DIR".to_string());
    argv.push(bind.sandbox_runtime_dir.display().to_string());
    // Redundant once [`push_clean_environment`] has emptied the environment,
    // kept so a caller that binds a socket without going through the argv
    // builder still does not name the privileged one.
    argv.push("--unsetenv".to_string());
    argv.push(PRIVILEGED_WAYLAND_ENV.to_string());
}

/// The `PATH` a sandboxed app runs with.
///
/// Fixed rather than inherited: the launching session's `PATH` names host
/// directories that are not in the sandbox's namespace, and one that *is* — a
/// writable directory the user put on their own `PATH` — would decide which
/// binary the app's `exec` finds.
pub const SANDBOX_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// The whole of what a sandboxed app inherits from the launching session.
///
/// An allowlist rather than a filter, because a deny-list is wrong the moment
/// somebody adds a variable and the failure is silent. Everything here
/// describes the *user* — their language, their clock, their terminal — and
/// nothing here names a socket, a room, an identity or a path: those reach the
/// app through an explicit `--setenv` beside the bind that makes them
/// reachable, and a variable naming something the app cannot open is a promise
/// the namespace does not keep.
pub const INHERITED_ENV: &[&str] = &[
    // `LANGUAGE` is the GNU fallback chain and `LC_ALL` overrides the rest, so
    // a session that set only one of these gets only that one honoured,
    // exactly as it would outside the sandbox.
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_COLLATE",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_MONETARY",
    "LC_NUMERIC",
    "LC_TIME",
    // `/etc/localtime` is in the system profile, so this is for the session
    // that chose a different zone.
    "TZ",
    // Absent for a GUI launch, which is why these are inherited-when-set
    // rather than given a default.
    "TERM",
    "COLORTERM",
    // The same kind of preference, naming no socket and no path: without it an
    // app's `EnvFilter::from_default_env()` falls back to errors only and
    // everything it reports about itself is dropped before the session reads
    // it.
    "RUST_LOG",
];

/// Empty the environment, then put back what the app is entitled to.
///
/// `bwrap` passes the parent's environment straight through unless it is told
/// not to, and the parent here is the compositor: its Wayland socket, its
/// collaboration broker, its identity path, and every `QOS_*` variable
/// describing a confinement this process is on the other side of. So the
/// environment is *built*, not filtered — `--clearenv` first, because bwrap
/// keeps only what a later `--setenv` puts back, then `PATH`, then
/// [`INHERITED_ENV`]. Everything else is set beside the bind that makes it
/// mean something.
pub fn push_clean_environment(argv: &mut Vec<String>) {
    argv.push("--clearenv".to_string());
    argv.push("--setenv".to_string());
    argv.push("PATH".to_string());
    argv.push(SANDBOX_PATH.to_string());
    for key in INHERITED_ENV {
        let Some(value) = std::env::var_os(key) else {
            continue;
        };
        // Carrying an empty value over would make "unset" and "empty" two
        // different states inside the sandbox for no reason.
        if value.is_empty() {
            continue;
        }
        argv.push("--setenv".to_string());
        argv.push((*key).to_string());
        argv.push(value.to_string_lossy().into_owned());
    }
}

/// Env var the compositor uses to publish its privileged Wayland socket.
///
/// Mirrors `shell::compositor::native::PRIVILEGED_SOCKET_ENV`. Duplicated
/// rather than imported because the launcher does not depend on the
/// compositor crate; the pair is held together by
/// [`tests::sandbox_binds_only_the_public_wayland_socket`].
pub const PRIVILEGED_WAYLAND_ENV: &str = "QOS_WAYLAND_PRIVILEGED";

/// Env var naming the workspace an app was launched into.
///
/// Two consumers, and they must not be confused:
///
/// - `runtime_portal::is_qos_native()` only asks whether it is *set*
///   ("am I running under a qOS session?"). Giving it a real value
///   keeps that answer `true`.
/// - `ui::collaboration` uses the value as the **collaboration
///   cluster id** — the room a CRDT delta is addressed to. Two apps
///   launched into the same workspace share a cluster and exchange
///   deltas; apps in different workspaces are different rooms and
///   never see each other's traffic.
///
/// The value the compositor sets is a UUID string (see
/// `shell::shell::launcher`). Anything else is accepted and passed
/// through — it just doesn't name a cluster, so the app falls back to
/// its standalone-dev identity.
pub const WORKSPACE_ID_ENV: &str = "QOS_WORKSPACE_ID";

/// Splice the workspace id into an argv built by [`build_bwrap_argv`].
///
/// Same contract as [`push_wayland_bind`]: callers must splice this in
/// *before* the trailing entrypoint token, because bwrap treats
/// everything after it as the child's argv.
///
/// Deliberately a splice rather than a parameter on
/// [`build_bwrap_argv`]: the workspace is a property of the *launch*
/// (which room did the user open this app into), not of the package,
/// so it arrives from `RunOptions` alongside the Wayland socket rather
/// than from the manifest the way `QOS_PACKAGE_ID` does.
/// Attach the scoped runtime protections from
/// [`crate::hardening::RuntimeHardening`] to a sandbox.
///
/// Both are environment, but the allocator also needs its library reachable
/// from inside the namespace: the sandbox mounts a curated set of paths, and an
/// `LD_PRELOAD` naming a path that was not bound in is not an error — the loader
/// prints a warning to stderr and the process runs with the ordinary allocator.
/// That failure mode is the reason the bind is emitted here next to the variable
/// rather than left to whatever the manifest happened to list.
///
/// One assumption, stated because it is load-bearing: the preloaded library is
/// the host's, and it is linked against the host's libc. For a package running
/// against a runtime bundle — which rebinds its own loader and libc over the
/// canonical paths — that only holds because the bundle is baked from the same
/// rootfs as the allocator, in the same image build. A bundle from a different
/// glibc would need its own copy of the library.
pub fn push_runtime_hardening(argv: &mut Vec<String>, hardening: &crate::hardening::RuntimeHardening) {
    if let Some(allocator) = &hardening.allocator {
        // Same path inside and out, so LD_PRELOAD and the bind cannot drift.
        push_bind(argv, "--ro-bind", allocator, allocator);
        argv.push("--setenv".to_string());
        argv.push("LD_PRELOAD".to_string());
        argv.push(allocator.display().to_string());
    }
    if hardening.shadow_stack {
        // Marking the binary is not enough on its own: glibc reads this
        // tunable to decide whether to ask the kernel for a shadow stack. On a
        // CPU or kernel without support it is ignored, not an error.
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

/// The DRM device directory. The system profile binds it, which on a
/// multi-GPU host exposes *every* GPU — including ones the compositor
/// is not rendering on. [`push_gpu_selection`] narrows it back to one.
const DRM_DIR: &str = "/dev/dri";

/// Splice the compositor's GPU choice into an argv built by
/// [`build_bwrap_argv`].
///
/// Three things happen here, and all three are needed for the app to
/// land on the same GPU as the compositor:
///
/// 1. **Only the chosen render node is visible.** A tmpfs over
///    `/dev/dri` hides whatever the manifest's `/dev/dri` grant exposed,
///    and the one node is bound back on top. Device enumeration is how
///    Mesa's EGL picks a GPU — there is no environment variable for it —
///    so removing the other nodes is the only way to stop a client
///    opening a GPU the compositor cannot import buffers from. It is
///    also a strict narrowing of the manifest's grant, never a widening.
/// 2. **The loader manifests are readable.** `__EGL_VENDOR_LIBRARY_FILENAMES`
///    and `VK_DRIVER_FILES` name files; a manifest that grants
///    `/usr/share/vulkan` as a directory usually covers them, but an app
///    that grants neither would get variables pointing at nothing —
///    which both loaders read as "no drivers at all".
/// 3. **The variables are set.** Explicitly, rather than inherited from
///    the launcher's environment: what the compositor resolved is a
///    decision, and decisions belong in the argv where they can be
///    asserted on.
///
/// Callers must splice this in *before* the trailing entrypoint token.
/// `grants_drm` says whether the manifest gave this app the DRM device
/// directory at all — an app with no GPU grant gets neither the tmpfs
/// nor the node, because narrowing nothing is nothing and creating an
/// empty `/dev/dri` would be a new (if useless) mount it never asked
/// for.
pub fn push_gpu_selection(
    argv: &mut Vec<String>,
    selection: &crate::gpu::GpuSelection,
    grants_drm: bool,
) {
    if grants_drm {
        // Order matters: the tmpfs must land on top of the manifest's
        // own `/dev/dri` bind, and the node must land on top of the
        // tmpfs.
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

/// Whether this launch exposes the DRM device directory at all.
///
/// The system profile puts `/dev/dri` in every sandbox, so this is
/// normally `true` — but it still consults both sources rather than
/// returning a constant, because [`push_gpu_selection`]'s narrowing has
/// to land on something that was actually mounted, and a future profile
/// that dropped the device would otherwise leave the narrowing creating
/// an empty `/dev/dri` nobody asked for.
///
/// A grant *of* `/dev/dri` and a grant of a node *inside* it both
/// count: an app whose capability names only `/dev/dri/renderD128`
/// still has its device view narrowed to the compositor's node.
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

/// In-sandbox path the launcher ro-binds the host identity JSON at.
///
/// Deliberately outside `~/.config` so the launcher doesn't need to
/// stage the whole config directory into the sandbox (which would
/// leak an inode listing of every other config file the user has).
/// The apps read this path via the `QOS_USER_IDENTITY` env var, which
/// the launcher sets in lock-step with the bind.
pub const SANDBOX_IDENTITY_PATH: &str = "/run/qos/identity.json";

/// Build the bwrap argv from a manifest plus the launcher's resolved
/// inputs.
///
/// `payload_root` is the host path containing the unpacked package;
/// `runtime_root` is the (optional) host directory holding the
/// qos-runtime tree, paired with the version string the manifest
/// pins (today: hard-coded to "1" — `qos-pkg` will surface the real
/// version on integration).
///
/// `service_binds` are the `(name, host_fd_number, child_fd_number)`
/// tuples for opened service sockets. Each turns into a `bwrap --file`
/// triple binding the open fd into the sandbox at a stable per-service
/// path; the entrypoint binary picks them up via `QOS_SVC_<NAME>_FD`
/// env vars (set separately on the `Command`).
///
/// `dbus_bind` — when `Some`, wire the socket into the sandbox and
/// set `DBUS_SESSION_BUS_ADDRESS`. `None` means the sandbox has no
/// D-Bus access (the manifest didn't declare a `via-host` portal, or
/// the caller decided even the direct-bind fallback was too much).
/// The [`crate::dbus_proxy`] module builds the plan when the manifest
/// asks for portals.
///
/// `identity_host_path` is the resolved host location of the user
/// identity JSON (typically `<XDG_CONFIG_HOME>/qos/identity/primary.json`).
/// When present, it is ro-bound into the sandbox at
/// [`SANDBOX_IDENTITY_PATH`] and the `QOS_USER_IDENTITY` env var is
/// set so `runtime-identity::load()` finds it. When absent, no bind
/// and no env var — apps degrade to the "no identity" path unchanged.
///
/// `capabilities` is what the *user* granted this app in the workspace
/// it is being launched into (see [`crate::capabilities`]). It only ever
/// adds binds; passing [`CapabilityPlan::empty`] renders exactly the
/// sandbox the manifest alone describes, which is also what a launch
/// with no capability file gets. That is the whole enforcement story for
/// a revoked or never-granted permission: the mount is simply not there.
///
/// The returned argv ends with the entrypoint command and its args.
pub fn build_bwrap_argv(
    manifest: &MinimalManifest,
    payload_root: &Path,
    runtime: Option<(&Path, &str)>,
    service_binds: &[ServiceBind],
    dbus_bind: Option<&DBusBindPlan>,
    identity_host_path: Option<&Path>,
    capabilities: &CapabilityPlan,
) -> Vec<String> {
    // GUI launch: pick the GUI-flavour bin. Falls back to the
    // legacy top-level `bin` when neither split is set (i.e. old
    // manifest with `type` unset).
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

/// `build_bwrap_argv` variant that lets the caller pick which of
/// the manifest's binaries to exec. Used by the `run-installed`
/// subcommand to launch the CLI-flavour entrypoint instead of the
/// GUI one.
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

    // Namespace + lifetime defaults.
    //
    // The network namespace is shared ONLY for unrestricted access —
    // the manifest asking for the host network with nothing narrowing
    // it, or a grant that literally says `*`. Anything more specific
    // keeps the empty namespace `--unshare-all` gave it and reaches the
    // network through the egress gate instead, because `--share-net` is
    // all-or-nothing and sharing it for a grant naming one host would
    // be applying that grant as "everything".
    argv.push("--unshare-all".to_string());
    if egress_decision(manifest, capabilities).shares_host_network() {
        argv.push("--share-net".to_string());
    }
    argv.push("--die-with-parent".to_string());

    // Payload bind: host payload_root -> /var/qos-apps/<package_id>/
    let payload_mount = payload_mount_for(&manifest.package_id);
    push_bind(
        &mut argv,
        "--ro-bind",
        payload_root,
        &payload_mount,
    );

    // Tell the SDK who we are. Apps use QOS_PACKAGE_ID to pick a
    // per-app fallback state dir in standalone mode; the launcher
    // is the only source of truth for it.
    argv.push("--setenv".to_string());
    argv.push("QOS_PACKAGE_ID".to_string());
    argv.push(manifest.package_id.clone());

    // The human-readable app name, from the manifest `[package].name`.
    // The toolkit uses it for the window title (the string a titlebar /
    // window-list shows) — the counterpart to the `QOS_PACKAGE_ID` the
    // app_id is keyed on. Omitted when the manifest names no `name`; the
    // toolkit then derives a title from the package id.
    if let Some(name) = manifest
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        argv.push("--setenv".to_string());
        argv.push("QOS_APP_NAME".to_string());
        argv.push(name.to_string());
    }

    // The app's window icon, from the manifest `[package].icon`. The
    // manifest names it relative to the payload; the file lives inside
    // the payload we just bound, so the value the app sees is the path
    // under the in-sandbox payload mount. Omitted when the manifest
    // ships no icon art; the toolkit then draws a generated default.
    if let Some(icon_rel) = manifest
        .icon
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let in_sandbox = payload_mount.join(icon_rel.trim_start_matches('/'));
        argv.push("--setenv".to_string());
        argv.push("QOS_APP_ICON".to_string());
        argv.push(in_sandbox.display().to_string());
    }

    // Hardened allocator + shadow stacks, for the packages the image lists as
    // handling untrusted bytes. Decided from the package id against an
    // image-owned policy, not from anything the manifest can ask for — an app
    // must not be able to opt itself out of protection.
    push_runtime_hardening(
        &mut argv,
        &crate::hardening::for_package(&manifest.package_id),
    );

    // Runtime bind: host runtime_root -> /usr/lib/qos-runtime/<version>/
    // and set LD_LIBRARY_PATH inside the sandbox so the entrypoint can
    // find the shared libs the runtime ships.
    //
    // We also bind the loader + libc at their canonical absolute paths
    // (`/lib64/ld-linux-x86-64.so.2`, `/lib64/libc.so.6`, …) because
    // dynamically-linked ELFs hard-code their `INTERP` in the header
    // and the kernel refuses to exec them if that path is unreachable
    // — even with `LD_LIBRARY_PATH` set. The bundle already carries
    // those files at `lib/lib64/…`, so we can just re-bind them.
    // The launcher-shim itself is one such ELF, so this makes the
    // shim exec work.
    if let Some((rt_root, rt_version)) = runtime {
        let rt_mount = runtime_mount_for(rt_version);
        push_bind(&mut argv, "--ro-bind", rt_root, &rt_mount);
        argv.push("--setenv".to_string());
        argv.push("LD_LIBRARY_PATH".to_string());
        argv.push(rt_mount.join("lib").display().to_string());

        // Re-bind each host loader/libc path from inside the bundle
        // at its canonical absolute location. Silently skip files
        // the bundle doesn't contain.
        for canonical in CANONICAL_LOADER_PATHS {
            let no_slash = canonical.trim_start_matches('/');
            let in_bundle = rt_root.join("lib").join(no_slash);
            if in_bundle.exists() {
                argv.push("--ro-bind".to_string());
                argv.push(in_bundle.display().to_string());
                argv.push((*canonical).to_string());
            }
        }
    }

    // Standard "every app needs these" mounts. Come BEFORE the system
    // profile so that its device entries (`/dev/dri`, the nvidia nodes)
    // land on top of the `--dev` tmpfs instead of underneath it.
    argv.push("--tmpfs".to_string());
    argv.push("/tmp".to_string());
    argv.push("--proc".to_string());
    argv.push("/proc".to_string());
    argv.push("--dev".to_string());
    argv.push("/dev".to_string());

    // The system read profile. Fixed, identical for every package, and
    // fed by no manifest field — see `crate::profile` for what may go in
    // it and why. This is where the per-manifest `sandbox.fs.read` list
    // used to be expanded and bound; a package now names nothing.
    crate::profile::push_system_profile(
        &mut argv,
        egress_decision(manifest, capabilities).shares_host_network(),
    );

    // User-granted capabilities. These come after the system profile so
    // a path the user granted read-write wins over the same path the
    // profile exposes read-only.
    //
    // `-try` variants throughout: a capability outlives the thing it
    // names (a granted directory can be deleted, a webcam unplugged),
    // and a stale grant must not turn into a launch failure. It never
    // widens anything — bwrap simply skips a source that isn't there.
    push_capability_binds(&mut argv, capabilities);

    // XDG portals: if the caller supplied a bind plan (either the
    // preferred `xdg-dbus-proxy` socket or a direct-bind fallback),
    // wire it in. If NO plan is supplied, the bus is NOT exposed —
    // apps that didn't declare a portal (or hosts without a proxy
    // AND an abstract-namespace bus) can't reach the D-Bus attack
    // surface at all. The choice of plan (proxy vs direct) is made
    // by `run.rs`; this layer just materialises it.
    if let Some(bind) = dbus_bind {
        push_bind(&mut argv, "--ro-bind", &bind.host_socket, &bind.sandbox_path);
        argv.push("--setenv".to_string());
        argv.push("DBUS_SESSION_BUS_ADDRESS".to_string());
        argv.push(format!("unix:path={}", bind.sandbox_path.display()));
    }

    // Participation: tell the app which shape it is in, so chrome can
    // present a viewer rather than an editor whose saves vanish. This is
    // information, not enforcement — the enforcement is the identity that
    // is about to not be bound.
    if let Some(mode) = capabilities.participation.env_value() {
        argv.push("--setenv".to_string());
        argv.push("QOS_PARTICIPATION".to_string());
        argv.push(mode.to_string());
    }

    // User identity: ro-bind the caller-supplied host path into the
    // sandbox and set QOS_USER_IDENTITY. The host path is a specific
    // file — we deliberately do NOT bind `~/.config/qos/` wholesale
    // so identities the user has for other purposes (secondary
    // devices, staged records) stay off the app's view.
    //
    // A **read-only participant gets no identity at all.** That is what
    // makes "may not author" structural: without a signing key the app
    // cannot produce an authored change for anything downstream to
    // reject, and "there is no identity here" is a state
    // `runtime-identity` already handles. See
    // [`crate::capabilities::Participation`].
    if let Some(host_path) = identity_host_path.filter(|_| capabilities.participation.carries_identity())
    {
        argv.push("--ro-bind".to_string());
        argv.push(host_path.display().to_string());
        argv.push(SANDBOX_IDENTITY_PATH.to_string());
        argv.push("--setenv".to_string());
        argv.push("QOS_USER_IDENTITY".to_string());
        argv.push(SANDBOX_IDENTITY_PATH.to_string());
    }

    // Service sockets reach the app as inherited descriptors, not as
    // bwrap arguments. The launcher dup2s each connected socket onto
    // its `child_fd` (and clears CLOEXEC) in `pre_exec` — see
    // `run::place_service_fds` — and a descriptor left open across
    // `exec` keeps its number through bwrap and through the
    // `__sandbox-exec` re-exec, so the number published here in
    // `QOS_SVC_<NAME>_FD` is the number the app finds.
    //
    // There is deliberately **no bwrap flag** for this. `--file FD
    // DEST` is the one that looks right and is not: it *copies FD's
    // contents* into a regular file at DEST, so pointing it at a
    // connected socket makes bwrap block on a read for an EOF the
    // broker never sends. The launch hangs before `exec`, an app that
    // declares `collab` never starts at all, and the only symptom is
    // an app that never appears. Nothing here may name a live socket
    // to bwrap.
    for sb in service_binds {
        argv.push("--setenv".to_string());
        argv.push(svc_env_var(&sb.name));
        argv.push(sb.child_fd.to_string());
    }

    // The entrypoint command, joined onto the in-sandbox payload mount.
    let entry_abs = payload_mount.join(entry_bin);
    argv.push(entry_abs.display().to_string());

    argv
}

/// The outbound access this launch resolves to, from the packager's
/// manifest and the user's grants.
///
/// One function so the argv layer, the Landlock layer and the gate
/// spawn cannot reach different conclusions about the same launch —
/// the failure mode that would produce a sandbox with an empty
/// namespace and no gate (a silently offline app) or a gate plus
/// `--share-net` (a filter with a hole beside it).
pub fn egress_decision(
    manifest: &MinimalManifest,
    capabilities: &CapabilityPlan,
) -> crate::netgate::EgressDecision {
    let decision = crate::netgate::EgressDecision::resolve(
        &manifest.sandbox.net.mode,
        &manifest.sandbox.net.hosts,
        &capabilities.net_hosts,
    );
    // The operator escape hatch is applied *here*, not at the point the
    // gate is started, because the namespace decision and the gate have
    // to be made from the same answer. Turning the gate off downstream
    // while this layer still reported `Filtered` would leave the app
    // with an empty namespace and nothing to reach through it — offline
    // rather than unfiltered, which is neither of the two behaviours
    // anyone asked for.
    if matches!(decision, crate::netgate::EgressDecision::Filtered(_))
        && std::env::var(crate::run::NET_GATE_ENV).is_ok_and(|v| v == "off")
    {
        tracing::warn!(
            hosts = ?decision.policy().map(|p| p.hosts()),
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
    /// Host-side socket the launcher's gate listens on.
    pub host_socket: PathBuf,
    /// In-sandbox path it is bind-mounted at.
    pub sandbox_socket: PathBuf,
    /// Loopback port the relay listens on inside the sandbox.
    pub port: u16,
}

/// Wire the egress gate into the sandbox: the socket, and the
/// environment that points an ordinary HTTP client at it.
///
/// `--bind`, not `--ro-bind`: `connect(2)` on a pathname socket is a
/// write-flavoured access and a read-only bind surfaces as `EACCES` at
/// connect time.
///
/// Both proxy forms are advertised because the gate serves both on one
/// socket, and because picking only one would break apps:
///
/// - `HTTP(S)_PROXY` → `http://…`. HTTP `CONNECT` is understood by
///   every client with no feature flags. This is the one that matters
///   in practice — `reqwest`'s SOCKS support is a **non-default
///   feature**, so an app built without it ignores `ALL_PROXY`
///   entirely and would end up offline rather than filtered.
/// - `ALL_PROXY` → `socks5h://…`, for clients that prefer SOCKS.
///   `socks5h` rather than `socks5` is load-bearing: the `h` is what
///   sends the **hostname** to the proxy instead of resolving locally
///   first. The sandbox cannot resolve at all, so `socks5` would
///   simply fail.
///
/// Both carry the destination as a name, which is what the policy is
/// matched against.
pub fn push_net_gate(argv: &mut Vec<String>, bind: &NetGateBind) {
    push_bind(argv, "--bind", &bind.host_socket, &bind.sandbox_socket);
    let http = format!("http://127.0.0.1:{}", bind.port);
    for var in ["HTTP_PROXY", "http_proxy", "HTTPS_PROXY", "https_proxy"] {
        argv.push("--setenv".to_string());
        argv.push(var.to_string());
        argv.push(http.clone());
    }
    let socks = format!("socks5h://127.0.0.1:{}", bind.port);
    for var in ["ALL_PROXY", "all_proxy"] {
        argv.push("--setenv".to_string());
        argv.push(var.to_string());
        argv.push(socks.clone());
    }
    // For an app that would rather speak to the gate directly than go
    // through the loopback relay. The relay is a convenience for
    // unmodified clients, not a required hop.
    argv.push("--setenv".to_string());
    argv.push("QOS_NET_GATE".to_string());
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
    format!("QOS_SVC_{}_FD", name.to_uppercase())
}

/// Render a [`CapabilityPlan`]'s paths as bwrap binds.
///
/// Read grants get `--ro-bind-try` even for device nodes: a read
/// capability that silently conferred write access to `/dev/video0`
/// would be the permission model lying. Write grants under `/dev` get
/// `--dev-bind-try`, which is what lets bwrap mount a character device
/// at all.
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

/// Tell the app where the **user's** home is, when a capability actually
/// binds it.
///
/// Inside the sandbox `HOME` is the app's own directory — one per room,
/// per package, with every `XDG_*` pointed inside it (see
/// `crate::appdir`). That is right, and it has a consequence: an app
/// asking the platform "where is home?" is answered with a directory
/// that has nothing of the user's in it. For most apps that is the whole
/// point. For one whose job *is* the user's files it means opening on an
/// empty folder and reporting, accurately and uselessly, zero items.
///
/// So the real home is passed separately — and only when the capability
/// plan has bound a path that contains it. Deriving it from the binds
/// rather than from the manifest is what keeps it honest: the variable
/// cannot name a directory the app is unable to read, because the same
/// grant that sets it is the one that mounted it. An app that was
/// granted nothing does not see the variable at all, and can tell that
/// it has been given nothing rather than guessing.
///
/// Returns the path exported, if any.
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
    argv.push("QOS_USER_HOME".to_string());
    argv.push(home.display().to_string());
    Some(home)
}

/// Pass on the session's workspace roots, narrowed to the ones this app's
/// capability plan has actually bound.
///
/// The same contract as [`push_user_home`], and deliberately **not** on
/// [`INHERITED_ENV`]: that list is for preferences naming no path, and
/// `QOS_WORKSPACE_ROOTS` names paths. Deriving it from the binds instead means
/// the variable can never name a directory the app cannot read, and an app
/// granted none of them sees no variable rather than opening on a path that is
/// not there and reporting an empty folder.
///
/// Order is preserved, because the first root is the one a file manager
/// cold-starts into.
pub fn push_workspace_roots(
    argv: &mut Vec<String>,
    capabilities: &CapabilityPlan,
) -> Option<String> {
    let raw = std::env::var("QOS_WORKSPACE_ROOTS").ok()?;
    let value = granted_workspace_roots(&raw, capabilities)?;
    argv.push("--setenv".to_string());
    argv.push("QOS_WORKSPACE_ROOTS".to_string());
    argv.push(value.clone());
    Some(value)
}

/// The roots of `raw` that `capabilities` has actually bound, in order.
///
/// Split out so the narrowing is testable without mutating the process
/// environment. `None` rather than an empty string when nothing survives:
/// "no workspace" and "a workspace with no roots" are different states.
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

/// If the manifest declared at least one portal as `ViaHost`, return
/// the current `$DBUS_SESSION_BUS_ADDRESS` — else `None` (bus stays
/// out of the sandbox).
pub fn host_dbus_bus_address_if_needed(
    portals: &std::collections::BTreeMap<String, PortalPolicy>,
) -> Option<String> {
    let wants_bus = portals
        .values()
        .any(|p| matches!(p, PortalPolicy::ViaHost));
    if !wants_bus {
        return None;
    }
    std::env::var("DBUS_SESSION_BUS_ADDRESS").ok()
}

/// Extract the `unix:path=<path>` filesystem path from a D-Bus bus
/// address (or None if the address has no `unix:path=` transport).
///
/// The address is a semicolon-separated list of transports; we return
/// the first `unix:path=` we see. Abstract-namespace sockets
/// (`unix:abstract=...`) can't be bind-mounted so we skip them —
/// callers surface that as graceful "portal unavailable".
pub fn parse_unix_path_bus_address(addr: &str) -> Option<String> {
    for entry in addr.split(';') {
        let Some(rest) = entry.strip_prefix("unix:") else {
            continue;
        };
        for kv in rest.split(',') {
            if let Some(path) = kv.strip_prefix("path=") {
                if !path.is_empty() {
                    return Some(path.to_string());
                }
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

    /// Naming an ungranted root would be worse than useless: the path is not
    /// in the namespace, so the app opens on a directory that is not there and
    /// reports an empty folder — indistinguishable, on screen, from a shared
    /// workspace that genuinely has nothing in it.
    #[test]
    fn an_ungranted_workspace_root_is_not_named_to_the_app() {
        let plan = CapabilityPlan::default();
        assert_eq!(granted_workspace_roots("/var/workspace", &plan), None);
    }

    #[test]
    fn a_granted_workspace_root_reaches_the_app() {
        let plan = CapabilityPlan {
            ro_paths: vec![PathBuf::from("/var/workspace")],
            ..CapabilityPlan::default()
        };
        assert_eq!(
            granted_workspace_roots("/var/workspace", &plan).as_deref(),
            Some("/var/workspace"),
        );
    }

    /// Narrowed per root, not all-or-nothing.
    #[test]
    fn only_the_granted_roots_cross() {
        let plan = CapabilityPlan {
            ro_paths: vec![PathBuf::from("/var/workspace")],
            rw_paths: vec![PathBuf::from("/var/tmp/qos/workspace")],
            ..CapabilityPlan::default()
        };
        assert_eq!(
            granted_workspace_roots(
                "/var/workspace:/srv/somebody-elses:/var/tmp/qos/workspace",
                &plan,
            )
            .as_deref(),
            Some("/var/workspace:/var/tmp/qos/workspace"),
            "a root nobody granted must be dropped while the granted ones stay",
        );
    }

    /// Matching is by path component, not string prefix: a grant on
    /// `/var/workspace` must not carry `/var/workspace-notes` across.
    #[test]
    fn a_sibling_directory_is_not_covered_by_the_grant() {
        let plan = CapabilityPlan {
            ro_paths: vec![PathBuf::from("/var/workspace")],
            ..CapabilityPlan::default()
        };
        assert_eq!(granted_workspace_roots("/var/workspace-notes", &plan), None);
    }

    fn mk_manifest(net_mode: &str) -> MinimalManifest {
        MinimalManifest {
            package_id: "com.example.notes".to_string(),
            name: Some("Notes".to_string()),
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
                // A manifest names no path. One that did would be
                // refused before it reached this builder; see
                // `manifest::enforce_fs_policy`.
                fs: crate::manifest::Fs::default(),
                services: BTreeMap::new(),
                portals: BTreeMap::new(),
                ..Default::default()
            },
            cli_commands: Vec::new(),
            opens_ext: Vec::new(),
        }
    }

    /// Build argv for `mk_manifest`'s package id with `policy` as the image's
    /// untrusted-parser list, so the tests exercise the real
    /// `build_bwrap_argv` → `hardening::for_package` path rather than calling
    /// `push_runtime_hardening` directly with a hand-made value.
    fn argv_with_hardening_policy(policy: &str) -> Vec<String> {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("untrusted-parsers.conf");
        std::fs::write(&file, policy).unwrap();
        // SAFETY: the test body is single-threaded and takes the process_env
        // lock; the variable is removed before the assertions run.
        unsafe { std::env::set_var(crate::hardening::POLICY_PATH_ENV, &file) };
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
        // SAFETY: as above.
        unsafe { std::env::remove_var(crate::hardening::POLICY_PATH_ENV) };
        argv
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_listed_package_gets_the_allocator_bound_as_well_as_preloaded() {
        let argv = argv_with_hardening_policy("com.example.notes\n");

        let preload = argv
            .windows(3)
            .find(|w| w[0] == "--setenv" && w[1] == "LD_PRELOAD")
            .map(|w| w[2].clone());

        // compiler-rt may not be installed where the tests run; when it is
        // absent the launcher must fall back to the system allocator rather
        // than set a preload that cannot resolve.
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

    #[test]
    #[serial_test::serial(process_env)]
    fn an_unlisted_package_pays_for_neither_protection() {
        let argv = argv_with_hardening_policy("dev.qos.browser\n");
        assert!(
            !argv.iter().any(|a| a == "LD_PRELOAD"),
            "an ordinary app must not carry the hardened allocator: {argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a == "GLIBC_TUNABLES"),
            "an ordinary app must not carry the shadow-stack tunable: {argv:?}"
        );
    }

    #[test]
    fn argv_includes_namespace_and_lifetime_flags() {
        let m = mk_manifest("off");
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None, &CapabilityPlan::empty());
        assert!(argv.iter().any(|a| a == "--unshare-all"));
        assert!(argv.iter().any(|a| a == "--die-with-parent"));
        // net.mode = "off" means no --share-net
        assert!(!argv.iter().any(|a| a == "--share-net"));
        // Standard mounts
        assert!(argv.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == "/tmp"));
        assert!(argv.windows(2).any(|w| w[0] == "--proc" && w[1] == "/proc"));
        assert!(argv.windows(2).any(|w| w[0] == "--dev" && w[1] == "/dev"));
    }

    #[test]
    fn wayland_bind_normalises_the_in_sandbox_display_name() {
        // Host is on wayland-4; the app must still see wayland-0.
        let bind = wayland_bind_for(Path::new("/run/user/1000/wayland-4"), 1000);
        assert_eq!(bind.display_name, "wayland-0");
        assert_eq!(bind.sandbox_socket, Path::new("/run/user/1000/wayland-0"));
        assert_eq!(bind.sandbox_runtime_dir, Path::new("/run/user/1000"));
        assert_eq!(bind.host_socket, Path::new("/run/user/1000/wayland-4"));
    }

    /// The sandbox must bind exactly one Wayland socket — the public one —
    /// and must not hand the app the privileged socket in any form.
    ///
    /// This is the enforcement behind the clipboard-manager gate in
    /// `shell/src/compositor/selection.rs`. `zwlr_data_control_manager_v1`
    /// exposes every selection on the seat with no focus check and no user
    /// interaction, and the apps in this repo copy TOTP codes, scanned Wi-Fi
    /// passwords and identity recovery phrases. The global is refused to
    /// clients on the public socket, and the reason that refusal cannot be
    /// worked around is right here: a sandboxed app has no filesystem path to
    /// the socket where the global exists.
    #[test]
    fn sandbox_binds_only_the_public_wayland_socket() {
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
            !argv.iter().any(|a| a.contains("qos-privileged")),
            "the privileged Wayland socket must never appear in a sandbox argv: {argv:?}"
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--unsetenv" && w[1] == PRIVILEGED_WAYLAND_ENV),
            "the privileged socket's env var must be cleared inside the sandbox: {argv:?}"
        );
    }

    #[test]
    fn wayland_bind_argv_carries_socket_and_env() {
        let mut argv = Vec::new();
        let bind = wayland_bind_for(Path::new("/run/user/1000/wayland-4"), 1000);
        push_wayland_bind(&mut argv, &bind);
        // ro-bind <host socket> /run/user/1000/wayland-0
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

    #[test]
    fn argv_shares_net_when_mode_not_off() {
        let m = mk_manifest("on");
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None, &CapabilityPlan::empty());
        assert!(argv.iter().any(|a| a == "--share-net"));
    }

    #[test]
    fn argv_binds_payload_root_to_qos_apps_path() {
        let m = mk_manifest("off");
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None, &CapabilityPlan::empty());
        // Find the --ro-bind triple for the payload.
        let pos = argv
            .iter()
            .position(|a| a == "/host/pkg")
            .expect("payload host path in argv");
        assert_eq!(argv[pos - 1], "--ro-bind");
        assert_eq!(argv[pos + 1], "/var/qos-apps/com.example.notes");
    }

    /// Value of the `--setenv <key> <value>` triple for `key`, or `None`
    /// when the builder emitted no such assignment.
    fn setenv_value<'a>(argv: &'a [String], key: &str) -> Option<&'a str> {
        argv.windows(3)
            .find(|w| w[0] == "--setenv" && w[1] == key)
            .map(|w| w[2].as_str())
    }

    #[test]
    fn argv_exports_the_app_name_from_the_manifest() {
        // The manifest names the app "Notes"; the toolkit reads
        // QOS_APP_NAME for the window title, so the builder must export
        // it alongside QOS_PACKAGE_ID.
        let m = mk_manifest("off");
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None, &CapabilityPlan::empty());
        assert_eq!(setenv_value(&argv, "QOS_APP_NAME"), Some("Notes"));
    }

    #[test]
    fn argv_omits_the_app_name_when_the_manifest_has_none() {
        // A manifest with no `[package].name` exports nothing — the
        // toolkit then derives a title from the package id.
        let mut m = mk_manifest("off");
        m.name = None;
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None, &CapabilityPlan::empty());
        assert_eq!(setenv_value(&argv, "QOS_APP_NAME"), None);
    }

    #[test]
    fn argv_exports_the_icon_path_under_the_payload_mount() {
        // A manifest that ships icon art names it payload-relative; the
        // builder resolves it to the in-sandbox payload mount so the
        // toolkit can open it.
        let mut m = mk_manifest("off");
        m.icon = Some("assets/icon.png".to_string());
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None, &CapabilityPlan::empty());
        assert_eq!(
            setenv_value(&argv, "QOS_APP_ICON"),
            Some("/var/qos-apps/com.example.notes/assets/icon.png"),
        );
    }

    #[test]
    fn argv_omits_the_icon_when_the_manifest_has_none() {
        // No icon art: no QOS_APP_ICON, so the toolkit draws its default.
        let m = mk_manifest("off");
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None, &CapabilityPlan::empty());
        assert_eq!(setenv_value(&argv, "QOS_APP_ICON"), None);
    }

    /// The reads a launch gets are the system profile's, and a manifest
    /// that names a path never reaches this builder at all — so the argv
    /// is the same whatever a package wrote in `[sandbox.fs]`.
    #[test]
    fn argv_reads_come_from_the_system_profile_not_the_manifest() {
        let m = mk_manifest("off");
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None, &CapabilityPlan::empty());
        for entry in crate::profile::SYSTEM_READ_PATHS {
            assert!(
                argv.windows(3)
                    .any(|w| w[0] == "--ro-bind-try" && w[1] == *entry && w[2] == *entry),
                "profile entry {entry} missing from argv: {argv:?}"
            );
        }
        // A manifest that still carries `fs` fields changes nothing here:
        // the builder does not read them. (Reaching this builder with
        // them set is itself refused upstream — this asserts the builder
        // has no second path into a bind.)
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
            argv.windows(3).any(|w| w[0] == "--ro-bind" && w[1] == "/host/pkg"),
            "payload bind must stay non-try: {argv:?}"
        );
        // Nothing writable comes out of this builder. The app's one
        // writable directory is spliced by `run`, from the room it was
        // launched into — not from anything the package said.
        assert!(
            !argv.iter().any(|a| a == "--bind" || a == "--bind-try"),
            "the manifest layer must produce no writable bind: {argv:?}"
        );
    }

    #[test]
    fn argv_includes_runtime_bind_and_ld_library_path() {
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
                w[0] == "--ro-bind" && w[1] == "/host/runtime" && w[2] == "/usr/lib/qos-runtime/1"
            })
            .expect("runtime ro-bind");
        let _ = rt_pos;
        // setenv LD_LIBRARY_PATH /usr/lib/qos-runtime/1/lib
        let ld_pos = argv
            .windows(3)
            .position(|w| w[0] == "--setenv" && w[1] == "LD_LIBRARY_PATH" && w[2] == "/usr/lib/qos-runtime/1/lib")
            .expect("LD_LIBRARY_PATH setenv");
        let _ = ld_pos;
    }

    #[test]
    fn argv_ends_with_entrypoint_path() {
        let m = mk_manifest("off");
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None, &CapabilityPlan::empty());
        assert_eq!(
            argv.last().map(String::as_str),
            Some("/var/qos-apps/com.example.notes/bin/notes")
        );
    }

    #[test]
    fn parse_unix_path_bus_address_extracts_path() {
        assert_eq!(
            parse_unix_path_bus_address("unix:path=/run/user/1000/bus"),
            Some("/run/user/1000/bus".to_string())
        );
        assert_eq!(
            parse_unix_path_bus_address(
                "unix:path=/run/user/1000/bus,guid=deadbeef"
            ),
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

    #[test]
    fn argv_binds_dbus_proxy_socket_when_plan_is_supplied() {
        let mut m = mk_manifest("off");
        m.sandbox.portals.insert(
            "notifications".to_string(),
            PortalPolicy::ViaHost,
        );
        let plan = DBusBindPlan {
            host_socket: PathBuf::from("/run/user/1234/qos/dbus-proxy/9999.sock"),
            sandbox_path: PathBuf::from("/run/user/1234/bus"),
            kind: DBusBindKind::Proxy,
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
        // Proxy socket bound at /run/user/1234/bus (NOT the raw bus
        // path — that's exactly what the proxy is protecting).
        assert!(
            argv.windows(3).any(|w| w[0] == "--ro-bind"
                && w[1] == "/run/user/1234/qos/dbus-proxy/9999.sock"
                && w[2] == "/run/user/1234/bus"),
            "proxy socket ro-bind missing from argv: {argv:?}"
        );
        // The sandbox sees the bus at /run/user/<uid>/bus, so the
        // env var reflects THAT (not the host path).
        assert!(
            argv.windows(3).any(|w| w[0] == "--setenv"
                && w[1] == "DBUS_SESSION_BUS_ADDRESS"
                && w[2] == "unix:path=/run/user/1234/bus"),
            "dbus setenv missing from argv: {argv:?}"
        );
    }

    #[test]
    fn argv_binds_dbus_directly_when_direct_plan_is_supplied() {
        // Direct-bind fallback: the manifest declared a portal but
        // the launcher couldn't spawn xdg-dbus-proxy, so it binds
        // the raw bus. Argv shape is identical to the proxy path
        // by design — only the `kind` field distinguishes.
        let mut m = mk_manifest("off");
        m.sandbox.portals.insert(
            "notifications".to_string(),
            PortalPolicy::ViaHost,
        );
        let plan = DBusBindPlan {
            host_socket: PathBuf::from("/run/user/1234/bus"),
            sandbox_path: PathBuf::from("/run/user/1234/bus"),
            kind: DBusBindKind::Direct,
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
        assert!(
            argv.windows(3).any(|w| w[0] == "--ro-bind"
                && w[1] == "/run/user/1234/bus"
                && w[2] == "/run/user/1234/bus"),
            "direct bus ro-bind missing from argv: {argv:?}"
        );
    }

    #[test]
    fn argv_does_not_bind_dbus_without_plan() {
        let m = mk_manifest("off"); // no portals declared
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None, &CapabilityPlan::empty());
        assert!(
            !argv.iter().any(|a| a == "DBUS_SESSION_BUS_ADDRESS"),
            "bus must not leak into sandbox without a plan: {argv:?}"
        );
    }

    #[test]
    fn argv_does_not_bind_dbus_when_only_deny_portals() {
        // With the plan-based API, `Deny`-only portals means the
        // caller passes `None` here — same as no portals at all.
        let mut m = mk_manifest("off");
        m.sandbox
            .portals
            .insert("screencast".to_string(), PortalPolicy::Deny);
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None, &CapabilityPlan::empty());
        assert!(
            !argv.iter().any(|a| a == "DBUS_SESSION_BUS_ADDRESS"),
            "bus must stay out when caller passes no plan: {argv:?}"
        );
    }

    #[test]
    fn workspace_id_is_setenv_before_the_entrypoint() {
        // bwrap reads everything after the entrypoint token as the
        // child's argv, so a `--setenv` spliced after it would silently
        // become a command-line argument to the app instead of an env
        // var — the app would start, and only collaboration would be
        // quietly dead. Pin the ordering.
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

    #[test]
    fn build_bwrap_argv_for_entry_uses_provided_bin() {
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
        // The last argv token is the entrypoint absolute path.
        assert_eq!(
            argv.last().map(String::as_str),
            Some("/var/qos-apps/com.example.notes/bin/notes-cli")
        );
    }

    #[test]
    fn identity_bind_emits_ro_bind_and_setenv() {
        let m = mk_manifest("off");
        let argv = build_bwrap_argv(
            &m,
            Path::new("/host/pkg"),
            None,
            &[],
            None,
            Some(Path::new("/home/alice/.config/qos/identity/primary.json")),
            &CapabilityPlan::empty(),
        );
        // --ro-bind /home/alice/.config/qos/identity/primary.json /run/qos/identity.json
        assert!(
            argv.windows(3).any(|w| w[0] == "--ro-bind"
                && w[1] == "/home/alice/.config/qos/identity/primary.json"
                && w[2] == SANDBOX_IDENTITY_PATH),
            "identity ro-bind missing: {argv:?}"
        );
        // --setenv QOS_USER_IDENTITY /run/qos/identity.json
        assert!(
            argv.windows(3).any(|w| w[0] == "--setenv"
                && w[1] == "QOS_USER_IDENTITY"
                && w[2] == SANDBOX_IDENTITY_PATH),
            "identity setenv missing: {argv:?}"
        );
    }

    // A read-only participant is handed no identity, so it holds no
    // signing key and cannot originate an authored change — the absence
    // *is* the mechanism. The same call with an authoring plan does bind
    // it, so the absence below is participation and not a missing path.
    #[test]
    fn a_read_only_participant_is_given_no_identity() {
        use crate::capabilities::Participation;

        let m = mk_manifest("off");
        let host = Path::new("/home/alice/.config/qos/identity/primary.json");
        let read_only = CapabilityPlan {
            participation: Participation::ReadOnly,
            ..CapabilityPlan::empty()
        };
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, Some(host), &read_only);
        assert!(
            !argv.iter().any(|a| a == "QOS_USER_IDENTITY"),
            "a read-only participant must not be told where an identity is: {argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a == SANDBOX_IDENTITY_PATH),
            "nor may the identity be bound into its namespace at all: {argv:?}"
        );
        // …and it is told which shape it is in, so chrome can present a
        // viewer rather than an editor whose saves vanish.
        assert!(
            argv.windows(3).any(
                |w| w[0] == "--setenv" && w[1] == "QOS_PARTICIPATION" && w[2] == "read-only"
            ),
            "the app must be able to see that it is a viewer: {argv:?}"
        );

        let authoring = CapabilityPlan {
            participation: Participation::Author,
            ..CapabilityPlan::empty()
        };
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, Some(host), &authoring);
        assert!(
            argv.windows(3).any(|w| w[0] == "--ro-bind"
                && w[1] == host.to_str().unwrap()
                && w[2] == SANDBOX_IDENTITY_PATH),
            "an authoring participant keeps its identity: {argv:?}"
        );
        assert!(
            argv.windows(3)
                .any(|w| w[0] == "--setenv" && w[1] == "QOS_PARTICIPATION" && w[2] == "author"),
            "and is told so: {argv:?}"
        );
    }

    // An app no workspace capability names keeps the launch it always had:
    // identity bound, and no participation claim made on its behalf.
    #[test]
    fn an_unscoped_launch_is_unchanged() {
        let m = mk_manifest("off");
        let host = Path::new("/home/alice/.config/qos/identity/primary.json");
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
                && w[1] == "QOS_USER_IDENTITY"
                && w[2] == SANDBOX_IDENTITY_PATH),
            "an unscoped launch keeps its identity: {argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a == "QOS_PARTICIPATION"),
            "and no participation is claimed on its behalf: {argv:?}"
        );
    }

    #[test]
    fn identity_bind_absent_when_host_path_is_none() {
        let m = mk_manifest("off");
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None, &CapabilityPlan::empty());
        assert!(
            !argv.iter().any(|a| a == "QOS_USER_IDENTITY"),
            "QOS_USER_IDENTITY must not appear without a host path: {argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a == SANDBOX_IDENTITY_PATH),
            "identity path must not appear without a host path: {argv:?}"
        );
    }

    /// A selection with both loaders resolved must (a) hide every GPU
    /// except the compositor's, (b) bind the manifests its variables
    /// name, and (c) set those variables.
    #[test]
    fn gpu_selection_narrows_the_device_and_pins_the_loaders() {
        let sel = crate::gpu::GpuSelection {
            node: PathBuf::from("/dev/dri/renderD128"),
            driver: Some("amdgpu".to_string()),
            egl_vendor_json: Some(PathBuf::from("/usr/share/glvnd/egl_vendor.d/50_mesa.json")),
            vulkan_icd_json: Some(PathBuf::from("/usr/share/vulkan/icd.d/radeon_icd.x86_64.json")),
            notes: Vec::new(),
        };
        let mut argv = Vec::new();
        push_gpu_selection(&mut argv, &sel, true);

        // The tmpfs must come before the node re-bind, or the bind is
        // the thing that gets hidden.
        let tmpfs = argv
            .windows(2)
            .position(|w| w[0] == "--tmpfs" && w[1] == "/dev/dri")
            .expect("device dir must be emptied: {argv:?}");
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

    /// An app the manifest gave no DRM access to gets no device mount —
    /// the GPU plumbing must not hand out access nobody asked for.
    #[test]
    fn gpu_selection_does_not_mount_a_device_the_manifest_withheld() {
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

    /// The device is in the namespace because the *system* put it there,
    /// so the narrowing must fire for every app — including one whose
    /// manifest says nothing, which is now every app. A user-granted
    /// node counts too; missing that would leave every other GPU visible
    /// to an app whose GPU access came from the user.
    #[test]
    fn drm_grant_detection_follows_the_profile_and_capability_grants() {
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

    #[test]
    fn service_binds_name_their_child_fd() {
        let m = mk_manifest("off");
        let binds = vec![ServiceBind {
            name: "collab".to_string(),
            host_fd: 7,
            child_fd: 17,
        }];
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &binds, None, None, &CapabilityPlan::empty());
        // --setenv QOS_SVC_COLLAB_FD 17 — the number the app reads. The
        // descriptor itself is dup2'd onto it in `run::place_service_fds`.
        assert!(
            argv.windows(3)
                .any(|w| w[0] == "--setenv" && w[1] == "QOS_SVC_COLLAB_FD" && w[2] == "17"),
            "the app must be told which descriptor its service is on: {argv:?}"
        );
    }

    /// bwrap's `--file FD DEST` reads FD to EOF and writes the bytes
    /// into a regular file. Pointed at a connected socket it blocks on
    /// a read the broker never answers, so bwrap never reaches `exec`
    /// and an app declaring `collab` does not start — while an app
    /// whose broker socket happens to be missing starts fine with
    /// collaboration off. Neither is visible from the app.
    ///
    /// The descriptor reaches the child by inheritance instead
    /// (`run::place_service_fds`). Nothing in the argv may hand a
    /// service socket to bwrap.
    #[test]
    fn service_sockets_are_never_handed_to_bwrap_as_files() {
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
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &binds, None, None, &CapabilityPlan::empty());
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
        // Nothing emits `--file` at all any more, so the strongest
        // available statement is the flat one: not on these fds, not on
        // any fd a later change might route through here.
        assert!(
            !argv.iter().any(|a| a == "--file"),
            "a service socket must never be handed to bwrap as a file: {argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a.ends_with("/qos/services/collab.sock")),
            "the service socket has no in-sandbox path any more; the app \
             gets a descriptor: {argv:?}",
        );
    }

    // -----------------------------------------------------------------
    // Capability enforcement.
    //
    // These are the tests that make "Always allow" mean something. They
    // assert on the argv the production builder emits — the exact string
    // list handed to `bwrap` — because that argv *is* the sandbox. A
    // path missing from it is a path the app cannot reach, whatever the
    // permissions UI says.
    // -----------------------------------------------------------------

    use collab_types::caps::{CapOverride, Expiry, Scope, Subject, Verb};

    /// Deterministic stand-in for the host's device nodes, so a camera
    /// assertion doesn't depend on whether the machine running the test
    /// owns a webcam. The *policy* — who gets a camera at all — is the
    /// production path either way.
    fn fake_device_paths(device: &str) -> Vec<PathBuf> {
        match device {
            "camera" => vec![PathBuf::from("/dev/video0")],
            "microphone" => vec![PathBuf::from("/dev/snd")],
            _ => Vec::new(),
        }
    }

    /// Build the argv the launcher would really produce for `app_id`,
    /// given the workspace's capability list. Goes through the same
    /// `CapabilityPlan::for_app_with` → `build_bwrap_argv` pair that
    /// `run_with` uses; only the device-node lookup is substituted.
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

    /// Everything a launch with no grants binds, enumerated. Written as
    /// an equality rather than a set of `any()` probes so that a new
    /// mount cannot be added to the default sandbox without this test
    /// naming it.
    fn baseline_binds() -> Vec<(String, String, String)> {
        let mut expected = vec![(
            "--ro-bind".to_string(),
            "/host/pkg".to_string(),
            "/var/qos-apps/com.example.notes".to_string(),
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
    fn no_capabilities_adds_no_binds_beyond_the_system_profile() {
        let baseline = argv_with_caps("com.example.notes", &[], 1_000);
        let m = mk_manifest("off");
        let manifest_only =
            build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None, &CapabilityPlan::empty());
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

    #[test]
    fn read_capability_binds_the_path_read_only_and_never_writable() {
        let caps = vec![fs_grant("dev.qos.books", "/var/workspace/books", Verb::Read)];
        let argv = argv_with_caps("dev.qos.books", &caps, 1_000);
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

    #[test]
    fn write_capability_binds_the_path_read_write() {
        let caps = vec![fs_grant("dev.qos.books", "/var/workspace/books", Verb::Write)];
        let argv = argv_with_caps("dev.qos.books", &caps, 1_000);
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

    #[test]
    fn camera_bind_appears_only_with_a_camera_capability() {
        let without = argv_with_caps("dev.qos.meet", &[], 1_000);
        assert!(
            !without.iter().any(|a| a.starts_with("/dev/video")),
            "no camera capability must mean no camera device in the namespace: {without:?}"
        );

        let caps = vec![CapOverride::grant(
            Subject::app("dev.qos.meet"),
            Scope::Device("camera".to_string()),
            Verb::Read,
        )];
        let with = argv_with_caps("dev.qos.meet", &caps, 1_000);
        assert!(
            with.windows(3).any(
                |w| w[0] == "--dev-bind-try" && w[1] == "/dev/video0" && w[2] == "/dev/video0"
            ),
            "a camera capability must bind the device node: {with:?}"
        );
    }

    #[test]
    fn a_capability_for_another_app_never_reaches_this_apps_argv() {
        let caps = vec![
            fs_grant("dev.qos.other", "/var/workspace/secrets", Verb::Write),
            CapOverride::grant(
                Subject::app("dev.qos.other"),
                Scope::Device("camera".to_string()),
                Verb::Read,
            ),
            CapOverride::grant(
                Subject::app("dev.qos.other"),
                Scope::NetHost("*".to_string()),
                Verb::Write,
            ),
        ];
        let argv = argv_with_caps("dev.qos.books", &caps, 1_000);
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
        let theirs = argv_with_caps("dev.qos.other", &caps, 1_000);
        assert!(theirs.iter().any(|a| a == "/var/workspace/secrets"));
        assert!(theirs.iter().any(|a| a == "--share-net"));
    }

    #[test]
    fn an_expired_capability_produces_no_bind() {
        let caps = vec![
            fs_grant("dev.qos.books", "/var/workspace/books", Verb::Write)
                .expiring(Expiry::AtUnix(500)),
        ];
        let expired = argv_with_caps("dev.qos.books", &caps, 1_000);
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
        let live = argv_with_caps("dev.qos.books", &caps, 499);
        assert!(
            live.windows(3).any(|w| w[0] == "--bind-try"
                && w[1] == "/var/workspace/books"
                && w[2] == "/var/workspace/books"),
            "the same capability evaluated before its deadline must bind: {live:?}"
        );
    }

    #[test]
    fn network_capability_is_what_shares_the_namespace() {
        // The manifest says net.mode = "off": without a capability the
        // sandbox keeps the empty network namespace `--unshare-all` gave
        // it, and no packet can leave.
        let isolated = argv_with_caps("dev.qos.mail", &[], 1_000);
        assert!(
            !isolated.iter().any(|a| a == "--share-net"),
            "no network capability must leave the netns unshared: {isolated:?}"
        );

        let caps = vec![CapOverride::grant(
            Subject::app("dev.qos.mail"),
            Scope::NetHost("*".to_string()),
            Verb::Write,
        )];
        let granted = argv_with_caps("dev.qos.mail", &caps, 1_000);
        assert!(
            granted.iter().any(|a| a == "--share-net"),
            "a grant for `*` is unrestricted access and shares the namespace: {granted:?}"
        );
    }

    /// This test used to assert the opposite, and the assertion was
    /// correct at the time: a grant naming a host produced `--share-net`
    /// and the host itself appeared nowhere, because bwrap has no
    /// per-host vocabulary and nothing else was reading the name. It
    /// read:
    ///
    /// ```text
    /// assert!(!argv.iter().any(|a| a.contains("github.com")),
    ///     "the recorded allowlist reaches no enforcement mechanism — it is absent from the \
    ///      argv that builds the sandbox: {argv:?}");
    /// ```
    ///
    /// Both halves of that are now false, and this asserts the
    /// replacement: a named host does **not** share the namespace, and
    /// it does reach a mechanism. The mechanism is deliberately still
    /// not an argv string — a hostname in a bwrap argument would mean
    /// nothing to bwrap. It is the gate socket, which is what the empty
    /// namespace leaves as the app's only way out, so the assertion
    /// here is about the shape of the sandbox and
    /// `tests/netgate_egress.rs` is about the packets that shape
    /// permits.
    #[test]
    fn a_grant_naming_a_host_no_longer_shares_the_whole_network() {
        let caps = vec![CapOverride::grant(
            Subject::app("dev.qos.mail"),
            Scope::NetHost("github.com".to_string()),
            Verb::Write,
        )];
        let argv = argv_with_caps("dev.qos.mail", &caps, 1_000);

        assert!(
            !argv.iter().any(|a| a == "--share-net"),
            "a grant naming one host must NOT hand the app the host's whole network — that \
             equivalence between `github.com` and `*` is the bug this closes: {argv:?}"
        );

        // And the grant does reach a mechanism now: the decision that
        // drives the gate says so, from the same manifest + capability
        // pair that built the argv above.
        let plan = CapabilityPlan::for_app_with(
            "dev.qos.mail",
            &caps,
            Some(1_000),
            |_| Vec::new(),
        );
        let decision = egress_decision(&mk_manifest("off"), &plan);
        let policy = decision
            .policy()
            .unwrap_or_else(|| panic!("a named host must resolve to a filtered egress policy, got {decision:?}"));
        assert!(policy.permits("github.com"));
        assert!(
            !policy.permits("evil.example.com"),
            "the allowlist must exclude what it does not name: {policy:?}"
        );
    }

    #[test]
    fn the_gate_bind_is_writable_and_points_the_app_at_the_relay() {
        let bind = NetGateBind {
            host_socket: PathBuf::from("/run/user/1000/qos/net/7.sock"),
            sandbox_socket: PathBuf::from(crate::netgate::SANDBOX_GATE_PATH),
            port: crate::netgate::RELAY_PORT,
        };
        let mut argv = Vec::new();
        push_net_gate(&mut argv, &bind);

        assert!(
            argv.windows(3).any(|w| w[0] == "--bind"
                && w[1] == "/run/user/1000/qos/net/7.sock"
                && w[2] == crate::netgate::SANDBOX_GATE_PATH),
            "the gate socket must be bound read-write: connect(2) on a pathname socket is a \
             write and a --ro-bind surfaces as EACCES: {argv:?}"
        );
        // socks5h, not socks5: the `h` is what sends the hostname to
        // the gate instead of resolving it locally first, and the
        // sandbox cannot resolve at all.
        let socks = format!("socks5h://127.0.0.1:{}", crate::netgate::RELAY_PORT);
        for var in ["ALL_PROXY", "all_proxy"] {
            assert!(
                argv.windows(3)
                    .any(|w| w[0] == "--setenv" && w[1] == var && w[2] == socks),
                "{var} must point at the relay as socks5h: {argv:?}"
            );
        }
        // And the HTTP form, which is the one that works without an
        // opt-in feature in the client. An app whose HTTP library was
        // built without SOCKS support reads only these, and would be
        // offline rather than filtered if they were missing.
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
