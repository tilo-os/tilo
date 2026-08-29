//! Top-level `run` orchestration, matching the CLI sequence: read the
//! manifest; resolve service sockets (per `--no-services`); stage the
//! seccomp blob to a tempfile; build the bwrap argv; spawn `bwrap` with
//! `pre_exec` installing Landlock (best-effort) and the seccomp fd; wait
//! and relay the exit code.

use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use crate::capabilities::{CapabilityPlan, load_capability_file, now_unix};
use crate::dbus_proxy::{self, DBusProxyGuard};
use crate::manifest::{MinimalManifest, PortalPolicy, read_manifest};
use crate::sandbox::{DBusBindKind, DBusBindPlan, build_bwrap_argv};
use crate::seccomp::compile_baseline;
use crate::services::{
    EXIT_REQUIRED_SERVICE_MISSING, ServiceError, resolve_service_binds, to_service_binds,
};

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Manifest(#[from] crate::manifest::ManifestError),
    #[error("refusing to launch: {0}")]
    FsPolicy(#[from] crate::manifest::FsPolicyViolation),
    #[error(
        "refusing to launch `{package}`: could not create its directory at {path} ({source}). \
         An app with nowhere to write loses the user's work on exit, so this is a refusal \
         rather than a launch into a read-only sandbox."
    )]
    AppDir {
        package: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "refusing to launch `{0}`: neither XDG_DATA_HOME nor HOME is set, so there is no \
         user data directory to put this app's per-room state in."
    )]
    NoDataHome(String),
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error(transparent)]
    Seccomp(#[from] crate::seccomp::SeccompError),
    #[error("spawning bwrap: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error(
        "manifest declares runtime `{runtime}` but no bundle was found at any of: {paths:?}. \
         Install it with `qos-install runtime install --version <v> --from-host`, \
         or pass --allow-missing-runtime to fall back to --minimal-libs / no runtime binds."
    )]
    RuntimeBundleMissing {
        runtime: String,
        paths: Vec<PathBuf>,
    },
    #[error("manifest declares runtime `{0}` but the value is not in `<name>/<version>` form")]
    RuntimeReferenceMalformed(String),
    #[error(
        "runtime bundle at {path} is unsigned — refuse to launch. Pass \
         --allow-unsigned-runtime for bring-up / debugging."
    )]
    RuntimeBundleUnsigned { path: PathBuf },
    #[error(
        "runtime bundle at {path} failed signature check: {reason}. Rebuild the \
         bundle with a trusted publisher or pass --allow-unsigned-runtime."
    )]
    RuntimeBundleSignature { path: PathBuf, reason: String },
    #[error(
        "`{desktop_id}` is a host application, not a qOS package: it has no manifest, so \
         there is no sandbox to build for it. Launch it with `qos-launcher run-host \
         --desktop-id {desktop_id}`, which runs it unconfined and says so."
    )]
    HostAppNotAPackage { desktop_id: String },
    #[error(
        "refusing to launch: this app was granted access to {hosts:?} and that can only be \
         enforced by brokering its traffic, but the egress gate could not start ({source}). \
         Launching anyway would mean giving it unrestricted outbound access, which is not \
         what was granted. Set QOS_LAUNCHER_NET_GATE=off to do that deliberately."
    )]
    EgressGate {
        hosts: Vec<String>,
        #[source]
        source: crate::netgate::GateError,
    },
}

impl RunError {
    /// Translate to a process exit code; required-service-missing gets its
    /// own code, distinct from a generic launch failure.
    pub fn to_exit_code(&self) -> i32 {
        match self {
            RunError::Service(ServiceError::RequiredMissing { .. }) => {
                EXIT_REQUIRED_SERVICE_MISSING
            }
            _ => 1,
        }
    }
}

/// Optional knobs for `run_with`.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Skip all service-socket resolution.
    pub no_services: bool,
    /// Explicit override for the qos-runtime bundle directory; wins over
    /// the manifest's `runtime` field.
    pub runtime_root: Option<PathBuf>,
    /// Override the bwrap binary (defaults to `bwrap`).
    pub bwrap_bin: Option<PathBuf>,
    /// Bind-mount the minimum host paths a dynamically-linked ELF needs to
    /// `execve()`, read-only. Ignored (silently) when a runtime bundle resolves.
    pub minimal_libs: bool,
    /// Route exec through the `__sandbox-exec` shim so Landlock installs
    /// inside bwrap's mount namespace. Requires this binary bind-mounted at
    /// [`SHIM_MOUNT_PATH`]. Default on; off is a logged debug escape hatch.
    pub landlock_enforce: bool,
    /// Launch mode — GUI (default) or CLI. CLI picks `cli_bin()` (falling
    /// back to `bin`), forwards argv, and inherits stdio instead of a Wayland surface.
    pub mode: LaunchMode,
    /// Debug escape hatch: when a runtime is declared but no bundle resolves,
    /// fall back to `minimal_libs` instead of aborting. Default off.
    pub allow_missing_runtime: bool,
    /// Accept an unsigned runtime bundle. Default refuse — the OS image only
    /// ships signed bundles; this is a bring-up/debug escape hatch.
    pub allow_unsigned_runtime: bool,
    /// Require the bundle's `publisher_hex` to equal this. Used by
    /// policy-strict callers and tests.
    pub require_runtime_publisher: Option<String>,
    /// Host path of the Wayland socket, bound into the sandbox at
    /// `/run/user/<uid>/wayland-0`. `None` means no display — never falls
    /// back to the launcher's own `$WAYLAND_DISPLAY`.
    pub wayland_socket: Option<PathBuf>,
    /// The compositor's DRM render node. A client must render on the same
    /// GPU as the compositor or buffer imports fail or cost a copy; taken
    /// from the compositor's own `zwp_linux_dmabuf_v1` decision rather than
    /// re-derived. `None` means loaders keep their own defaults.
    pub render_node: Option<PathBuf>,
    /// Host path to the user identity JSON; `None` resolves the
    /// `$XDG_CONFIG_HOME`/`$HOME` default. Ro-bound with `QOS_USER_IDENTITY` set.
    pub identity_host_path: Option<PathBuf>,
    /// Workspace this launch belongs to — the CRDT cluster id
    /// ([`crate::sandbox::WORKSPACE_ID_ENV`]). `None` means no session.
    pub workspace_id: Option<String>,
    /// Host path of the compositor-written `Vec<CapOverride>` JSON. `None`,
    /// missing, unreadable or malformed all mean the manifest's sandbox as-is.
    pub capabilities_path: Option<PathBuf>,
    /// The app id capabilities are addressed to (defaults to the manifest's
    /// package id); re-checked per capability rather than trusted.
    pub app_id: Option<String>,
    /// A document to open, forwarded as `argv[1]`. Only an argument — binds
    /// and unlocks nothing on its own. `None` launches with no arguments.
    pub open_document: Option<PathBuf>,
}

/// Resolve the capability plan: read the compositor's file, evaluate it
/// for this app now. Split out so [`run_with_captured`] and tests share it.
pub fn resolve_capabilities(manifest: &MinimalManifest, opts: &RunOptions) -> CapabilityPlan {
    let Some(path) = opts.capabilities_path.as_deref() else {
        return CapabilityPlan::empty();
    };
    let app_id = opts
        .app_id
        .clone()
        .unwrap_or_else(|| manifest.package_id.clone());
    let records = load_capability_file(path);
    let plan = CapabilityPlan::for_app(&app_id, &records, Some(now_unix()));
    tracing::info!(
        target: "qos_launcher::capabilities",
        app = %app_id,
        ro = plan.ro_paths.len(),
        rw = plan.rw_paths.len(),
        devices = ?plan.devices,
        network = plan.allow_network,
        "workspace capabilities resolved",
    );
    plan
}

