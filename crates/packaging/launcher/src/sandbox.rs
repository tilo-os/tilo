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

use crate::manifest::{MinimalManifest, PortalPolicy, expand_path};

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
/// The returned argv ends with the entrypoint command and its args.
pub fn build_bwrap_argv(
    manifest: &MinimalManifest,
    payload_root: &Path,
    runtime: Option<(&Path, &str)>,
    service_binds: &[ServiceBind],
    dbus_bind: Option<&DBusBindPlan>,
    identity_host_path: Option<&Path>,
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
) -> Vec<String> {
    let mut argv: Vec<String> = Vec::new();

    // Namespace + lifetime defaults.
    argv.push("--unshare-all".to_string());
    if manifest.sandbox.net.mode != "off" {
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

    // Standard "every app needs these" mounts. Come BEFORE the
    // manifest fs binds so that a device path (e.g. `/dev/dri`) declared
    // in `sandbox.fs.{read,write}` lands on top of the `--dev` tmpfs
    // instead of underneath it.
    argv.push("--tmpfs".to_string());
    argv.push("/tmp".to_string());
    argv.push("--proc".to_string());
    argv.push("/proc".to_string());
    argv.push("--dev".to_string());
    argv.push("/dev".to_string());

    // FS read/write rules from the manifest. Both source and dest use
    // the host-side expanded path — the app sees the same paths it
    // declared, so e.g. `~/Documents` resolves identically in and out of
    // the sandbox. Same convention as the per-workspace profile.
    // Device paths (anything under `/dev`) get `--dev-bind-try` instead
    // of --ro-bind/--bind so bwrap accepts the character-device inode
    // and silently skips paths that don't exist on the host.
    for entry in &manifest.sandbox.fs.read {
        let host = expand_path(entry);
        let flag = if host.starts_with("/dev") {
            "--dev-bind-try"
        } else {
            "--ro-bind"
        };
        push_bind(&mut argv, flag, &host, &host);
    }
    for entry in &manifest.sandbox.fs.write {
        let host = expand_path(entry);
        let flag = if host.starts_with("/dev") {
            "--dev-bind-try"
        } else {
            "--bind"
        };
        push_bind(&mut argv, flag, &host, &host);
    }

    // Per-workspace grants. When the manifest opts in with
    // `sandbox.workspace_grants = "auto"` and the compositor set
    // `QOS_WORKSPACE_ID`, mount every path the shell has recorded
    // as granted for this (workspace_id, package_id) pair. Same
    // host-side path is used for the sandbox path so the app sees
    // the same path it originally requested — that's the property
    // that lets a portal FileChooser "open" flow round-trip a
    // path across the sandbox boundary.
    for granted in resolve_workspace_grants(manifest) {
        let host = &granted.path;
        let flag = pick_bind_flag(host, granted.write);
        push_bind(&mut argv, flag, host, host);
    }

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

    // User identity: ro-bind the caller-supplied host path into the
    // sandbox and set QOS_USER_IDENTITY. The host path is a specific
    // file — we deliberately do NOT bind `~/.config/qos/` wholesale
    // so identities the user has for other purposes (secondary
    // devices, staged records) stay off the app's view.
    if let Some(host_path) = identity_host_path {
        argv.push("--ro-bind".to_string());
        argv.push(host_path.display().to_string());
        argv.push(SANDBOX_IDENTITY_PATH.to_string());
        argv.push("--setenv".to_string());
        argv.push("QOS_USER_IDENTITY".to_string());
        argv.push(SANDBOX_IDENTITY_PATH.to_string());
    }

    // Service fds: --file <host_fd> /run/qos/services/<name>.sock
    // (matches the well-known service path inside the sandbox).
    for sb in service_binds {
        let in_sandbox = sandbox_service_path(&sb.name);
        argv.push("--file".to_string());
        argv.push(sb.host_fd.to_string());
        argv.push(in_sandbox.display().to_string());
        // Expose the fd number to the app via env. We use the
        // child-side fd because the entrypoint reads
        // `QOS_SVC_<NAME>_FD` and either dup2s or fdopen()s it.
        argv.push("--setenv".to_string());
        argv.push(svc_env_var(&sb.name));
        argv.push(sb.child_fd.to_string());
    }

    // The entrypoint command, joined onto the in-sandbox payload mount.
    let entry_abs = payload_mount.join(entry_bin);
    argv.push(entry_abs.display().to_string());

    argv
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

/// The path inside the sandbox that the service socket fd is mounted at.
pub fn sandbox_service_path(name: &str) -> PathBuf {
    PathBuf::from("/run/qos/services").join(format!("{name}.sock"))
}

/// The env var name the entrypoint reads to find an opened service fd.
pub fn svc_env_var(name: &str) -> String {
    format!("QOS_SVC_{}_FD", name.to_uppercase())
}

fn push_bind(argv: &mut Vec<String>, kind: &str, src: &Path, dst: &Path) {
    argv.push(kind.to_string());
    argv.push(src.display().to_string());
    argv.push(dst.display().to_string());
}

/// A single resolved workspace-grant path, plus its read/write mode.
/// Exposed so `run.rs` can also feed it into the Landlock ruleset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedGrant {
    pub path: PathBuf,
    pub write: bool,
}

fn pick_bind_flag(path: &Path, write: bool) -> &'static str {
    if path.starts_with("/dev") {
        "--dev-bind-try"
    } else if write {
        "--bind"
    } else {
        "--ro-bind"
    }
}

