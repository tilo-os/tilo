use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use windows_vm::{
    ApplianceConfig, DomainState, EgressLease, EgressPurpose, GraphicsAssignment,
    GraphicsCapabilities, LibvirtBackend, StdCommandRunner, VmBackend,
};

const ROOT: &str = "/var/lib/tilo/windows";
const UUID_FILE: &str = "/var/lib/tilo/windows/domain.uuid";
const SYSTEM_DISK: &str = "/var/lib/tilo/windows/system.qcow2";
/// Group-readable by the normal Tilo owner, for `owner-status`.
const APPLIANCE_STATE: &str = "/var/lib/tilo/windows/session/state.json";
const NVRAM: &str = "/var/lib/tilo/windows/nvram.fd";
// Keep controller egress state apart from per-session portal-file lease
// projections below `$XDG_RUNTIME_DIR/tilo/windows-vm`.
const EGRESS_RUNTIME_ROOT: &str = "/run/tilo/windows-egress";
const LEASE_ROOT: &str = "/run/tilo/windows-egress/leases";
const EGRESS_SUMMARY: &str = "/run/tilo/windows-egress/summary.json";
const STATUS_SUMMARY: &str = "/run/tilo/windows-egress/status.json";
const CONTROLLER_LOCK: &str = "/run/tilo/windows-egress/controller.lock";
/// Records the reaper could not retire within `REAP_GIVE_UP_SECONDS`. They
/// stay out of `LEASE_ROOT` so they cannot pin the reaper through its path
/// unit; every later locked pass retries each once, and `egress-open`
/// refuses to attach a new NIC while any remains.
const UNRETIRED_ROOT: &str = "/run/tilo/windows-egress/unretired";
const GRAPHICS_ASSIGNMENT: &str = "/var/lib/tilo/windows/graphics-assignment.json";