/// Priority: explicit override > manifest declaration (per-user, then
/// system) > no bind. `Ok(None)` (no declaration) is distinct from `Err`.
pub fn resolve_runtime_test_helper(
    manifest: &crate::manifest::MinimalManifest,
    opts: &RunOptions,
) -> Result<Option<(PathBuf, String)>, RunError> {
    resolve_runtime(manifest, opts)
}

/// Resolve/create/migrate the one directory this launch may write to,
/// keyed by `opts.workspace_id` — no list to walk.
fn resolve_app_dir(manifest: &MinimalManifest, opts: &RunOptions) -> Result<PathBuf, RunError> {
    let data_home = crate::appdir::host_data_home()
        .ok_or_else(|| RunError::NoDataHome(manifest.package_id.clone()))?;
    crate::appdir::prepare_host_app_dir(
        &data_home,
        opts.workspace_id.as_deref(),
        &manifest.package_id,
    )
    .map_err(|source| RunError::AppDir {
        package: manifest.package_id.clone(),
        path: crate::appdir::host_app_dir(
            &data_home,
            &crate::appdir::room_slug(opts.workspace_id.as_deref()),
            &manifest.package_id,
        ),
        source,
    })
}

/// Priority: explicit override (no existence check) >
/// `$XDG_CONFIG_HOME/qos/identity/primary.json` >
/// `$HOME/.config/qos/identity/primary.json` > `None`.
pub fn resolve_identity_host_path(opts: &RunOptions) -> Option<PathBuf> {
    if let Some(p) = &opts.identity_host_path {
        return Some(p.clone());
    }
    let file_name = "primary.json";
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        let p = PathBuf::from(xdg)
            .join("qos")
            .join("identity")
            .join(file_name);
        if p.exists() {
            return Some(p);
        }
    }
    if let Some(home) = std::env::var_os("HOME")
        && !home.is_empty()
    {
        let p = PathBuf::from(home)
            .join(".config")
            .join("qos")
            .join("identity")
            .join(file_name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Derive GPU pinning, or `None` when nothing to pin. Every decline is
/// logged — landing on the wrong GPU must never happen silently.
pub fn resolve_gpu_selection(
    manifest: &crate::manifest::MinimalManifest,
    opts: &RunOptions,
) -> Option<crate::gpu::GpuSelection> {
    let node = opts.render_node.as_ref()?;
    let selection = crate::gpu::select_for_node(node, &crate::gpu::IcdSearch::default());
    for note in &selection.notes {
        tracing::warn!(target: "qos_launcher::gpu", package = %manifest.package_id, "{note}");
    }
    tracing::info!(
        target: "qos_launcher::gpu",
        package = %manifest.package_id,
        node = %selection.node.display(),
        driver = selection.driver.as_deref().unwrap_or("<unknown>"),
        egl = selection.egl_vendor_json.as_ref().map_or_else(|| "<loader default>".into(), |p| p.display().to_string()),
        vulkan = selection.vulkan_icd_json.as_ref().map_or_else(|| "<loader default>".into(), |p| p.display().to_string()),
        "pinning the app to the compositor's render node",
    );
    Some(selection)
}

pub fn resolve_runtime(
    manifest: &crate::manifest::MinimalManifest,
    opts: &RunOptions,
) -> Result<Option<(PathBuf, String)>, RunError> {
    // 1. Explicit override wins, verified against the same signature policy
    //    as the manifest path.
    if let Some(explicit) = &opts.runtime_root {
        let version = manifest
            .runtime_ref()
            .map_or_else(|| "1".to_string(), |(_, v)| v.to_string());
        enforce_bundle_policy(explicit, opts)?;
        return Ok(Some((explicit.clone(), version)));
    }
    // 2. Manifest declaration.
    let Some(raw) = manifest.runtime.as_deref() else {
        return Ok(None);
    };
    let (_, version) = raw
        .split_once('/')
        .ok_or_else(|| RunError::RuntimeReferenceMalformed(raw.to_string()))?;
    // Look up: per-user first, then system.
    let per_user = runtime_bundle::default_bundle_path(version).ok();
    let system = runtime_bundle::system_bundle_path(version);
    let candidates: Vec<PathBuf> = per_user.iter().cloned().chain([system]).collect();
    for cand in &candidates {
        if cand.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME).exists() {
            enforce_bundle_policy(cand, opts)?;
            return Ok(Some((cand.clone(), version.to_string())));
        }
    }
    if opts.allow_missing_runtime {
        tracing::warn!(
            "manifest declares runtime `{raw}` but no bundle found; \
             continuing with --minimal-libs={} (allow_missing_runtime=on)",
            opts.minimal_libs
        );
        return Ok(None);
    }
    Err(RunError::RuntimeBundleMissing {
        runtime: raw.to_string(),
        paths: candidates,
    })
}

/// Verify the bundle at `path` against the signature policy. Unless
/// `allow_unsigned_runtime`, it must be signed. Publisher policy:
///   1. `opts.require_runtime_publisher`, if set, must match.
///   2. Else the system trust-list file, if present and non-empty
///      (`/etc/qos/runtime-bundle-trusted-publishers.txt`, or
///      `QOS_LAUNCHER_TRUSTED_PUBLISHERS_FILE`).
///   3. Else any signed publisher is accepted.
/// Layering lets the OS image bake and pin a fresh key at build time.
fn enforce_bundle_policy(path: &Path, opts: &RunOptions) -> Result<(), RunError> {
    let require_publisher = match opts.require_runtime_publisher.clone() {
        Some(explicit) => Some(explicit),
        None => trusted_publisher_pin_from_file(),
    };
    let verify_opts = runtime_bundle::VerifyOptions {
        require_publisher,
        require_signature: !opts.allow_unsigned_runtime,
        require_arch: None,
    };
    match runtime_bundle::verify_bundle_with(path, &verify_opts) {
        Ok(manifest) => {
            // Multi-publisher trust list: `require_publisher` above was `None`
            // (only one key can be pinned there), so check by hand here.
            if let Some(list) = load_trusted_publisher_list()
                && !list.is_empty()
            {
                let ok = manifest
                    .publisher_hex
                    .as_deref()
                    .is_some_and(|pk| list.iter().any(|t| t.eq_ignore_ascii_case(pk)));
                if !ok {
                    return Err(RunError::RuntimeBundleSignature {
                        path: path.to_path_buf(),
                        reason: format!(
                            "publisher {:?} not in trust list",
                            manifest.publisher_hex.as_deref().unwrap_or("<none>"),
                        ),
                    });
                }
            }
            Ok(())
        }
        Err(runtime_bundle::BundleError::NotSigned) => Err(RunError::RuntimeBundleUnsigned {
            path: path.to_path_buf(),
        }),
        Err(e) => Err(RunError::RuntimeBundleSignature {
            path: path.to_path_buf(),
            reason: e.to_string(),
        }),
    }
}

/// A single-entry trust list is the pin handed to
/// `verify_bundle_with`; multi-entry lists are checked after it returns.
fn trusted_publisher_pin_from_file() -> Option<String> {
    let list = load_trusted_publisher_list()?;
    if list.len() == 1 {
        Some(list.into_iter().next().unwrap())
    } else {
        None
    }
}

/// Read the trusted-publisher list from disk, ignoring blank/`#` lines.
/// `None` when neither the env override nor the default file exists.
fn load_trusted_publisher_list() -> Option<Vec<String>> {
    let path: PathBuf = match std::env::var_os("QOS_LAUNCHER_TRUSTED_PUBLISHERS_FILE") {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from("/etc/qos/runtime-bundle-trusted-publishers.txt"),
    };
    let bytes = std::fs::read(&path).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let list: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(std::string::ToString::to_string)
        .collect();
    Some(list)
}

/// Top-level entry: reads the manifest, builds the sandbox, execs.
/// Does not call `process::exit` — that's `main.rs`'s job.
pub fn run(
    manifest_path: &Path,
    payload_root: &Path,
    opts: &RunOptions,
) -> Result<ExitStatus, RunError> {
    let manifest = read_manifest(manifest_path)?;
    run_with(&manifest, payload_root, opts)
}

/// Launch mode — which of the entrypoint binaries we're targeting
/// and what extra args to forward.
#[derive(Debug, Clone, Default)]
pub enum LaunchMode {
    /// The default: GUI-flavour bin, used implicitly by `run`/`run_with`.
    #[default]
    Gui,
    /// CLI-flavour bin. `passthrough_argv` follows `--` on the shim command
    /// line, forwarded verbatim; `command` names the manifest entry resolved.
    Cli {
        command: String,
        passthrough_argv: Vec<String>,
    },
    /// An application qOS did not package (a host `.desktop` entry, Flatpak
    /// or otherwise).
    ///
    /// **This mode never reaches the sandbox** — `run_with` refuses it with
    /// [`RunError::HostAppNotAPackage`]; [`crate::host`] spawns the entry's
    /// command line directly, with a controlled environment and no bubblewrap.
    ///
    /// Deliberate: the baseline seccomp filter denies the `unshare`/`mount`/
    /// `pivot_root` a Flatpak needs, and a host binary has no manifest to
    /// sandbox from — so this is a hard error here, not a mystery failure later.
    Host {
        /// Desktop-file id of the entry, e.g. `com.spotify.Client`.
        desktop_id: String,
    },
}

/// Env override that turns the egress gate off, downgrading a host-scoped
/// grant to unrestricted access. Explicit only — never a silent default.
pub const NET_GATE_ENV: &str = "QOS_LAUNCHER_NET_GATE";

/// Start the egress gate if this launch needs one. Returns the bwrap bind
/// description and the guard keeping the gate alive; both `None` when
/// there's nothing to filter.
///
/// ## Degradation
///
/// Needs no privilege and no kernel feature beyond what `bwrap` already
/// requires — unlike `nftables`-in-a-netns, which needs `CAP_NET_ADMIN`
/// for a `veth` pair or a userspace TCP/IP stack. The pieces here are
/// ordinary: the unshared netns every app gets from `--unshare-all`, one
/// bind-mounted `AF_UNIX` socket (same primitive as Wayland/D-Bus), and a
/// process binding an ordinary socket under `$XDG_RUNTIME_DIR`. So "kernel
/// doesn't support this" can't arise, and "we lack privilege" can't arise
/// either — no step is privileged. What can fail is binding the socket:
/// a missing, full or unwritable runtime directory.
///
/// **On that failure we refuse the launch** rather than fall back to
/// `--share-net`, which would silently turn "this host" into "every
/// host" — the false assurance this code exists to remove. Cost is
/// bounded: only a *narrowing* grant needs the gate. `QOS_LAUNCHER_NET_GATE=off`
/// takes the unfiltered path deliberately, logged, so it's on the record.
///
/// Landlock is the one kernel-dependent piece and is deliberately *not*
/// held to this standard — defence in depth behind the gate, not the
/// boundary; without ABI 4 the port pin does nothing and the empty
/// namespace + gate still hold.
fn start_egress_gate(
    egress: &crate::netgate::EgressDecision,
) -> Result<
    (
        Option<crate::sandbox::NetGateBind>,
        Option<crate::netgate::NetGate>,
    ),
    RunError,
> {
    // `Filtered` is the only decision needing a gate; `QOS_LAUNCHER_NET_GATE=off`
    // is handled in `crate::sandbox::egress_decision` so turning it off also
    // shares the network rather than sealing the app in an empty namespace.
    let Some(policy) = egress.policy() else {
        return Ok((None, None));
    };

    let host_socket = crate::netgate::gate_socket_path(current_uid(), std::process::id());
    match crate::netgate::NetGate::start(&host_socket, policy.clone()) {
        Ok(gate) => {
            let bind = crate::sandbox::NetGateBind {
                host_socket,
                sandbox_socket: PathBuf::from(crate::netgate::SANDBOX_GATE_PATH),
                port: crate::netgate::RELAY_PORT,
            };
            Ok((Some(bind), Some(gate)))
        }
        Err(source) => Err(RunError::EgressGate {
            hosts: policy.hosts().to_vec(),
            source,
        }),
    }
}

/// Put every opened service socket on the fd the app expects, in the
/// child, before `exec`. bwrap forwards fds it wasn't asked about
/// untouched, so a placement here survives namespace setup.
///
/// Not bwrap's `--file FD DEST`: that copies the descriptor's *contents*
/// into a regular file, so a connected socket blocks reading for an EOF
/// that never comes — the launch hangs before `exec` with no symptom.
///
/// Two passes: target numbers start at 17 and a source can already sit on
/// an unwritten target, so every source is relocated above the target
/// window first, so no `dup2` clobbers one still waiting to be placed.
///
/// Errors rather than continuing quietly: a failure here is a bug, not
/// an absent service, and collaboration silently starting off is exactly
/// the outcome worth refusing.
///
/// # Safety
/// Call only from `Command::pre_exec` (post-fork, pre-exec). Uses only
/// `fcntl`, `dup2` and `close`, all async-signal-safe; allocates nothing.
unsafe fn place_service_fds(pairs: &mut [(RawFd, RawFd)]) -> std::io::Result<()> {
    let Some(highest) = pairs.iter().map(|(_, dst)| *dst).max() else {
        return Ok(());
    };
    let floor = highest + 1;
    for (src, _) in pairs.iter_mut() {
        // F_DUPFD returns the lowest free fd >= floor, i.e. clear of
        // every target.
        let moved = unsafe { libc::fcntl(*src, libc::F_DUPFD, floor) };
        if moved >= 0 {
            // The original number is done with — a second live descriptor at an
            // unnamed number is one the app cannot close.
            unsafe { libc::close(*src) };
            *src = moved;
        }
    }
    for (src, dst) in pairs.iter() {
        if unsafe { libc::dup2(*src, *dst) } == -1 {
            return Err(std::io::Error::last_os_error());
        }
        // dup2 leaves the target without FD_CLOEXEC — it must survive bwrap's and
        // the app's exec. Cleared again explicitly since relocation above is
        // best-effort.
        unsafe {
            let flags = libc::fcntl(*dst, libc::F_GETFD);
            if flags != -1 {
                let _ = libc::fcntl(*dst, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
        }
    }
    // Close relocated copies now that every target holds one, so the app gets
    // exactly one descriptor per service. Skip numbers that are themselves a
    // target — closing one could shut a descriptor `dup2` just placed there.
    for (src, dst) in pairs.iter() {
        if *src != *dst && !pairs.iter().any(|(_, other)| other == src) {
            unsafe { libc::close(*src) };
        }
    }
    Ok(())
}

pub fn run_with(
    manifest: &MinimalManifest,
    payload_root: &Path,
    opts: &RunOptions,
) -> Result<ExitStatus, RunError> {
    // 0a. A host application has no manifest and no sandbox. Refusing here,
    // before any socket/directory/argv exists, keeps the two launch kinds
    // from quietly converging.
    if let LaunchMode::Host { desktop_id } = &opts.mode {
        return Err(RunError::HostAppNotAPackage {
            desktop_id: desktop_id.clone(),
        });
    }

    // 0. Policy: a manifest naming a filesystem path is refused before
    // anything else exists — silently dropping it would surface as an app
    // bug, not a policy one. Checked at launch too, not just install.
    crate::manifest::enforce_fs_policy(manifest)?;

    // 1. Service sockets.
    let resolved = resolve_service_binds(&manifest.sandbox.services, opts.no_services)?;
    let binds = to_service_binds(&resolved);

    // 2. Seccomp baseline → tmpfile → fd. The manifest *asks* for a profile;
    // the image decides whether this package may have it. An unrecognised
    // name is a packaging mistake; a recognised-but-unentitled one is an app
    // trying to lift a protection off itself — logged distinctly.
    let decision = crate::seccomp::resolve_profile(&manifest.sandbox.seccomp, &manifest.package_id);
    let profile = decision.applied;
    if decision.refused {
        eprintln!(
            "qos-launcher: {} asked for seccomp profile {:?}, which the image does not \
             entitle it to; applying {} (write-xor-execute enforced). Entitlement is \
             decided by {}, not by the package.",
            manifest.package_id,
            decision.requested,
            profile.as_str(),
            crate::seccomp::JIT_POLICY_PATH,
        );
    } else if profile.as_str() != decision.requested {
        eprintln!(
            "qos-launcher: manifest asked for seccomp profile {:?}, which is not one we know; \
             applying {} (write-xor-execute enforced)",
            decision.requested,
            profile.as_str()
        );
    }
    let seccomp_blob = compile_baseline(profile)?;
    let seccomp_fd: OwnedFd = stage_seccomp_blob(&seccomp_blob)?;
    const SECCOMP_CHILD_FD: i32 = 16;

    // 2b. D-Bus proxy / direct-bind decision: try xdg-dbus-proxy, fall back
    // to a raw-path bind on failure. `_dbus_guard` stays alive until bwrap
    // is waited on so Drop can SIGTERM the proxy.
    let (dbus_plan, _dbus_guard) = resolve_dbus_bind(&manifest.sandbox.portals);

    // 3. Argv. Runtime bundle resolution: a resolved bundle wins over
    // --minimal-libs and skips the loader-bind stopgap entirely.
    let resolved_runtime = resolve_runtime(manifest, opts)?;
    let runtime_arg = resolved_runtime
        .as_ref()
        .map(|(p, v)| (p.as_path(), v.as_str()));

    // The app's one writable directory, for the room it's opened into.
    // Created here since bwrap can only bind what exists.
    let app_dir = resolve_app_dir(manifest, opts)?;

    // Identity resolution, threaded through both bwrap-argv builders so the
    // child gets `QOS_USER_IDENTITY` pointing at the sandbox-side path.
    let identity_host = resolve_identity_host_path(opts);
    let identity_arg = identity_host.as_deref();

    // The user's capability decision. Empty unless the compositor wrote a
    // file — an app launched without one is confined to its manifest alone.
    let capabilities = resolve_capabilities(manifest, opts);

    // Egress: a grant naming specific hosts runs a gate; `*` or bare `host`
    // mode shares the host network with no allowlist to enforce, so neither
    // needs a gate. Guard bound here (not in the `if let`) so it outlives the child.
    let egress = crate::sandbox::egress_decision(manifest, &capabilities);
    let (net_gate, _gate_guard) = start_egress_gate(&egress)?;

    // Pick the entrypoint bin per launch mode: GUI uses `bin`; CLI uses
    // `cli_bin()` (falling back to `bin`) and splices QOS_CLI_COMMAND /
    // forwards passthrough argv.
    let entry_bin_owned: String;
    let (mut argv, extra_setenv, cli_passthrough) = match &opts.mode {
        LaunchMode::Gui => (
            build_bwrap_argv(
                manifest,
                payload_root,
                runtime_arg,
                &binds,
                dbus_plan.as_ref(),
                identity_arg,
                &capabilities,
            ),
            None,
            // A GUI launch carries at most one argument — the document to open —
            // riding the same tail as CLI passthrough (opaque bytes after entrypoint).
            opts.open_document
                .as_ref()
                .map(|p| vec![p.display().to_string()])
                .unwrap_or_default(),
        ),
        LaunchMode::Cli {
            command,
            passthrough_argv,
        } => {
            entry_bin_owned = manifest
                .entrypoint
                .cli_bin()
                .unwrap_or(&manifest.entrypoint.bin)
                .to_string();
            let v = crate::sandbox::build_bwrap_argv_for_entry(
                manifest,
                payload_root,
                runtime_arg,
                &binds,
                dbus_plan.as_ref(),
                identity_arg,
                &entry_bin_owned,
                &capabilities,
            );
            (
                v,
                Some(("QOS_CLI_COMMAND".to_string(), command.clone())),
                passthrough_argv.clone(),
            )
        }
        // Refused at the top of this function, before any of the setup
        // above ran. Reaching here would mean that guard was removed.
        LaunchMode::Host { desktop_id } => unreachable!(
            "host application {desktop_id} reached the sandbox builder; \
             the LaunchMode::Host guard at the top of run_with is gone"
        ),
    };
    // Splice in the QOS_CLI_COMMAND setenv before the trailing
    // entrypoint token so bwrap still parses it as a flag.
    if let Some((k, v)) = extra_setenv {
        let entry = argv.pop().expect("argv non-empty");
        argv.push("--setenv".to_string());
        argv.push(k);
        argv.push(v);
        argv.push(entry);
    }

    // Narrow dynamic-loader binds only (not all of /usr), for a plain
    // dynamically-linked execve(). Skipped when a runtime bundle (which
    // already carries the loader) is mounted.
    let mut extra_ro_paths: Vec<String> = Vec::new();
    let use_minimal_libs = opts.minimal_libs && resolved_runtime.is_none();
    if opts.minimal_libs && resolved_runtime.is_some() {
        tracing::info!(
            "runtime bundle mounted; ignoring --minimal-libs (loader + libc come from the bundle)"
        );
    }
    if use_minimal_libs {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        for path in minimal_loader_binds() {
            if std::path::Path::new(path).exists() {
                argv.push("--ro-bind".to_string());
                argv.push(path.to_string());
                argv.push(path.to_string());
                extra_ro_paths.push((*path).to_string());
            }
        }
        argv.push(entry);
    }

    // Workspace id, spliced before the entrypoint token so the child learns
    // which room it was opened into. Absent when the launch has no session.
    if let Some(workspace_id) = opts.workspace_id.as_deref() {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        crate::sandbox::push_workspace_id(&mut argv, workspace_id);
        argv.push(entry);
    }

    // App directory: bound read-write at a fixed sandbox path, HOME/XDG vars
    // pointed inside it. Spliced here (not in `build_bwrap_argv`) since the
    // room is a launch property, not the package's.
    {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        crate::appdir::push_app_dir(&mut argv, &app_dir);
        // After `push_app_dir` (which repoints `HOME`): this says where the
        // *user's* home is, only when a grant actually bound it.
        if let Some(user_home) = crate::sandbox::push_user_home(&mut argv, &capabilities) {
            tracing::debug!(
                user_home = %user_home.display(),
                "a granted bind covers the user's home; telling the app where it is"
            );
        }
        // And where the room's shared mounts are, on the same terms: a
        // workspace the app cannot read is not one it should be told about.
        if let Some(roots) = crate::sandbox::push_workspace_roots(&mut argv, &capabilities) {
            tracing::debug!(
                workspace_roots = %roots,
                "granted binds cover workspace roots; telling the app where they are"
            );
        }
        argv.push(entry);
    }

    // Wayland display, spliced before the entrypoint token like the
    // minimal-libs binds. No `wayland_socket` means no display at all.
    let wayland_bind = opts
        .wayland_socket
        .as_ref()
        .map(|host| crate::sandbox::wayland_bind_for(host, current_uid()));
    if let Some(bind) = &wayland_bind {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        crate::sandbox::push_wayland_bind(&mut argv, bind);
        argv.push(entry);
    }

    // Egress gate socket, spliced like Wayland — the app's only route out of
    // an otherwise empty network namespace.
    if let Some(bind) = &net_gate {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        crate::sandbox::push_net_gate(&mut argv, bind);
        argv.push(entry);
    }

    // GPU: the compositor's render node decides EGL/Vulkan visibility,
    // spliced after the manifest's own `/dev/dri` grant to narrow it.
    let gpu = resolve_gpu_selection(manifest, opts);
    if let Some(sel) = &gpu {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        crate::sandbox::push_gpu_selection(
            &mut argv,
            sel,
            crate::sandbox::grants_drm(manifest, &capabilities),
        );
        argv.push(entry);
    }

    // In-sandbox shim: bind this launcher binary in and rewrite the trailing
    // entrypoint token into `<shim> __sandbox-exec [rules…] -- <entry>`.
    // Two jobs justify it alone: apply Landlock after bwrap's mount namespace
    // is up, and start the egress relay before the app's first connect.
    let need_shim = opts.landlock_enforce || net_gate.is_some();
    if need_shim {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        let launcher = std::env::current_exe()
            .map_err(|e| RunError::Io(std::io::Error::other(format!("current_exe: {e}"))))?;
        argv.push("--ro-bind".to_string());
        argv.push(launcher.display().to_string());
        argv.push(SHIM_MOUNT_PATH.to_string());
        argv.push(SHIM_MOUNT_PATH.to_string());
        argv.push(crate::landlock_shim::SANDBOX_EXEC_SUBCOMMAND.to_string());
        if !opts.landlock_enforce {
            argv.push("--no-landlock".to_string());
        }
        // The relay's endpoints. Passed whenever a gate is running, so
        // an unmodified HTTP client honouring ALL_PROXY reaches it.
        if let Some(bind) = &net_gate {
            argv.push("--net-gate".to_string());
            argv.push(bind.sandbox_socket.display().to_string());
            argv.push("--net-gate-port".to_string());
            argv.push(bind.port.to_string());
        }
        // Sandbox-side ro paths for Landlock: the payload mount, the runtime
        // mount (if any), minimal-libs binds, and the manifest's read paths.
        let payload_mount = crate::sandbox::payload_mount_for(&manifest.package_id);
        argv.push("--ro".to_string());
        argv.push(payload_mount.display().to_string());
        if let Some((rt_root, ver)) = runtime_arg {
            let rt = crate::sandbox::runtime_mount_for(ver);
            argv.push("--ro".to_string());
            argv.push(rt.display().to_string());
            // Also allow the canonical loader paths re-bound out of the bundle —
            // otherwise the entry ELF's INTERP is blocked by Landlock despite the bind.
            for canonical in crate::sandbox::CANONICAL_LOADER_PATHS {
                let no_slash = canonical.trim_start_matches('/');
                if rt_root.join("lib").join(no_slash).exists() {
                    argv.push("--ro".to_string());
                    argv.push((*canonical).to_string());
                }
            }
        }
        for p in &extra_ro_paths {
            argv.push("--ro".to_string());
            argv.push(p.clone());
        }
        // System read profile, derived from the same list the binds came from —
        // an unnamed bind is visible but returns EACCES on every open, which
        // looks like a corrupted system rather than a policy.
        for path in crate::profile::landlock_ro_paths(egress.shares_host_network()) {
            argv.push("--ro".to_string());
            argv.push(path.display().to_string());
        }
        // Capability-granted paths need matching Landlock rules from the same
        // plan, or the bind is visible but every open() returns EACCES.
        for path in &capabilities.ro_paths {
            argv.push("--ro".to_string());
            argv.push(path.display().to_string());
        }
        for path in &capabilities.rw_paths {
            argv.push("--rw".to_string());
            argv.push(path.display().to_string());
        }
        // A bound D-Bus socket also needs a Landlock read grant on the
        // sandbox-side path, or ashpd's connect() surfaces EACCES.
        if let Some(plan) = &dbus_plan {
            argv.push("--ro".to_string());
            argv.push(plan.sandbox_path.display().to_string());
        }
        // Same for the ro-bound identity file: without a grant, `load()` gets
        // EACCES and the feature quietly no-ops. A read-only participant has no
        // identity bound.
        if identity_arg.is_some() && capabilities.participation.carries_identity() {
            argv.push("--ro".to_string());
            argv.push(crate::sandbox::SANDBOX_IDENTITY_PATH.to_string());
        }
        // Wayland needs read+write in Landlock: connect(2) on a unix socket is
        // write-flavoured, so read-only surfaces as EACCES at `wl_display_connect`.
        if let Some(bind) = &wayland_bind {
            argv.push("--rw".to_string());
            argv.push(bind.sandbox_socket.display().to_string());
        }
        // GPU selection is only real if Landlock agrees: loader manifests need
        // read, the render node needs read+write (opened O_RDWR) — read-only
        // looks like no GPU present.
        if let Some(sel) = &gpu {
            for file in sel.manifest_files() {
                argv.push("--ro".to_string());
                argv.push(file.display().to_string());
            }
            argv.push("--rw".to_string());
            argv.push(sel.node.display().to_string());
        }
        // The app's own directory, named by its in-sandbox path — the host path
        // isn't reachable from inside, so a rule on it would match nothing.
        argv.push("--rw".to_string());
        argv.push(crate::appdir::SANDBOX_APP_DIR.to_string());
        // The GPU device nodes the profile bound — same O_RDWR reasoning
        // as above.
        for path in crate::profile::landlock_rw_device_paths() {
            argv.push("--rw".to_string());
            argv.push(path.display().to_string());
        }
        // Standard tmpfs mounts the app expects writable; --rw grants full FS
        // access, and both vanish with the process (scratch, not hidden state).
        for p in ["/tmp", "/dev"] {
            argv.push("--rw".to_string());
            argv.push(p.to_string());
        }
        // Service sockets need no Landlock rule: they arrive as already-connected
        // descriptors dup2'd in, never mounted or opened by path.
        //
        // The gate socket is reached via connect(2) (write-flavoured), so it
        // needs the same read+write rule as Wayland above.
        if let Some(bind) = &net_gate {
            argv.push("--rw".to_string());
            argv.push(bind.sandbox_socket.display().to_string());
        }
        // Landlock's network restriction mirrors the namespace decision: Blocked
        // denies all TCP on an already-empty netns; Filtered pins the app to the
        // relay port instead.
        if matches!(egress, crate::netgate::EgressDecision::Blocked) {
            argv.push("--net-restricted".to_string());
        }
        argv.push("--".to_string());
        argv.push(entry);
    }

    // Insert --seccomp <child_fd> at the front so bwrap installs it
    // before any other action (matches the compositor pattern).
    let mut prefix = vec!["--seccomp".to_string(), SECCOMP_CHILD_FD.to_string()];
    prefix.append(&mut argv);
    argv = prefix;

    // Passthrough: everything after the entrypoint token is opaque to bwrap,
    // forwarded as the sandboxed program's argv[1..].
    argv.extend(cli_passthrough);

    // 4. Build the bwrap Command.
    let bwrap = opts
        .bwrap_bin
        .clone()
        .unwrap_or_else(|| PathBuf::from("bwrap"));
    let mut cmd = Command::new(&bwrap);
    cmd.args(&argv);
    clear_bwrap_environment(&mut cmd);

    // pre_exec: dup the seccomp blob fd into SECCOMP_CHILD_FD, and put
    // each service socket on the fd number the app was told to read.
    let host_seccomp_fd = seccomp_fd.as_raw_fd();
    let mut service_fds: Vec<(RawFd, RawFd)> = resolved
        .iter()
        .map(|r| (r.host_fd(), r.child_fd as RawFd))
        .collect();
    for r in &resolved {
        tracing::info!(
            service = %r.name,
            child_fd = r.child_fd,
            "service socket connected; handing the descriptor to the app",
        );
    }
    // Landlock plan: derived here so we can log it from the parent.
    let landlock_plan = crate::landlock::plan_from_paths(
        &crate::profile::landlock_ro_paths(egress.shares_host_network()),
        &[PathBuf::from(crate::appdir::SANDBOX_APP_DIR)],
    );
    let net_restricted = matches!(egress, crate::netgate::EgressDecision::Blocked);
    // The shim enforces Landlock via opts.landlock_enforce (argv splice
    // above); this just logs intent — the shim's own line has the real outcome.
    tracing::info!(
        target: "qos_launcher::landlock",
        rules = landlock_plan.len(),
        net_restricted,
        enforce = opts.landlock_enforce,
        "landlock plan derived",
    );
    tracing::info!(
        target: "qos_launcher::netgate",
        gated = net_gate.is_some(),
        hosts = ?egress.policy().map(super::netgate::EgressPolicy::hosts),
        shares_host_network = egress.shares_host_network(),
        "egress resolved",
    );
    let _ = landlock_plan;

    // SAFETY: pre_exec runs in the child after fork, before exec. We
    // perform only async-signal-safe operations: dup2, fcntl, plus the
    // landlock crate's syscall wrappers. No allocator calls, no locks.
    unsafe {
        cmd.pre_exec(move || {
            // Plumb seccomp blob fd → fixed child fd, strip CLOEXEC.
            if libc::dup2(host_seccomp_fd, SECCOMP_CHILD_FD) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            let flags = libc::fcntl(SECCOMP_CHILD_FD, libc::F_GETFD);
            if flags != -1 {
                let _ = libc::fcntl(SECCOMP_CHILD_FD, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
            place_service_fds(&mut service_fds)?;
            // Landlock is NOT installed here — bwrap needs broad FS access to set up
            // the namespace; the `__sandbox-exec` shim installs it after. See `landlock_shim`.
            Ok(())
        });
    }

    // 5. Spawn + wait.
    let mut child = cmd.spawn().map_err(RunError::Spawn)?;
    // Important: keep `seccomp_fd` alive until after `spawn` returns,
    // so its dup2'd copy in the child remains valid. Drop here.
    drop(seccomp_fd);
    // Same rule: the child holds its own copies, so the launcher's are dead
    // weight, dropped before the wait so the far end learns of the exit.
    drop(resolved);
    let status = child.wait()?;
    Ok(status)
}

/// `run_with` variant that captures the child's stdout. Returns the
/// exit status alongside the captured bytes. Used by tests.
pub fn run_with_captured(
    manifest: &MinimalManifest,
    payload_root: &Path,
    opts: &RunOptions,
) -> Result<(ExitStatus, Vec<u8>), RunError> {
    // Same refusals as `run_with`: this is a second entry into the sandbox
    // builder, and a policy with one enforced door is a door beside it.
    if let LaunchMode::Host { desktop_id } = &opts.mode {
        return Err(RunError::HostAppNotAPackage {
            desktop_id: desktop_id.clone(),
        });
    }
    crate::manifest::enforce_fs_policy(manifest)?;

    // Service sockets.
    let resolved = resolve_service_binds(&manifest.sandbox.services, opts.no_services)?;
    let binds = to_service_binds(&resolved);

    // Seccomp baseline resolved identically to `run_with` — two paths
    // reaching different conclusions would make capture a way around the filter.
    let seccomp_blob = compile_baseline(manifest.sandbox.seccomp_profile(&manifest.package_id))?;
    let seccomp_fd: OwnedFd = stage_seccomp_blob(&seccomp_blob)?;
    const SECCOMP_CHILD_FD: i32 = 16;

    // Argv. run_with_captured is a lighter sibling that skips Landlock +
    // minimal-libs, but still honours a manifest-declared runtime bundle.
    let resolved_runtime = resolve_runtime(manifest, opts)?;
    let runtime_arg = resolved_runtime
        .as_ref()
        .map(|(p, v)| (p.as_path(), v.as_str()));
    // D-Bus plan (same decision path as `run_with`).
    let (dbus_plan, _dbus_guard) = resolve_dbus_bind(&manifest.sandbox.portals);
    let identity_host = resolve_identity_host_path(opts);
    let identity_arg = identity_host.as_deref();
    let capabilities = resolve_capabilities(manifest, opts);
    // Same egress decision as `run_with`: without the gate spliced in here
    // too, a host-scoped grant would be silently offline. No shim on this
    // path, so no loopback relay — ALL_PROXY has nothing listening behind it.
    let egress = crate::sandbox::egress_decision(manifest, &capabilities);
    let (net_gate, _gate_guard) = start_egress_gate(&egress)?;
    // Entrypoint + trailing arguments, resolved the same way `run_with`
    // does — both entries must agree on binary and arguments.
    let (entry_bin, passthrough): (String, Vec<String>) = match &opts.mode {
        LaunchMode::Gui => (
            manifest
                .entrypoint
                .gui_bin()
                .unwrap_or(&manifest.entrypoint.bin)
                .to_string(),
            opts.open_document
                .as_ref()
                .map(|p| vec![p.display().to_string()])
                .unwrap_or_default(),
        ),
        LaunchMode::Cli {
            passthrough_argv, ..
        } => (
            manifest
                .entrypoint
                .cli_bin()
                .unwrap_or(&manifest.entrypoint.bin)
                .to_string(),
            passthrough_argv.clone(),
        ),
        // Refused at the top of this function. See `run_with`.
        LaunchMode::Host { desktop_id } => unreachable!(
            "host application {desktop_id} reached the sandbox builder; \
             the LaunchMode::Host guard at the top of run_with_captured is gone"
        ),
    };
    let mut argv = crate::sandbox::build_bwrap_argv_for_entry(
        manifest,
        payload_root,
        runtime_arg,
        &binds,
        dbus_plan.as_ref(),
        identity_arg,
        &entry_bin,
        &capabilities,
    );
    if let Some(bind) = &net_gate {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        crate::sandbox::push_net_gate(&mut argv, bind);
        argv.push(entry);
    }
    if let Some(workspace_id) = opts.workspace_id.as_deref() {
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        crate::sandbox::push_workspace_id(&mut argv, workspace_id);
        argv.push(entry);
    }
    {
        let app_dir = resolve_app_dir(manifest, opts)?;
        let entry = argv.pop().expect("argv non-empty after build_bwrap_argv");
        crate::appdir::push_app_dir(&mut argv, &app_dir);
        argv.push(entry);
    }
    let mut prefix = vec!["--seccomp".to_string(), SECCOMP_CHILD_FD.to_string()];
    prefix.append(&mut argv);
    argv = prefix;
    // Everything after the entrypoint token is opaque to bwrap and
    // becomes the child's argv[1..].
    argv.extend(passthrough);

    let bwrap = opts
        .bwrap_bin
        .clone()
        .unwrap_or_else(|| PathBuf::from("bwrap"));
    let mut cmd = Command::new(&bwrap);
    cmd.args(&argv);
    clear_bwrap_environment(&mut cmd);
    cmd.stdout(std::process::Stdio::piped());

    let host_seccomp_fd = seccomp_fd.as_raw_fd();
    let mut service_fds: Vec<(RawFd, RawFd)> = resolved
        .iter()
        .map(|r| (r.host_fd(), r.child_fd as RawFd))
        .collect();
    unsafe {
        cmd.pre_exec(move || {
            if libc::dup2(host_seccomp_fd, SECCOMP_CHILD_FD) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            let flags = libc::fcntl(SECCOMP_CHILD_FD, libc::F_GETFD);
            if flags != -1 {
                let _ = libc::fcntl(SECCOMP_CHILD_FD, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
            place_service_fds(&mut service_fds)?;
            Ok(())
        });
    }

    let mut child = cmd.spawn().map_err(RunError::Spawn)?;
    drop(seccomp_fd);
    // As in `run_with`: the child has its own copies, so holding these open
    // only keeps the service from seeing the app end.
    drop(resolved);
    let mut buf = Vec::new();
    if let Some(mut so) = child.stdout.take() {
        use std::io::Read;
        so.read_to_end(&mut buf)?;
    }
    let status = child.wait()?;
    Ok((status, buf))
}

/// Give `bwrap` itself an environment worth inheriting, so a dropped
/// `--clearenv` cannot silently become a leak.
///
/// The argv already carries `--clearenv`
/// ([`crate::sandbox::push_clean_environment`]), deciding what the *app*
/// sees. This is the same statement made where the process is spawned:
/// bwrap is handed nothing to pass on, so a lost flag fails closed with
/// an empty environment instead of handing over the compositor's.
///
/// `PATH` survives because spawn needs it: `bwrap_bin` defaults to the
/// bare name `bwrap`, resolved against the child's own
/// [`crate::sandbox::SANDBOX_PATH`], not the session's.
fn clear_bwrap_environment(cmd: &mut Command) {
    cmd.env_clear();
    cmd.env("PATH", crate::sandbox::SANDBOX_PATH);
}

/// Write the seccomp blob to a `memfd` so its fd outlives the parent's
/// tempdir teardown and the bwrap exec; falls back to a real tempfile.
fn stage_seccomp_blob(bytes: &[u8]) -> std::io::Result<OwnedFd> {
    // Try memfd first.
    let name = b"qos-launcher-seccomp\0";
    let raw_fd = unsafe {
        libc::syscall(
            libc::SYS_memfd_create,
            name.as_ptr().cast::<libc::c_char>(),
            0u32,
        )
    } as RawFd;
    if raw_fd >= 0 {
        // memfd_create returns a regular fd; safe to wrap.
        let owned = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(raw_fd) };
        write_all_pwrite(raw_fd, bytes)?;
        return Ok(owned);
    }

    // Fallback: O_TMPFILE under /tmp. If even that fails we surface the IO
    // error — the launcher cannot safely continue without a seccomp blob fd.
    let path = std::ffi::CString::new("/tmp").unwrap();
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    write_all_pwrite(fd, bytes)?;
    Ok(unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) })
}

fn write_all_pwrite(fd: RawFd, bytes: &[u8]) -> std::io::Result<()> {
    let mut written = 0usize;
    while written < bytes.len() {
        let n = unsafe {
            libc::pwrite(
                fd,
                bytes[written..].as_ptr().cast::<libc::c_void>(),
                bytes.len() - written,
                written as libc::off_t,
            )
        };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "pwrite returned 0",
            ));
        }
        written += n as usize;
    }
    Ok(())
}

/// Narrow host paths a dynamically-linked ELF needs to `execve()`.
/// Excludes `/usr/bin` etc. — stopgap until every app carries a qos-runtime bundle.
fn minimal_loader_binds() -> &'static [&'static str] {
    &[
        // Dynamic loader itself.
        "/lib64/ld-linux-x86-64.so.2",
        "/lib/ld-linux-x86-64.so.2",
        "/lib/ld-linux.so.2",
        "/usr/lib64/ld-linux-x86-64.so.2",
        // libc + friends that most Rust binaries need.
        "/lib64/libc.so.6",
        "/lib64/libgcc_s.so.1",
        "/lib64/libm.so.6",
        "/lib64/libdl.so.2",
        "/lib64/libpthread.so.0",
        "/lib64/librt.so.1",
        "/lib64/libutil.so.1",
        // Dynamic linker's cache. Without it the loader falls back to a slower
        // search but still works — bound because launch-latency matters.
        "/etc/ld.so.cache",
    ]
}