/// Read the grant file the shell writes at
/// `$XDG_STATE_HOME/qos-shell/grants/<QOS_WORKSPACE_ID>.toml` and
/// return every path it authorises for this package.
///
/// Silently returns an empty vec when
///   * the manifest doesn't opt in (`workspace_grants` != "auto"),
///   * `QOS_WORKSPACE_ID` isn't set (running outside a workspace),
///   * the grant file is missing or unreadable.
///
/// This is intentionally best-effort: a torn grant file must not
/// prevent an app from launching — the launcher's manifest binds
/// are still applied and the app is still functional, just without
/// its previously-granted paths. The user is asked again on next
/// portal call.
pub fn resolve_workspace_grants(manifest: &MinimalManifest) -> Vec<ResolvedGrant> {
    let Some(mode) = manifest.sandbox.workspace_grants.as_deref() else {
        return Vec::new();
    };
    if mode != "auto" {
        return Vec::new();
    }
    let Some(ws) = std::env::var_os("QOS_WORKSPACE_ID") else {
        return Vec::new();
    };
    let ws = ws.to_string_lossy().to_string();
    if ws.is_empty() {
        return Vec::new();
    }
    let Ok(dir) = workspace_grants_dir() else {
        return Vec::new();
    };
    let file = dir.join(format!("{}.toml", sanitise_id(&ws)));
    let Ok(text) = std::fs::read_to_string(&file) else {
        return Vec::new();
    };
    let Ok(raw) = toml::from_str::<RawGrantFile>(&text) else {
        return Vec::new();
    };
    raw.entries
        .into_iter()
        .filter(|e| e.pkg_id == manifest.package_id)
        .filter(|e| e.grant.path.is_absolute())
        .map(|e| ResolvedGrant {
            path: e.grant.path,
            write: matches!(e.grant.mode.as_deref(), Some("write")),
        })
        .collect()
}

/// Public wrapper so an external caller (a test or `run.rs`
/// building the Landlock ro/rw list) can turn one raw entry into
/// its host-side bind flag string. Small enough to sit next to
/// [`resolve_workspace_grants`].
pub fn workspace_grant_bind_flag(grant: &ResolvedGrant) -> &'static str {
    pick_bind_flag(&grant.path, grant.write)
}