fn main() -> ExitCode {
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    if arguments == ["egress-reap", "--until-idle"] {
        return run_reaper();
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("tilo-windows-vm-control: {message}");
            ExitCode::FAILURE
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the root controller keeps its complete typed action dispatch in one auditable function"
)]
fn run() -> Result<(), String> {
    let mut arguments = env::args_os().skip(1);
    let action = arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(usage)?;
    let rest = arguments
        .map(|value| value.into_string().map_err(|_| usage()))
        .collect::<Result<Vec<_>, _>>()?;

    // This read-only probe is useful before the appliance is provisioned. It
    // never creates a VF/mdev or unbinds the GPU currently displaying Tilo.
    if action == "graphics-capabilities" {
        if !rest.is_empty() {
            return Err(usage());
        }
        let capabilities = GraphicsCapabilities::probe().map_err(|error| error.to_string())?;
        println!(
            "{}",
            serde_json::to_string(&capabilities)
                .map_err(|error| format!("cannot encode graphics capabilities: {error}"))?
        );
        return Ok(());
    }

    // The owner's read-only summary runs unprivileged, from the owner helper,
    // and must answer before the appliance is provisioned.
    if action == "owner-status" {
        if !rest.is_empty() {
            return Err(usage());
        }
        println!(
            "{}",
            windows_vm::owner_status_json(Path::new(APPLIANCE_STATE), Path::new(STATUS_SUMMARY), 0,)
        );
        return Ok(());
    }

    // The reaper also runs without a provisioned appliance, so it is
    // dispatched before the domain identity is required.
    if action == "egress-reap" {
        return match rest.as_slice() {
            // `egress-reap --until-idle` is dispatched by `main`.
            [] => reap_pass(unix_now()?, false).map(|_| ()),
            _ => Err(usage()),
        };
    }

    let (config, backend) = appliance()?;

    if !matches!(
        action.as_str(),
        "egress-open" | "egress-close" | "graphics-attach"
    ) && !rest.is_empty()
    {
        return Err(usage());
    }

    // All operations that inspect or mutate the live NIC/lease set share one
    // root-owned lock. This serialises concurrent pkexec requests with the
    // reaper (`reap_pass`), so an old lease can never remove a newer NIC or
    // summary.
    let _controller_lock = matches!(
        action.as_str(),
        "egress-open"
            | "egress-close"
            | "egress-revoke"
            | "start"
            | "shutdown"
            | "force-shutdown"
            | "restart"
            | "suspend"
            | "resume"
            | "define"
            | "graphics-attach"
            | "graphics-detach"
    )
    .then(acquire_controller_lock)
    .transpose()?;

    // A live-only NIC disappears on a domain restart, so retire its lease
    // records before any power transition. This also makes a later retry
    // unable to resurrect an already-expired capability.
    if matches!(
        action.as_str(),
        "shutdown" | "force-shutdown" | "restart" | "suspend"
    ) {
        close_all_leases(&backend, &config)?;
        write_status_summary(&backend, &config)?;
    }

    match action.as_str() {
        "status" => {
            let state = backend.state(&config).map_err(|error| error.to_string())?;
            let temporary_egress = read_egress_summary()?
                .map(|(purpose, expires_unix_seconds)| {
                    format!(
                        ",\"temporary_egress\":{{\"purpose\":\"{purpose}\",\"expires_at_unix\":{expires_unix_seconds}}}"
                    )
                })
                .unwrap_or_default();
            println!(
                "{{\"schema_version\":1,\"domain\":\"{}\"{temporary_egress}}}",
                domain_state_name(state)
            );
            Ok(())
        }
        "define" => backend.ensure_singleton(&config).map_err(|e| e.to_string()),
        "start" => backend.start(&config).map_err(|e| e.to_string()),
        "shutdown" => backend.shutdown(&config).map_err(|e| e.to_string()),
        "force-shutdown" => backend.force_shutdown(&config).map_err(|e| e.to_string()),
        "restart" => backend.restart(&config).map_err(|e| e.to_string()),
        "suspend" => backend.suspend(&config).map_err(|e| e.to_string()),
        "resume" => backend.resume(&config).map_err(|e| e.to_string()),
        "graphics-attach" => {
            let [kind, identifier] = rest.as_slice() else {
                return Err("usage: tilo-windows-vm-control graphics-attach {sriov-vf|mdev} <canonical-id>".to_string());
            };
            if Path::new(GRAPHICS_ASSIGNMENT).exists() {
                return Err("a Windows graphics partition is already recorded; detach it first".to_string());
            }
            let capabilities = GraphicsCapabilities::probe().map_err(|error| error.to_string())?;
            let assignment = match kind.as_str() {
                "sriov-vf" => capabilities.sriov_assignment(identifier),
                "mdev" => capabilities.mediated_assignment(identifier),
                _ => None,
            }
            .ok_or_else(|| {
                "the selected partition is not an existing VFIO-ready display VF/mdev"
                    .to_string()
            })?;
            write_graphics_assignment(&assignment)?;
            if let Err(error) = backend.attach_graphics(&config, &assignment) {
                let _ = fs::remove_file(GRAPHICS_ASSIGNMENT);
                return Err(error.to_string());
            }
            println!(
                "{}",
                serde_json::to_string(&assignment)
                    .map_err(|error| format!("cannot encode graphics assignment: {error}"))?
            );
            Ok(())
        }
        "graphics-detach" => {
            let assignment = read_graphics_assignment()?;
            backend
                .detach_graphics(&config, &assignment)
                .map_err(|error| error.to_string())?;
            fs::remove_file(GRAPHICS_ASSIGNMENT)
                .map_err(|error| format!("cannot retire graphics assignment: {error}"))?;
            println!("{{\"schema_version\":1,\"result\":\"detached\"}}");
            Ok(())
        }
        "egress-open" => {
            let purpose = match rest.first().map(String::as_str) {
                Some("activation") => EgressPurpose::Activation,
                Some("windows-update") => EgressPurpose::WindowsUpdate,
                _ => return Err("usage: tilo-windows-vm-control egress-open {activation|windows-update} [ttl-seconds]".to_string()),
            };
            let ttl = match rest.get(1) {
                None => 600,
                Some(value) => value.parse::<u64>().map_err(|_| "invalid egress TTL".to_string())?,
            };
            if rest.len() > 2 { return Err(usage()); }
            // A user asking for a new connection should not wait for the
            // reaper service to retire an already-expired record; reap it
            // transactionally while this controller lock is held.
            reap_leases(&backend, &config, unix_now()?)?;
            if active_lease()?.is_some() {
                return Err("an egress lease is already active".to_string());
            }
            // A set-aside record may still stand for an attached NIC.
            if !lease_dir_entries(Path::new(UNRETIRED_ROOT))?.is_empty() {
                return Err("an expired egress lease could not be revoked yet; retry later".to_string());
            }
            let now = tilo_clock::try_unix_now().ok_or_else(|| "system clock is before the Unix epoch".to_string())?;
            let pending_id = format!("egress-pending-{}", std::process::id());
            write_lease(&pending_id, purpose, now.saturating_add(ttl))?;
            let lease = match backend.open_egress(&config, purpose, now, ttl) {
                Ok(lease) => lease,
                Err(error) => { remove_lease(&pending_id); return Err(error.to_string()); }
            };
            let pending_path = lease_path(&pending_id)?;
            let active_path = lease_path(&lease.id)?;
            if active_path.exists() {
                return Err(rollback_attached_egress(
                    &backend,
                    &config,
                    &lease,
                    &pending_id,
                    "an egress lease became active during setup".to_string(),
                ));
            }
            if let Err(error) = fs::rename(pending_path, active_path) {
                return Err(rollback_attached_egress(
                    &backend,
                    &config,
                    &lease,
                    &pending_id,
                    format!("cannot commit the egress lease: {error}"),
                ));
            }
            if let Err(error) = write_egress_summary(lease.purpose, lease.expires_unix_seconds) {
                return Err(rollback_attached_egress(
                    &backend,
                    &config,
                    &lease,
                    &lease.id,
                    error,
                ));
            }
            println!("{{\"schema_version\":1,\"id\":\"{}\",\"purpose\":\"{}\",\"expires_unix_seconds\":{}}}", lease.id, lease.purpose.as_str(), lease.expires_unix_seconds);
            Ok(())
        }
        "egress-close" => {
            let id = rest.first().cloned().ok_or_else(|| "usage: tilo-windows-vm-control egress-close <lease-id>".to_string())?;
            if rest.len() != 1 { return Err(usage()); }
            let lease = EgressLease { id, purpose: EgressPurpose::WindowsUpdate, expires_unix_seconds: 0 };
            backend.close_egress(&config, &lease).map_err(|e| e.to_string())?;
            remove_lease(&lease.id);
            remove_egress_summary();
            Ok(())
        }
        "egress-revoke" => {
            if !rest.is_empty() {
                return Err(usage());
            }
            let Some((id, purpose, expires_unix_seconds)) = active_lease()?
            else {
                return Err("there is no active Windows egress lease".to_string());
            };
            let lease = EgressLease {
                id: id.clone(),
                purpose,
                expires_unix_seconds,
            };
            backend.close_egress(&config, &lease).map_err(|error| error.to_string())?;
            remove_lease(&id);
            remove_egress_summary();
            println!("{{\"schema_version\":1,\"result\":\"revoked\"}}");
            Ok(())
        }
        _ => return Err(usage()),
    }
    .map_err(|error| format!("cannot manage the singleton appliance: {error}"))?;

    if matches!(
        action.as_str(),
        "egress-open"
            | "egress-close"
            | "egress-revoke"
            | "start"
            | "shutdown"
            | "force-shutdown"
            | "restart"
            | "suspend"
            | "resume"
    ) {
        write_status_summary(&backend, &config)?;
    }

    Ok(())
}

fn usage() -> String {
    "usage: tilo-windows-vm-control {status|owner-status|graphics-capabilities|graphics-attach|graphics-detach|define|start|shutdown|force-shutdown|restart|suspend|resume|egress-open|egress-close|egress-revoke|egress-reap [--until-idle]}".to_string()
}