/// Path inside the bwrap sandbox where the launcher binary is bind-mounted
/// to be exec'd as the Landlock shim. Kept out of `/usr` and `/tmp`.
pub const SHIM_MOUNT_PATH: &str = "/qos-launcher";

/// Decide how to expose D-Bus per the manifest's portal declaration.
/// Preferred: spawn `xdg-dbus-proxy` filtered to portals/notifications/MPRIS.
/// Fallback (proxy missing/failed, logged): bind-mount the raw session
/// bus path — abstract addresses can't be bind-mounted, so those hosts
/// get `(None, None)` and need the proxy.
fn resolve_dbus_bind(
    portals: &std::collections::BTreeMap<String, PortalPolicy>,
) -> (Option<DBusBindPlan>, Option<DBusProxyGuard>) {
    let wants_bus = portals.values().any(|p| matches!(p, PortalPolicy::ViaHost));
    if !wants_bus {
        return (None, None);
    }
    let Ok(bus_addr) = std::env::var("DBUS_SESSION_BUS_ADDRESS") else {
        tracing::warn!(
            "manifest declares a via-host portal but DBUS_SESSION_BUS_ADDRESS is unset; \
             portals will be unavailable to the sandboxed app"
        );
        return (None, None);
    };
    if bus_addr.is_empty() {
        return (None, None);
    }
    let uid = current_uid();

    // Preferred path: xdg-dbus-proxy filtered to portal-adjacent names.
    if let Some(binary) = dbus_proxy::find_proxy_binary() {
        let sock = dbus_proxy::proxy_socket_path(uid, std::process::id());
        match dbus_proxy::spawn_dbus_proxy(&binary, &bus_addr, &sock) {
            Ok(guard) => {
                let plan = DBusBindPlan {
                    host_socket: guard.socket_path().to_path_buf(),
                    sandbox_path: dbus_proxy::sandbox_bus_path(uid),
                    kind: DBusBindKind::Proxy,
                };
                tracing::info!(
                    proxy_binary = %binary.display(),
                    socket = %guard.socket_path().display(),
                    "xdg-dbus-proxy spawned for portal access"
                );
                return (Some(plan), Some(guard));
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "xdg-dbus-proxy failed to spawn; falling back to raw bus bind"
                );
                // Fall through to direct-bind fallback.
            }
        }
    } else {
        tracing::warn!(
            "xdg-dbus-proxy not found on PATH; falling back to raw bus bind. \
             Install `flatpak-xdg-utils` to filter portal traffic."
        );
    }

    // Fallback: raw bus path. Only works for unix:path=... — abstract
    // sockets degrade to "portal unavailable".
    if let Some(path) = crate::sandbox::parse_unix_path_bus_address(&bus_addr) {
        let path_buf = PathBuf::from(&path);
        let plan = DBusBindPlan {
            host_socket: path_buf.clone(),
            sandbox_path: path_buf,
            kind: DBusBindKind::Direct,
        };
        (Some(plan), None)
    } else {
        tracing::warn!(
            bus_addr = %bus_addr,
            "cannot bind D-Bus into sandbox: address is not `unix:path=` and no proxy is available"
        );
        (None, None)
    }
}

