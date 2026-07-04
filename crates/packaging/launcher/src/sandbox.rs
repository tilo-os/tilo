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
/// points. Kept in sync with `runtime_bundle::HOST_MINIMAL_PATHS`.
pub const CANONICAL_LOADER_PATHS: &[&str] = &[
    "/lib64/ld-linux-x86-64.so.2",
    "/lib/ld-linux-x86-64.so.2",
    "/lib/ld-linux.so.2",
    "/usr/lib64/ld-linux-x86-64.so.2",
    "/lib64/libc.so.6",
    "/lib64/libgcc_s.so.1",
    "/lib64/libm.so.6",
    "/lib64/libdl.so.2",
    "/lib64/libpthread.so.0",
    "/lib64/librt.so.1",
    "/lib64/libutil.so.1",
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
/// The returned argv ends with the entrypoint command and its args.
pub fn build_bwrap_argv(
    manifest: &MinimalManifest,
    payload_root: &Path,
    runtime: Option<(&Path, &str)>,
    service_binds: &[ServiceBind],
    dbus_bind: Option<&DBusBindPlan>,
) -> Vec<String> {
    // GUI launch: pick the GUI-flavour bin. Falls back to the
    // legacy top-level `bin` when neither split is set (i.e. old
    // manifest with `type` unset).
    let entry = manifest
        .entrypoint
        .gui_bin()
        .unwrap_or(&manifest.entrypoint.bin);
    build_bwrap_argv_for_entry(manifest, payload_root, runtime, service_binds, dbus_bind, entry)
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

    // FS read/write rules from the manifest. Both source and dest use
    // the host-side expanded path — the app sees the same paths it
    // declared, so e.g. `~/Documents` resolves identically in and out of
    // the sandbox. Same convention as the per-workspace profile.
    for entry in &manifest.sandbox.fs.read {
        let host = expand_path(entry);
        push_bind(&mut argv, "--ro-bind", &host, &host);
    }
    for entry in &manifest.sandbox.fs.write {
        let host = expand_path(entry);
        push_bind(&mut argv, "--bind", &host, &host);
    }

    // Standard "every app needs these" mounts.
    argv.push("--tmpfs".to_string());
    argv.push("/tmp".to_string());
    argv.push("--proc".to_string());
    argv.push("/proc".to_string());
    argv.push("--dev".to_string());
    argv.push("/dev".to_string());

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
            },
            cli_commands: Vec::new(),
        }
    }

    #[test]
    fn argv_includes_namespace_and_lifetime_flags() {
        let m = mk_manifest("off");
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None);
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
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None);
        assert!(argv.iter().any(|a| a == "--share-net"));
    }

    #[test]
    fn argv_binds_payload_root_to_qos_apps_path() {
        let m = mk_manifest("off");
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None);
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
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None);
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
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None);
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
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None);
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
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &[], None);
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
            "bin/notes-cli",
        );
        // The last argv token is the entrypoint absolute path.
        assert_eq!(
            argv.last().map(String::as_str),
            Some("/var/qos-apps/com.example.notes/bin/notes-cli")
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
        let argv = build_bwrap_argv(&m, Path::new("/host/pkg"), None, &binds, None);
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
}