#[derive(Debug, serde::Deserialize)]
struct RawGrantFile {
    #[serde(default, rename = "entry")]
    entries: Vec<RawGrantEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct RawGrantEntry {
    pkg_id: String,
    grant: RawGrant,
}

#[derive(Debug, serde::Deserialize)]
struct RawGrant {
    path: PathBuf,
    #[serde(default)]
    mode: Option<String>,
}

fn sanitise_id(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '-' => c,
            _ => '_',
        })
        .collect()
}

fn workspace_grants_dir() -> Result<PathBuf, ()> {
    if let Some(dir) = std::env::var_os("QOS_SHELL_STATE_DIR") {
        return Ok(PathBuf::from(dir).join("grants"));
    }
    if let Some(xdg) = std::env::var_os("XDG_STATE_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join("qos-shell").join("grants"));
        }
    }
    let home = std::env::var_os("HOME").ok_or(())?;
    Ok(Path::new(&home)
        .join(".local/state")
        .join("qos-shell")
        .join("grants"))
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

    fn mk_manifest(net_mode: &str) -> MinimalManifest {
        MinimalManifest {
            package_id: "com.example.notes".to_string(),
            version: "0.1.0".to_string(),
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
                },
                fs: crate::manifest::Fs {
                    read: vec!["/usr/share/fonts".to_string()],
                    write: vec!["/var/lib/qos/notes".to_string()],
                },
                services: BTreeMap::new(),
                portals: BTreeMap::new(),
                workspace_grants: None,
            },
            cli_commands: Vec::new(),
        }
    }

    #[test]
    fn argv_includes_namespace_and_lifetime_flags() {
        let m = mk_manifest("off");
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None);
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
    fn argv_shares_net_when_mode_not_off() {
        let m = mk_manifest("on");
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None);
        assert!(argv.iter().any(|a| a == "--share-net"));
    }

    #[test]
    fn argv_binds_payload_root_to_qos_apps_path() {
        let m = mk_manifest("off");
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None);
        // Find the --ro-bind triple for the payload.
        let pos = argv
            .iter()
            .position(|a| a == "/host/pkg")
            .expect("payload host path in argv");
        assert_eq!(argv[pos - 1], "--ro-bind");
        assert_eq!(argv[pos + 1], "/var/qos-apps/com.example.notes");
    }

    #[test]
    fn argv_includes_fs_read_and_write_binds() {
        let m = mk_manifest("off");
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None);
        // --ro-bind /usr/share/fonts /usr/share/fonts
        let ro_pos = argv
            .windows(3)
            .position(|w| w[0] == "--ro-bind" && w[1] == "/usr/share/fonts" && w[2] == "/usr/share/fonts")
            .expect("fs.read ro-bind");
        let _ = ro_pos;
        let rw_pos = argv
            .windows(3)
            .position(|w| w[0] == "--bind" && w[1] == "/var/lib/qos/notes" && w[2] == "/var/lib/qos/notes")
            .expect("fs.write bind");
        let _ = rw_pos;
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
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None);
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
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None);
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
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None);
        assert!(
            !argv.iter().any(|a| a == "DBUS_SESSION_BUS_ADDRESS"),
            "bus must stay out when caller passes no plan: {argv:?}"
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

    #[test]
    fn identity_bind_absent_when_host_path_is_none() {
        let m = mk_manifest("off");
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None);
        assert!(
            !argv.iter().any(|a| a == "QOS_USER_IDENTITY"),
            "QOS_USER_IDENTITY must not appear without a host path: {argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a == SANDBOX_IDENTITY_PATH),
            "identity path must not appear without a host path: {argv:?}"
        );
    }

    #[test]
    fn service_binds_emit_file_and_setenv() {
        let m = mk_manifest("off");
        let binds = vec![ServiceBind {
            name: "collab".to_string(),
            host_fd: 7,
            child_fd: 17,
        }];
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &binds, None, None);
        // --file 7 /run/qos/services/collab.sock
        let file_pos = argv
            .windows(3)
            .position(|w| w[0] == "--file" && w[1] == "7" && w[2] == "/run/qos/services/collab.sock")
            .expect("service --file triple");
        let _ = file_pos;
        // --setenv QOS_SVC_COLLAB_FD 17
        let env_pos = argv
            .windows(3)
            .position(|w| w[0] == "--setenv" && w[1] == "QOS_SVC_COLLAB_FD" && w[2] == "17")
            .expect("service env var");
        let _ = env_pos;
    }

    #[test]
    fn resolve_workspace_grants_ignored_when_manifest_does_not_opt_in() {
        let m = mk_manifest("off");
        // sandbox.workspace_grants is None — no lookup happens.
        assert!(resolve_workspace_grants(&m).is_empty());
    }

    #[test]
    fn resolve_workspace_grants_reads_shell_state_file() {
        let tmp = tempfile::tempdir().unwrap();
        let grants_dir = tmp.path().join("grants");
        std::fs::create_dir_all(&grants_dir).unwrap();
        // Two entries — one for our pkg, one for a different pkg.
        // Only the matching one should surface.
        let file = grants_dir.join("ws-a.toml");
        std::fs::write(
            &file,
            "[[entry]]\n\
             pkg_id = 'com.example.notes'\n\
             [entry.grant]\n\
             path = '/home/alice/Documents'\n\
             mode = 'read'\n\
             [[entry]]\n\
             pkg_id = 'com.example.other'\n\
             [entry.grant]\n\
             path = '/home/alice/Music'\n\
             mode = 'write'\n",
        )
        .unwrap();

        let mut m = mk_manifest("off");
        m.sandbox.workspace_grants = Some("auto".to_string());

        // Point the sandbox helper at our tempdir + fake workspace.
        // SAFETY: single-thread env mutation for this test only.
        unsafe {
            std::env::set_var("QOS_SHELL_STATE_DIR", tmp.path());
            std::env::set_var("QOS_WORKSPACE_ID", "ws-a");
        }
        let out = resolve_workspace_grants(&m);
        unsafe {
            std::env::remove_var("QOS_SHELL_STATE_DIR");
            std::env::remove_var("QOS_WORKSPACE_ID");
        }
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, PathBuf::from("/home/alice/Documents"));
        assert!(!out[0].write);
    }

    #[test]
    fn build_bwrap_argv_binds_workspace_grants() {
        let tmp = tempfile::tempdir().unwrap();
        let grants_dir = tmp.path().join("grants");
        std::fs::create_dir_all(&grants_dir).unwrap();
        std::fs::write(
            grants_dir.join("ws-b.toml"),
            "[[entry]]\n\
             pkg_id = 'com.example.notes'\n\
             [entry.grant]\n\
             path = '/home/alice/Pictures'\n\
             mode = 'read'\n\
             [[entry]]\n\
             pkg_id = 'com.example.notes'\n\
             [entry.grant]\n\
             path = '/home/alice/Videos'\n\
             mode = 'write'\n",
        )
        .unwrap();
        let mut m = mk_manifest("off");
        m.sandbox.workspace_grants = Some("auto".to_string());
        unsafe {
            std::env::set_var("QOS_SHELL_STATE_DIR", tmp.path());
            std::env::set_var("QOS_WORKSPACE_ID", "ws-b");
        }
        let argv =
            build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None, None);
        unsafe {
            std::env::remove_var("QOS_SHELL_STATE_DIR");
            std::env::remove_var("QOS_WORKSPACE_ID");
        }
        // Read grant becomes --ro-bind ...
        assert!(
            argv.windows(3).any(|w| w[0] == "--ro-bind"
                && w[1] == "/home/alice/Pictures"
                && w[2] == "/home/alice/Pictures"),
            "expected ro-bind of Pictures in argv: {argv:?}"
        );
        // Write grant becomes --bind ...
        assert!(
            argv.windows(3).any(|w| w[0] == "--bind"
                && w[1] == "/home/alice/Videos"
                && w[2] == "/home/alice/Videos"),
            "expected --bind of Videos in argv: {argv:?}"
        );
    }
}