/// Current process UID via libc, avoiding a `users`/`nix` dependency.
/// Falls back to 1000 if the syscall somehow fails.
fn current_uid() -> u32 {
    // SAFETY: getuid always succeeds and returns a uid_t.
    unsafe { libc::getuid() }
}

#[cfg(test)]
mod identity_resolution_tests {
    use super::*;

    fn snapshot_and_clear() -> (Option<std::ffi::OsString>, Option<std::ffi::OsString>) {
        let x = std::env::var_os("XDG_CONFIG_HOME");
        let h = std::env::var_os("HOME");
        // SAFETY: the callers are all in the `process_env` serial group.
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("HOME");
        }
        (x, h)
    }

    fn restore(x: Option<std::ffi::OsString>, h: Option<std::ffi::OsString>) {
        // SAFETY: the callers are all in the `process_env` serial group.
        unsafe {
            match x {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            match h {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn explicit_override_wins_and_skips_existence_check() {
        let saved = snapshot_and_clear();
        let opts = RunOptions {
            identity_host_path: Some(PathBuf::from("/nowhere/does/not/exist.json")),
            ..RunOptions::default()
        };
        let resolved = resolve_identity_host_path(&opts);
        restore(saved.0, saved.1);
        assert_eq!(
            resolved,
            Some(PathBuf::from("/nowhere/does/not/exist.json")),
            "explicit path is used verbatim, no exists() check"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn xdg_wins_when_file_exists_there() {
        let saved = snapshot_and_clear();
        let tmp = tempfile::tempdir().unwrap();
        let ident = tmp.path().join("qos").join("identity").join("primary.json");
        std::fs::create_dir_all(ident.parent().unwrap()).unwrap();
        std::fs::write(&ident, b"{}").unwrap();
        // SAFETY: this test is in the `process_env` serial group.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", tmp.path()) };
        let opts = RunOptions::default();
        let resolved = resolve_identity_host_path(&opts);
        restore(saved.0, saved.1);
        assert_eq!(resolved, Some(ident));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn home_fallback_used_when_no_xdg() {
        let saved = snapshot_and_clear();
        let tmp = tempfile::tempdir().unwrap();
        let ident = tmp
            .path()
            .join(".config")
            .join("qos")
            .join("identity")
            .join("primary.json");
        std::fs::create_dir_all(ident.parent().unwrap()).unwrap();
        std::fs::write(&ident, b"{}").unwrap();
        // SAFETY: this test is in the `process_env` serial group.
        unsafe { std::env::set_var("HOME", tmp.path()) };
        let opts = RunOptions::default();
        let resolved = resolve_identity_host_path(&opts);
        restore(saved.0, saved.1);
        assert_eq!(resolved, Some(ident));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn none_when_nothing_resolves() {
        let saved = snapshot_and_clear();
        let opts = RunOptions::default();
        let resolved = resolve_identity_host_path(&opts);
        restore(saved.0, saved.1);
        assert_eq!(resolved, None);
    }
}

#[cfg(test)]
mod app_dir_tests {
    use super::*;

    fn manifest(id: &str) -> crate::manifest::MinimalManifest {
        toml::from_str(&format!(
            "[package]\nid = {id:?}\nversion = \"0.1.0\"\n\n[entrypoint]\nbin = \"bin/x\"\n"
        ))
        .expect("fixture manifest decodes")
    }

    fn opts_in(room: Option<&str>) -> RunOptions {
        RunOptions {
            workspace_id: room.map(str::to_string),
            ..RunOptions::default()
        }
    }

    /// The launch path resolves the directory rather than the manifest
    /// naming it — the room launched into decides which one.
    #[test]
    #[serial_test::serial(process_env)]
    fn the_launch_resolves_one_directory_per_room() {
        let data = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("XDG_DATA_HOME");
        // SAFETY: this test is in the `process_env` serial group.
        unsafe { std::env::set_var("XDG_DATA_HOME", data.path()) };

        let m = manifest("dev.qos.fixture");
        let a = resolve_app_dir(&m, &opts_in(Some("room-a"))).expect("room a");
        let b = resolve_app_dir(&m, &opts_in(Some("room-b"))).expect("room b");
        let personal = resolve_app_dir(&m, &opts_in(None)).expect("personal");

        // SAFETY: this test is in the `process_env` serial group.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }

        assert!(a.is_dir() && b.is_dir() && personal.is_dir());
        assert_ne!(a, b);
        assert_ne!(a, personal);
        assert!(
            !a.starts_with(&b) && !b.starts_with(&a),
            "neither room may contain the other: {a:?} {b:?}"
        );
        for dir in [&a, &b, &personal] {
            assert!(
                dir.starts_with(data.path().join("qos-rooms")),
                "{dir:?} is outside the rooms tree"
            );
        }
    }

    /// A package cannot make the launcher create a directory it named: the
    /// retired `sandbox.fs.write` block is refused before the directory step.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_declared_write_path_is_never_created() {
        let data = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("XDG_DATA_HOME");
        // SAFETY: this test is in the `process_env` serial group.
        unsafe { std::env::set_var("XDG_DATA_HOME", data.path()) };

        let mut m = manifest("dev.qos.fixture");
        let victim = data.path().join("dev.qos.totp");
        m.sandbox.fs.write.push(victim.display().to_string());
        let refused = run_with(&m, Path::new("/nonexistent"), &opts_in(None));

        // SAFETY: this test is in the `process_env` serial group.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }

        let err = refused.expect_err("a manifest naming a write path must be refused");
        assert!(
            matches!(err, RunError::FsPolicy(_)),
            "expected a policy refusal, got: {err}"
        );
        assert!(
            !victim.exists(),
            "the launcher must not create a path a package named"
        );
    }
}