fn write_graphics_assignment(assignment: &GraphicsAssignment) -> Result<(), String> {
    let bytes = serde_json::to_vec(assignment)
        .map_err(|error| format!("cannot encode graphics assignment: {error}"))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(GRAPHICS_ASSIGNMENT)
        .map_err(|error| format!("cannot record graphics assignment: {error}"))?;
    let result = file
        .write_all(&bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("cannot commit graphics assignment: {error}"));
    if result.is_err() {
        let _ = fs::remove_file(GRAPHICS_ASSIGNMENT);
    }
    result
}

fn read_graphics_assignment() -> Result<GraphicsAssignment, String> {
    let metadata = fs::symlink_metadata(GRAPHICS_ASSIGNMENT)
        .map_err(|_| "no Tilo-managed Windows graphics partition is attached".to_string())?;
    if !metadata.file_type().is_file()
        || metadata.uid() != 0
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err("the graphics assignment record has unsafe permissions".to_string());
    }
    let bytes = fs::read(GRAPHICS_ASSIGNMENT)
        .map_err(|error| format!("cannot read graphics assignment: {error}"))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("graphics assignment record is invalid: {error}"))
}

/// `_` is allowed because the backend names a lease after its purpose
/// (`egress-windows_update-<t>`); a lease ID still cannot name another
/// directory or file type.
fn lease_path(id: &str) -> Result<std::path::PathBuf, String> {
    if !id.starts_with("egress-")
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err("invalid egress lease identifier".to_string());
    }
    Ok(std::path::Path::new(LEASE_ROOT).join(format!("{id}.json")))
}

fn write_lease(id: &str, purpose: EgressPurpose, expires: u64) -> Result<(), String> {
    let path = lease_path(id)?;
    fs::create_dir_all(LEASE_ROOT)
        .map_err(|error| format!("cannot create lease directory: {error}"))?;
    fs::set_permissions(LEASE_ROOT, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot secure egress lease directory: {error}"))?;
    let bytes = format!("{{\"schema_version\":1,\"id\":\"{id}\",\"purpose\":\"{}\",\"expires_unix_seconds\":{expires}}}\n", purpose.as_str()).into_bytes();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|error| format!("cannot create egress lease: {error}"))?;
    let result = file
        .write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("cannot commit egress lease: {error}"));
    if result.is_err() {
        let _ = fs::remove_file(&path);
    }
    result
}

/// Publish only the user-safe egress purpose and expiry. The controller's
/// opaque lease ID remains in its root-only record under `LEASE_ROOT`.
fn write_egress_summary(purpose: EgressPurpose, expires: u64) -> Result<(), String> {
    ensure_egress_runtime_root()?;
    let bytes = format!(
        "{{\"schema_version\":1,\"purpose\":\"{}\",\"expires_at_unix\":{expires}}}\n",
        purpose.as_str()
    );
    tilo_files::native::atomic::write_with_mode(Path::new(EGRESS_SUMMARY), bytes.as_bytes(), 0o640)
        .map_err(|error| format!("cannot publish egress summary: {error}"))
}

fn ensure_egress_runtime_root() -> Result<(), String> {
    let root = Path::new(EGRESS_RUNTIME_ROOT);
    fs::create_dir_all(root)
        .map_err(|error| format!("cannot create egress runtime root: {error}"))?;
    // Repair only a wrong mode. The reaper service runs without
    // CAP_FSETID and outside the directory's group, so its chmod would
    // silently clear the setgid bit that tmpfiles set and later summaries
    // would no longer be readable by the tilo-identity group.
    let mode = fs::metadata(root)
        .map_err(|error| format!("cannot inspect egress runtime root: {error}"))?
        .permissions()
        .mode();
    if mode & 0o7777 == 0o2750 {
        return Ok(());
    }
    fs::set_permissions(root, fs::Permissions::from_mode(0o2750))
        .map_err(|error| format!("cannot secure egress runtime root: {error}"))
}

fn acquire_controller_lock() -> Result<advisory_lock::FileLock, String> {
    ensure_egress_runtime_root()?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(CONTROLLER_LOCK)
        .map_err(|error| format!("cannot open Windows controller lock: {error}"))?;
    advisory_lock::FileLock::exclusive(file)
        .map_err(|error| format!("cannot lock Windows controller: {error}"))
}

fn write_status_summary<R: VmBackend>(backend: &R, config: &ApplianceConfig) -> Result<(), String> {
    ensure_egress_runtime_root()?;
    let state = backend
        .state(config)
        .map_err(|error| format!("cannot read Windows domain state for status summary: {error}"))?;
    let power_state = match state {
        DomainState::Running => "running",
        DomainState::Suspended => "suspended",
        DomainState::Paused => "paused",
        DomainState::ShutOff => "shut_down",
        DomainState::Absent
        | DomainState::Starting
        | DomainState::ShuttingDown
        | DomainState::Crashed
        | DomainState::Other => "unknown",
    };
    let temporary_egress = active_lease()?
        .map(|(_, purpose, expires_at_unix)| {
            format!(
                ",\"temporary_egress\":{{\"purpose\":\"{}\",\"expires_at_unix\":{expires_at_unix}}}",
                purpose.as_str()
            )
        })
        .unwrap_or_default();
    let summary =
        format!("{{\"schema_version\":1,\"power_state\":\"{power_state}\"{temporary_egress}}}\n");
    tilo_files::native::atomic::write_with_mode(
        Path::new(STATUS_SUMMARY),
        summary.as_bytes(),
        0o640,
    )
    .map_err(|error| format!("cannot publish Windows status summary: {error}"))
}

fn remove_egress_summary() {
    let _ = fs::remove_file(EGRESS_SUMMARY);
}

fn read_egress_summary() -> Result<Option<(&'static str, u64)>, String> {
    let metadata = match fs::symlink_metadata(EGRESS_SUMMARY) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot inspect egress summary: {error}")),
    };
    if !metadata.file_type().is_file()
        || metadata.uid() != 0
        || metadata.permissions().mode() & 0o007 != 0
    {
        return Err("egress summary has unsafe permissions".to_string());
    }
    let value: serde_json::Value = serde_json::from_slice(
        &fs::read(EGRESS_SUMMARY)
            .map_err(|error| format!("cannot read egress summary: {error}"))?,
    )
    .map_err(|_| "egress summary is malformed".to_string())?;
    if value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Err("egress summary has an unsupported schema".to_string());
    }
    let purpose = match value.get("purpose").and_then(serde_json::Value::as_str) {
        Some("activation") => "activation",
        Some("windows_update") => "windows_update",
        _ => return Err("egress summary has an invalid purpose".to_string()),
    };
    let expires = value
        .get("expires_at_unix")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "egress summary has an invalid expiry".to_string())?;
    Ok(Some((purpose, expires)))
}

fn remove_lease(id: &str) {
    if let Ok(path) = lease_path(id) {
        let _ = fs::remove_file(path);
    }
}

/// Undo an attachment only when libvirt confirms that the live NIC is gone.
/// On a transient detach failure the root-only lease record deliberately
/// remains available to manual revoke and the lease reaper.
fn rollback_attached_egress<R: VmBackend>(
    backend: &R,
    config: &ApplianceConfig,
    lease: &EgressLease,
    recorded_id: &str,
    cause: String,
) -> String {
    match backend.close_egress(config, lease) {
        Ok(()) => {
            remove_lease(recorded_id);
            remove_egress_summary();
            cause
        }
        Err(detach_error) => {
            let status_detail = write_status_summary(backend, config)
                .err()
                .map(|error| format!("; runtime status refresh also failed: {error}"))
                .unwrap_or_default();
            format!(
                "{cause}; live egress detach failed and its lease was retained for retry: {detach_error}{status_detail}"
            )
        }
    }
}

/// Read the one active egress lease without ever printing its opaque ID in a
/// management status response.  Lease files are controller-owned and their
/// filename still passes through `lease_path` before use.
fn active_lease() -> Result<Option<(String, EgressPurpose, u64)>, String> {
    let mut active = None;
    for path in lease_dir_entries(Path::new(LEASE_ROOT))? {
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let lease = read_lease_record(&path)?;
        if active
            .replace((lease.id, lease.purpose, lease.expires_unix_seconds))
            .is_some()
        {
            return Err("more than one Windows egress lease is active".to_string());
        }
    }
    Ok(active)
}

/// The one reader for controller-written lease records: `active_lease`, the
/// reaper's locked passes and its unlocked scan all go through it.
fn read_lease_record(path: &Path) -> Result<EgressLease, String> {
    let source =
        fs::read_to_string(path).map_err(|error| format!("cannot read egress lease: {error}"))?;
    let lease = parse_lease_record(&source)?;
    if lease_path(&lease.id)?.file_name() != path.file_name() {
        return Err("an egress lease is stored under an unexpected name".to_string());
    }
    Ok(lease)
}

fn parse_lease_record(source: &str) -> Result<EgressLease, String> {
    let value: serde_json::Value = serde_json::from_str(source)
        .map_err(|_| "an egress lease record is malformed".to_string())?;
    let id = value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "an egress lease has no id".to_string())?;
    let purpose = match value.get("purpose").and_then(serde_json::Value::as_str) {
        Some("activation") => EgressPurpose::Activation,
        Some("windows_update") => EgressPurpose::WindowsUpdate,
        _ => return Err("an egress lease has an invalid purpose".to_string()),
    };
    let expires_unix_seconds = value
        .get("expires_unix_seconds")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "an egress lease has an invalid expiry".to_string())?;
    Ok(EgressLease {
        id: id.to_string(),
        purpose,
        expires_unix_seconds,
    })
}

fn lease_dir_entries(root: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot inspect egress leases: {error}")),
    };
    entries
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| format!("cannot inspect egress lease: {error}"))
        })
        .collect()
}

/// Retire the expired leases in `LEASE_ROOT` and retry every record an
/// earlier reaper set aside. Returns what the pass found in `LEASE_ROOT`.
fn reap_leases<R: VmBackend>(
    backend: &R,
    config: &ApplianceConfig,
    now: u64,
) -> Result<ReapPass, String> {
    let pass = reap_lease_records(backend, config, Path::new(LEASE_ROOT), now)?;
    // Every set-aside record has expired already; each pass retries it once
    // and never schedules anything for it.
    let unretired = reap_lease_records(backend, config, Path::new(UNRETIRED_ROOT), u64::MAX)?;
    if pass.closed_any || unretired.closed_any || active_lease()?.is_none() {
        remove_egress_summary();
    }
    Ok(pass)
}

#[derive(Debug, Default, Eq, PartialEq)]
struct ReapPass {
    /// A lease NIC was detached, or found gone, and its record retired.
    closed_any: bool,
    /// Expired records whose NIC could not be detached, kept for a retry.
    failed: Vec<PathBuf>,
}

/// Retire every expired record below `root`.
///
/// A lease record that cannot be read counts as expired: it may stand for a
/// live NIC, and the egress NIC has one fixed MAC, so it can be detached
/// without its lease. Records are only read here under the controller lock,
/// which `egress-open` also holds while it writes one, so an unreadable
/// record is damaged rather than half-written. An entry that is not a lease
/// record at all is removed, so nothing but a lease keeps the reaper alive.
fn reap_lease_records<R: VmBackend>(
    backend: &R,
    config: &ApplianceConfig,
    root: &Path,
    now: u64,
) -> Result<ReapPass, String> {
    let mut pass = ReapPass::default();
    for path in lease_dir_entries(root)? {
        if !is_lease_record(&path) {
            remove_entry(&path, "it is not an egress lease record");
            continue;
        }
        let lease = match read_lease_record(&path) {
            Ok(lease) if lease.expires_unix_seconds > now => continue,
            Ok(lease) => lease,
            Err(error) => {
                eprintln!(
                    "tilo-windows-vm-control: revoking egress for the unreadable lease record {}: {error}",
                    path.display()
                );
                EgressLease {
                    id: "egress-unreadable-record".to_string(),
                    purpose: EgressPurpose::WindowsUpdate,
                    expires_unix_seconds: 0,
                }
            }
        };
        match retire_lease(backend, config, &lease) {
            Ok(()) => {
                let _ = fs::remove_file(&path);
                pass.closed_any = true;
            }
            Err(error) => {
                eprintln!(
                    "tilo-windows-vm-control: cannot revoke egress lease {}: {error}",
                    lease.id
                );
                pass.failed.push(path);
            }
        }
    }
    Ok(pass)
}

/// A lease record is a regular `egress-*.json` file.
fn is_lease_record(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("json")
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("egress-"))
        && fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
}

fn remove_entry(path: &Path, reason: &str) {
    eprintln!(
        "tilo-windows-vm-control: removing {} from the egress lease records: {reason}",
        path.display()
    );
    let removed = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        _ => fs::remove_file(path),
    };
    if let Err(error) = removed {
        eprintln!(
            "tilo-windows-vm-control: cannot remove {}: {error}",
            path.display()
        );
    }
}

/// Detach a lease's NIC, or confirm that the domain cannot hold it: the NIC
/// is attached live only, so a shut-off or undefined domain has none, and
/// `detach-interface --live` on it fails for good.
fn retire_lease<R: VmBackend>(
    backend: &R,
    config: &ApplianceConfig,
    lease: &EgressLease,
) -> Result<(), String> {
    let Err(close_error) = backend.close_egress(config, lease) else {
        return Ok(());
    };
    match backend.state(config) {
        Ok(DomainState::ShutOff | DomainState::Absent) => Ok(()),
        Ok(_) | Err(_) => Err(close_error.to_string()),
    }
}

/// Drop every record below `root`. Used only without a provisioned
/// appliance, where no domain can hold an egress NIC and none can be
/// detached.
fn discard_records(root: &Path) -> Result<(), String> {
    for path in lease_dir_entries(root)? {
        remove_entry(&path, "no Windows appliance is provisioned");
    }
    Ok(())
}

/// Move the records the reaper gave up on out of `LEASE_ROOT`, where they
/// would keep its path unit firing. Every later locked pass retries them.
fn set_aside(paths: &[PathBuf], unretired_root: &Path) -> Result<(), String> {
    fs::create_dir_all(unretired_root)
        .map_err(|error| format!("cannot create the unretired lease directory: {error}"))?;
    fs::set_permissions(unretired_root, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot secure the unretired lease directory: {error}"))?;
    for path in paths {
        let Some(name) = path.file_name() else {
            continue;
        };
        match fs::rename(path, unretired_root.join(name)) {
            Ok(()) => eprintln!(
                "tilo-windows-vm-control: gave up revoking egress lease record {} after {REAP_GIVE_UP_SECONDS} s; set it aside for the status timer to retry",
                path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "cannot set aside egress lease record {}: {error}",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

fn unix_now() -> Result<u64, String> {
    tilo_clock::try_unix_now().ok_or_else(|| "system clock is before the Unix epoch".to_string())
}

fn appliance() -> Result<(ApplianceConfig, LibvirtBackend<StdCommandRunner>), String> {
    require_regular_file(Path::new(UUID_FILE), "domain identity")?;
    require_regular_file(Path::new(SYSTEM_DISK), "system disk")?;
    let domain_uuid = fs::read_to_string(UUID_FILE)
        .map_err(|error| format!("cannot read the protected domain identity: {error}"))?;
    Ok((
        ApplianceConfig::recommended(domain_uuid.trim(), SYSTEM_DISK, NVRAM),
        LibvirtBackend::new(StdCommandRunner),
    ))
}

/// One locked reaper pass: retire expired leases and refresh the public
/// status summary. With `give_up`, records that still cannot be retired are
/// set aside in the same locked section.
fn reap_pass(now: u64, give_up: bool) -> Result<ReapPass, String> {
    let _lock = acquire_controller_lock()?;
    if !Path::new(UUID_FILE).exists() {
        // Also drop any public status a failed or replaced appliance left.
        discard_records(Path::new(LEASE_ROOT))?;
        discard_records(Path::new(UNRETIRED_ROOT))?;
        remove_egress_summary();
        let _ = fs::remove_file(STATUS_SUMMARY);
        return Ok(ReapPass::default());
    }
    let (config, backend) = appliance()?;
    let mut pass = reap_leases(&backend, &config, now)?;
    if give_up && !pass.failed.is_empty() {
        set_aside(&pass.failed, Path::new(UNRETIRED_ROOT))?;
        pass.failed.clear();
    }
    // The status summary is informational; failing to refresh it must not
    // stop the reaper from enforcing lease expiry.
    if let Err(error) = write_status_summary(&backend, &config) {
        eprintln!("tilo-windows-vm-control: {error}");
    }
    Ok(pass)
}

/// The status summary is refreshed this often while a lease keeps the
/// reaper running, matching the status timer's period.
const STATUS_REFRESH_SECONDS: u64 = 30;
/// The reaper wakes this often while a lease record exists. Each wake reads
/// the records afresh, so a lease opened while it slept is enforced at its
/// own expiry; and because the wall clock is re-read, an expiry that passed
/// while the host was suspended is acted on within this bound of resume.
const MAX_SLEEP_SECONDS: u64 = 5;
/// A lease whose NIC could not be detached is retried after this delay,
/// doubling up to `REAP_RETRY_MAX_SECONDS`.
const REAP_RETRY_SECONDS: u64 = 5;
const REAP_RETRY_MAX_SECONDS: u64 = 60;
/// After this long without a successful pass the reaper sets the failing
/// records aside and stops, instead of polling as root without end.
const REAP_GIVE_UP_SECONDS: u64 = 10 * 60;
/// Exit status when no lease is left to enforce but the last pass failed.
/// The unit's `RestartPreventExitStatus=` names it: there is nothing to
/// retry, only something to report.
const IDLE_AFTER_FAILURE_EXIT: u8 = 3;

/// An unlocked look at the lease records between locked passes.
#[derive(Debug, Default, Eq, PartialEq)]
struct LeaseScan {
    entries: usize,
    /// Expiries of the records that could be read. A record being written
    /// reads as unreadable here and is left to the next locked pass.
    expiries: Vec<u64>,
}

fn scan_lease_records(root: &Path) -> Result<LeaseScan, String> {
    let paths = lease_dir_entries(root)?;
    Ok(LeaseScan {
        entries: paths.len(),
        expiries: paths
            .iter()
            .filter_map(|path| read_lease_record(path).ok())
            .map(|lease| lease.expires_unix_seconds)
            .collect(),
    })
}

#[derive(Debug, Eq, PartialEq)]
enum Wake {
    Pass { give_up: bool },
    Sleep,
    Idle,
}

/// When the reaper runs its next locked pass, kept apart from the I/O so the
/// policy is tested as it runs.
#[derive(Debug, Default)]
struct ReaperSchedule {
    /// Unix time of the last locked pass; `None` before the first.
    last_pass: Option<u64>,
    /// Unix time the next pass is due even if no lease expires before it.
    next_pass: u64,
    /// Start of the current run of failed passes.
    failing_since: Option<u64>,
    failures: u32,
    last_error: Option<String>,
}

impl ReaperSchedule {
    fn on_wake(&self, scan: &LeaseScan, now: u64) -> Wake {
        let Some(last_pass) = self.last_pass else {
            return Wake::Pass { give_up: false };
        };
        if scan.entries == 0 {
            return Wake::Idle;
        }
        let newly_expired = scan
            .expiries
            .iter()
            .any(|expiry| *expiry > last_pass && *expiry <= now);
        if !newly_expired && now < self.next_pass {
            return Wake::Sleep;
        }
        let give_up = self
            .failing_since
            .is_some_and(|since| now.saturating_sub(since) >= REAP_GIVE_UP_SECONDS);
        Wake::Pass { give_up }
    }

    fn after_pass(&mut self, outcome: &Result<ReapPass, String>, now: u64) {
        self.last_pass = Some(now);
        let failure = match outcome {
            Ok(pass) if pass.failed.is_empty() => None,
            Ok(_) => Some("an expired egress lease could not be revoked".to_string()),
            Err(error) => Some(error.clone()),
        };
        let Some(error) = failure else {
            self.failing_since = None;
            self.failures = 0;
            self.last_error = None;
            self.next_pass = now.saturating_add(STATUS_REFRESH_SECONDS);
            return;
        };
        self.failing_since.get_or_insert(now);
        let backoff = REAP_RETRY_SECONDS
            .saturating_mul(2_u64.saturating_pow(self.failures.min(4)))
            .min(REAP_RETRY_MAX_SECONDS);
        self.failures = self.failures.saturating_add(1);
        self.next_pass = now.saturating_add(backoff);
        self.last_error = Some(error);
    }
}

#[derive(Debug, Eq, PartialEq)]
enum ReaperExit {
    Idle,
    IdleAfterFailure(String),
}

/// `egress-reap --until-idle`, the reaper service's entry point.
fn run_reaper() -> ExitCode {
    match reap_until_idle() {
        Ok(ReaperExit::Idle) => ExitCode::SUCCESS,
        Ok(ReaperExit::IdleAfterFailure(error)) => {
            eprintln!(
                "tilo-windows-vm-control: no egress lease is left to enforce, but the last reaper pass failed: {error}"
            );
            ExitCode::from(IDLE_AFTER_FAILURE_EXIT)
        }
        Err(error) => {
            eprintln!("tilo-windows-vm-control: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Run until no lease record is left. The `tilo-windows-vm-egress-lease.path`
/// unit starts this as soon as a record exists, which is before its NIC is
/// attached, and the status timer starts it on a provisioned machine. It
/// exits only once `LEASE_ROOT` is empty, so the path unit, which fires
/// while the directory is not empty, never starts it again straight away.
fn reap_until_idle() -> Result<ReaperExit, String> {
    let root = Path::new(LEASE_ROOT);
    let mut schedule = ReaperSchedule::default();
    loop {
        let now = unix_now()?;
        match schedule.on_wake(&scan_lease_records(root)?, now) {
            Wake::Idle => {
                return Ok(schedule
                    .last_error
                    .take()
                    .map_or(ReaperExit::Idle, ReaperExit::IdleAfterFailure));
            }
            Wake::Pass { give_up } => {
                let outcome = reap_pass(now, give_up);
                if let Err(error) = &outcome {
                    eprintln!("tilo-windows-vm-control: egress reaper pass failed: {error}");
                }
                schedule.after_pass(&outcome, now);
                // Decide at once whether anything is left to wait for.
                continue;
            }
            Wake::Sleep => {}
        }
        std::thread::sleep(Duration::from_secs(MAX_SLEEP_SECONDS));
    }
}

fn close_all_leases<R: VmBackend>(backend: &R, config: &ApplianceConfig) -> Result<(), String> {
    let Some((id, purpose, expires_unix_seconds)) = active_lease()? else {
        remove_egress_summary();
        return Ok(());
    };
    let lease = EgressLease {
        id: id.clone(),
        purpose,
        expires_unix_seconds,
    };
    // Never discard the controller record until libvirt confirms the live
    // NAT interface is detached. On a transient failure the lifecycle action
    // stops, while the lease remains visible and retryable by UI/reaper.
    backend.close_egress(config, &lease).map_err(|error| {
        format!("cannot revoke Windows egress before power transition: {error}")
    })?;
    remove_lease(&id);
    remove_egress_summary();
    Ok(())
}

fn domain_state_name(state: DomainState) -> &'static str {
    match state {
        DomainState::Absent => "absent",
        DomainState::ShutOff => "shut_off",
        DomainState::Running => "running",
        DomainState::Paused => "paused",
        DomainState::Suspended => "suspended",
        DomainState::Starting => "starting",
        DomainState::ShuttingDown => "shutting_down",
        DomainState::Crashed => "crashed",
        DomainState::Other => "other",
    }
}

fn require_regular_file(path: &Path, label: &str) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("missing or unreadable {label} below {ROOT}: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err(format!("{label} below {ROOT} is not a regular file"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use advisory_lock::{FileLock, LockMode, TryLockError};

    #[test]
    fn controller_lock_contract_is_exclusive_across_concurrent_callers() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("controller.lock");
        let first = FileLock::open(&path, LockMode::Exclusive).unwrap();
        assert!(matches!(
            FileLock::try_open(&path, LockMode::Exclusive),
            Err(TryLockError::WouldBlock)
        ));
        drop(first);
        drop(FileLock::try_open(&path, LockMode::Exclusive).unwrap());
    }

    use std::path::Path;
    use std::sync::Mutex;

    use windows_vm::{
        ApplianceConfig, DomainState, EgressLease, EgressPurpose, VmBackend, VmBackendError,
    };

    use super::{
        LeaseScan, REAP_GIVE_UP_SECONDS, REAP_RETRY_MAX_SECONDS, REAP_RETRY_SECONDS, ReapPass,
        ReaperSchedule, STATUS_REFRESH_SECONDS, Wake, discard_records, lease_path,
        parse_lease_record, reap_lease_records, scan_lease_records, set_aside,
    };

    /// Records which leases the reaper detached. Detaching fails on demand,
    /// and `state` is what libvirt reports for the domain.
    struct EgressBackend {
        detach_fails: bool,
        state: DomainState,
        closed: Mutex<Vec<String>>,
    }

    impl EgressBackend {
        fn new(detach_fails: bool, state: DomainState) -> Self {
            Self {
                detach_fails,
                state,
                closed: Mutex::new(Vec::new()),
            }
        }

        fn closed(&self) -> Vec<String> {
            self.closed.lock().unwrap().clone()
        }
    }

    impl VmBackend for EgressBackend {
        fn ensure_singleton(&self, _: &ApplianceConfig) -> Result<(), VmBackendError> {
            Ok(())
        }
        fn state(&self, _: &ApplianceConfig) -> Result<DomainState, VmBackendError> {
            Ok(self.state)
        }
        fn start(&self, _: &ApplianceConfig) -> Result<(), VmBackendError> {
            Ok(())
        }
        fn retire_installation_media(&self, _: &ApplianceConfig) -> Result<(), VmBackendError> {
            Ok(())
        }
        fn close_egress(
            &self,
            _: &ApplianceConfig,
            lease: &EgressLease,
        ) -> Result<(), VmBackendError> {
            if self.detach_fails {
                return Err(VmBackendError::UnsupportedAction("close_egress"));
            }
            self.closed.lock().unwrap().push(lease.id.clone());
            Ok(())
        }
    }

    fn config() -> ApplianceConfig {
        ApplianceConfig::recommended(
            "00000000-0000-4000-8000-000000000001",
            "/var/lib/tilo/windows/system.qcow2",
            "/var/lib/tilo/windows/nvram.fd",
        )
    }

    /// Writes a record exactly as `write_lease` does.
    fn write_record(root: &Path, id: &str, expires: u64) {
        std::fs::write(
            root.join(format!("{id}.json")),
            format!(
                "{{\"schema_version\":1,\"id\":\"{id}\",\"purpose\":\"windows_update\",\"expires_unix_seconds\":{expires}}}\n"
            ),
        )
        .unwrap();
    }

    fn entries(root: &Path) -> usize {
        scan_lease_records(root).unwrap().entries
    }

    /// The schedule right after a clean pass at `at`.
    fn after_clean_pass(at: u64) -> ReaperSchedule {
        let mut schedule = ReaperSchedule::default();
        schedule.after_pass(&Ok(ReapPass::default()), at);
        schedule
    }

    #[test]
    fn a_lease_opened_while_the_reaper_sleeps_is_reaped_at_its_own_expiry() {
        let root = tempfile::tempdir().unwrap();
        let backend = EgressBackend::new(false, DomainState::Running);
        // The reaper passed at 1000 and next refreshes status at 1030. A lease
        // opened at 1001 with a 10 s lifetime must not wait for that refresh.
        let schedule = after_clean_pass(1_000);
        assert_eq!(STATUS_REFRESH_SECONDS, 30);
        assert_eq!(schedule.next_pass, 1_030);
        write_record(root.path(), "egress-windows_update-1001", 1_011);

        let scan = scan_lease_records(root.path()).unwrap();
        assert_eq!(scan.expiries, [1_011]);
        assert_eq!(schedule.on_wake(&scan, 1_010), Wake::Sleep);
        assert_eq!(
            schedule.on_wake(&scan, 1_011),
            Wake::Pass { give_up: false }
        );

        let pass = reap_lease_records(&backend, &config(), root.path(), 1_011).unwrap();
        assert!(pass.closed_any);
        assert_eq!(backend.closed(), ["egress-windows_update-1001"]);
        let mut schedule = schedule;
        schedule.after_pass(&Ok(pass), 1_011);
        assert_eq!(
            schedule.on_wake(&scan_lease_records(root.path()).unwrap(), 1_011),
            Wake::Idle
        );
    }

    #[test]
    fn a_live_lease_keeps_its_nic_and_its_record() {
        let root = tempfile::tempdir().unwrap();
        write_record(root.path(), "egress-windows_update-1000", 1_600);
        let backend = EgressBackend::new(false, DomainState::Running);

        let pass = reap_lease_records(&backend, &config(), root.path(), 1_599).unwrap();

        assert_eq!(pass, ReapPass::default());
        assert!(backend.closed().is_empty());
        assert_eq!(entries(root.path()), 1);
    }

    #[test]
    fn a_shut_off_domain_has_no_egress_nic_left_to_detach() {
        let root = tempfile::tempdir().unwrap();
        write_record(root.path(), "egress-windows_update-1000", 1_600);
        // `detach-interface --live` fails for good on a domain that is not
        // running, but its live-only NIC went with it.
        let backend = EgressBackend::new(true, DomainState::ShutOff);

        let pass = reap_lease_records(&backend, &config(), root.path(), 1_600).unwrap();

        assert!(pass.closed_any);
        assert!(pass.failed.is_empty());
        assert_eq!(entries(root.path()), 0);
    }

    #[test]
    fn a_detach_that_keeps_failing_backs_off_then_is_set_aside_and_the_reaper_stops() {
        let root = tempfile::tempdir().unwrap();
        let unretired = tempfile::tempdir().unwrap();
        write_record(root.path(), "egress-windows_update-1000", 1_600);
        let backend = EgressBackend::new(true, DomainState::Running);
        let mut schedule = ReaperSchedule::default();
        let mut now: u64 = 1_600;
        let mut delays = Vec::new();
        let mut passes: u32 = 0;

        loop {
            let scan = scan_lease_records(root.path()).unwrap();
            match schedule.on_wake(&scan, now) {
                Wake::Idle => break,
                Wake::Sleep => now = now.saturating_add(1),
                Wake::Pass { give_up } => {
                    passes = passes.saturating_add(1);
                    let mut pass =
                        reap_lease_records(&backend, &config(), root.path(), now).unwrap();
                    assert_eq!(pass.failed.len(), 1, "the record stays until set aside");
                    if give_up {
                        set_aside(&pass.failed, unretired.path()).unwrap();
                        pass.failed.clear();
                    }
                    schedule.after_pass(&Ok(pass), now);
                    delays.push(schedule.next_pass.saturating_sub(now));
                }
            }
            assert!(now < 2_800, "the reaper never stopped");
        }

        assert_eq!(
            (
                REAP_RETRY_SECONDS,
                REAP_RETRY_MAX_SECONDS,
                REAP_GIVE_UP_SECONDS
            ),
            (5, 60, 600)
        );
        assert_eq!(delays[..5], [5, 10, 20, 40, 60]);
        assert!(passes < 20, "{passes} passes polled the failing detach");
        assert!(
            now >= 2_200,
            "gave up at {now}, before ten minutes of failures"
        );
        assert_eq!(entries(root.path()), 0);
        assert!(
            unretired
                .path()
                .join("egress-windows_update-1000.json")
                .exists()
        );
        assert!(
            schedule.last_error.is_none(),
            "setting the record aside ends the failure run"
        );
    }

    #[test]
    fn a_set_aside_record_is_retried_without_a_schedule() {
        let unretired = tempfile::tempdir().unwrap();
        write_record(unretired.path(), "egress-windows_update-1000", 1_600);
        let backend = EgressBackend::new(false, DomainState::Running);

        // `reap_leases` passes `u64::MAX`: every set-aside record is expired.
        let pass = reap_lease_records(&backend, &config(), unretired.path(), u64::MAX).unwrap();

        assert!(pass.closed_any);
        assert_eq!(backend.closed(), ["egress-windows_update-1000"]);
        assert_eq!(entries(unretired.path()), 0);
    }

    #[test]
    fn unreadable_records_fail_closed_and_debris_cannot_keep_the_reaper_alive() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("egress-windows_update-1000.json"),
            "{\"schema_version\":1",
        )
        .unwrap();
        std::fs::write(root.path().join("notes"), "").unwrap();
        std::fs::create_dir(root.path().join("egress-dir.json")).unwrap();
        let backend = EgressBackend::new(false, DomainState::Running);
        let schedule = after_clean_pass(999);
        let scan = scan_lease_records(root.path()).unwrap();
        assert_eq!(
            scan,
            LeaseScan {
                entries: 3,
                expiries: Vec::new(),
            }
        );

        let pass = reap_lease_records(&backend, &config(), root.path(), 1_000).unwrap();

        // The damaged record's expiry is unknown, so its NIC is detached now.
        assert_eq!(backend.closed(), ["egress-unreadable-record"]);
        assert!(pass.failed.is_empty());
        assert_eq!(
            schedule.on_wake(&scan_lease_records(root.path()).unwrap(), 1_000),
            Wake::Idle
        );
    }

    #[test]
    fn records_without_an_appliance_are_dropped_and_the_reaper_goes_idle() {
        let root = tempfile::tempdir().unwrap();
        write_record(root.path(), "egress-windows_update-1000", 1_600);
        std::fs::write(root.path().join("stray"), "").unwrap();

        discard_records(root.path()).unwrap();

        assert_eq!(
            after_clean_pass(1_000).on_wake(&scan_lease_records(root.path()).unwrap(), 1_000),
            Wake::Idle
        );
    }

    #[test]
    fn a_failed_pass_with_nothing_left_is_reported_not_retried() {
        let root = tempfile::tempdir().unwrap();
        let mut schedule = ReaperSchedule::default();
        assert_eq!(
            schedule.on_wake(&LeaseScan::default(), 1_000),
            Wake::Pass { give_up: false },
            "the first wake always runs a pass"
        );
        schedule.after_pass(&Err("cannot lock Windows controller".to_string()), 1_000);

        assert_eq!(
            schedule.on_wake(&scan_lease_records(root.path()).unwrap(), 1_000),
            Wake::Idle
        );
        assert_eq!(
            schedule.last_error.as_deref(),
            Some("cannot lock Windows controller")
        );
    }

    #[test]
    fn every_lease_id_egress_open_creates_has_a_record_path() {
        for purpose in [EgressPurpose::Activation, EgressPurpose::WindowsUpdate] {
            // The ID `LibvirtBackend::open_egress` returns for this purpose.
            let id = format!("egress-{}-1000", purpose.as_str());
            assert!(lease_path(&id).is_ok(), "{id} has no lease record path");
        }
        assert!(lease_path("egress-pending-42").is_ok());
        assert!(lease_path("egress-../leases").is_err());
        assert!(lease_path("egress-a/b").is_err());
        assert!(lease_path("windows_update-1").is_err());
    }

    #[test]
    fn lease_record_parses_what_egress_open_writes() {
        let lease = parse_lease_record(
            "{\"schema_version\":1,\"id\":\"egress-activation-7\",\"purpose\":\"activation\",\"expires_unix_seconds\":907}\n",
        )
        .unwrap();
        assert_eq!(lease.id, "egress-activation-7");
        assert_eq!(lease.purpose, EgressPurpose::Activation);
        assert_eq!(lease.expires_unix_seconds, 907);
        assert!(
            parse_lease_record(
                "{\"id\":\"egress-x\",\"purpose\":\"other\",\"expires_unix_seconds\":1}"
            )
            .is_err()
        );
    }
}
