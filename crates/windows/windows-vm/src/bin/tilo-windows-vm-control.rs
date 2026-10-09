use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::ExitCode;

use windows_vm::{
    ApplianceConfig, DomainState, EgressLease, EgressPurpose, GraphicsAssignment,
    GraphicsCapabilities, LibvirtBackend, StdCommandRunner, VmBackend,
};

const ROOT: &str = "/var/lib/tilo/windows";
const UUID_FILE: &str = "/var/lib/tilo/windows/domain.uuid";
const SYSTEM_DISK: &str = "/var/lib/tilo/windows/system.qcow2";
const NVRAM: &str = "/var/lib/tilo/windows/nvram.fd";
// Keep controller egress state apart from per-session portal-file lease
// projections below `$XDG_RUNTIME_DIR/tilo/windows-vm`.
const EGRESS_RUNTIME_ROOT: &str = "/run/tilo/windows-egress";
const LEASE_ROOT: &str = "/run/tilo/windows-egress/leases";
const EGRESS_SUMMARY: &str = "/run/tilo/windows-egress/summary.json";
const STATUS_SUMMARY: &str = "/run/tilo/windows-egress/status.json";
const CONTROLLER_LOCK: &str = "/run/tilo/windows-egress/controller.lock";
const GRAPHICS_ASSIGNMENT: &str = "/var/lib/tilo/windows/graphics-assignment.json";

fn main() -> ExitCode {
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

    // The reaper also runs without a provisioned appliance, so it is
    // dispatched before the domain identity is required.
    if action == "egress-reap" {
        return match rest.as_slice() {
            [] => reap_pass().map(|_| ()),
            [flag] if flag == "--until-idle" => reap_until_idle(),
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
            reap_leases(&backend, &config)?;
            if active_lease()?.is_some() {
                return Err("an egress lease is already active".to_string());
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
    "usage: tilo-windows-vm-control {status|graphics-capabilities|graphics-attach|graphics-detach|define|start|shutdown|force-shutdown|restart|suspend|resume|egress-open|egress-close|egress-revoke|egress-reap [--until-idle]}".to_string()
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

fn lease_path(id: &str) -> Result<std::path::PathBuf, String> {
    if !id.starts_with("egress-")
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
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
    let entries = match fs::read_dir(LEASE_ROOT) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot inspect egress leases: {error}")),
    };
    let mut active = None;
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot inspect egress lease: {error}"))?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let source = fs::read_to_string(&path)
            .map_err(|error| format!("cannot read egress lease: {error}"))?;
        let value: serde_json::Value = serde_json::from_str(&source)
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
        let expiry = value
            .get("expires_unix_seconds")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "an egress lease has an invalid expiry".to_string())?;
        let expected_path = lease_path(id)?;
        if expected_path != path {
            return Err("an egress lease is stored under an unexpected name".to_string());
        }
        if active.replace((id.to_string(), purpose, expiry)).is_some() {
            return Err("more than one Windows egress lease is active".to_string());
        }
    }
    Ok(active)
}

/// Retire every expired lease record and report the earliest Unix time at
/// which one of the records that remain needs another pass.
fn reap_leases<R: VmBackend>(backend: &R, config: &ApplianceConfig) -> Result<Option<u64>, String> {
    let now = unix_now()?;
    let pass = reap_lease_records(backend, config, Path::new(LEASE_ROOT), now)?;
    if pass.closed_any || active_lease()?.is_none() {
        remove_egress_summary();
    }
    Ok(pass.next_attention)
}

/// How long a record whose NIC could not be detached, or which could not be
/// read, waits before the reaper tries again.
const REAP_RETRY_SECONDS: u64 = 5;

#[derive(Debug, Default, Eq, PartialEq)]
struct ReapPass {
    closed_any: bool,
    /// Earliest Unix time at which a remaining record needs another pass:
    /// the expiry of a live lease, or a retry for a record that could not be
    /// retired. `None` once no record remains.
    next_attention: Option<u64>,
}

impl ReapPass {
    fn attend_at(&mut self, unix_seconds: u64) {
        self.next_attention = Some(
            self.next_attention
                .map_or(unix_seconds, |current| current.min(unix_seconds)),
        );
    }
}

fn reap_lease_records<R: VmBackend>(
    backend: &R,
    config: &ApplianceConfig,
    root: &Path,
    now: u64,
) -> Result<ReapPass, String> {
    let mut pass = ReapPass::default();
    let retry = now.saturating_add(REAP_RETRY_SECONDS);
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(pass),
        Err(error) => return Err(format!("cannot inspect egress leases: {error}")),
    };
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot inspect egress lease: {error}"))?;
        let path = entry.path();
        // A record that cannot be read or parsed may still stand for a live
        // NIC; keep it scheduled rather than forgetting it. Any other entry
        // is kept scheduled too: the path unit that starts the reaper fires
        // while the directory is not empty, so a reaper that went idle here
        // would only be started again at once.
        let Some(lease) = (path.extension().and_then(|ext| ext.to_str()) == Some("json"))
            .then(|| fs::read_to_string(&path).ok())
            .flatten()
            .and_then(|source| parse_lease_record(&source))
        else {
            pass.attend_at(retry);
            continue;
        };
        if lease.expires_unix_seconds > now {
            pass.attend_at(lease.expires_unix_seconds);
            continue;
        }
        if backend.close_egress(config, &lease).is_ok() {
            let _ = fs::remove_file(path);
            pass.closed_any = true;
        } else {
            pass.attend_at(retry);
        }
    }
    Ok(pass)
}

/// Parse a controller-written lease record leniently, so one malformed field
/// elsewhere cannot stop the reaper from closing the NIC it describes.
fn parse_lease_record(source: &str) -> Option<EgressLease> {
    let id = source
        .split("\"id\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())?;
    let purpose = match source
        .split("\"purpose\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())?
    {
        "activation" => EgressPurpose::Activation,
        "windows_update" => EgressPurpose::WindowsUpdate,
        _ => return None,
    };
    let expires_unix_seconds = source
        .split("\"expires_unix_seconds\":")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|value| value.parse::<u64>().ok())?;
    Some(EgressLease {
        id: id.to_string(),
        purpose,
        expires_unix_seconds,
    })
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
/// status summary. Returns when the remaining lease records next need a pass.
fn reap_pass() -> Result<Option<u64>, String> {
    let _lock = acquire_controller_lock()?;
    if !Path::new(UUID_FILE).exists() {
        // Without an appliance there is no domain to query; drop any public
        // status a failed or replaced appliance left behind. A lease record
        // cannot be retired without its domain, so keep it scheduled.
        remove_egress_summary();
        let _ = fs::remove_file(STATUS_SUMMARY);
        return if lease_records_exist()? {
            Ok(Some(unix_now()?.saturating_add(REAP_RETRY_SECONDS)))
        } else {
            Ok(None)
        };
    }
    let (config, backend) = appliance()?;
    let next_attention = reap_leases(&backend, &config)?;
    write_status_summary(&backend, &config)?;
    Ok(next_attention)
}

/// The status summary is refreshed at least this often while a lease keeps
/// the reaper running, matching the persistent timer's period.
const STATUS_REFRESH_SECONDS: u64 = 30;

/// Run while any lease record exists, waking at each lease's expiry. The
/// `tilo-windows-vm-egress-lease.path` unit starts this as soon as a record
/// is written, which is before its NIC is attached, so revocation never
/// waits for a polling interval. The process does not exit while a record
/// remains: the path unit would only start it again at once.
fn reap_until_idle() -> Result<(), String> {
    loop {
        let next_attention = match reap_pass() {
            Ok(next_attention) => next_attention,
            Err(error) => {
                if !lease_records_exist()? {
                    return Err(error);
                }
                eprintln!("tilo-windows-vm-control: egress reaper pass failed, retrying: {error}");
                Some(unix_now()?.saturating_add(REAP_RETRY_SECONDS))
            }
        };
        let Some(next_attention) = next_attention else {
            return Ok(());
        };
        let refresh = unix_now()?.saturating_add(STATUS_REFRESH_SECONDS);
        sleep_until_unix(next_attention.min(refresh))?;
    }
}

/// Longest single sleep. A monotonic sleep stops counting while the machine
/// is suspended but a lease's wall-clock expiry does not, so the wall clock
/// is re-read at least this often: an expiry that passed during suspend is
/// acted on within this bound after resume.
const MAX_SLEEP_SECONDS: u64 = 5;

fn sleep_until_unix(deadline: u64) -> Result<(), String> {
    while let Some(step) = sleep_step(unix_now()?, deadline) {
        std::thread::sleep(step);
    }
    Ok(())
}

fn sleep_step(now: u64, deadline: u64) -> Option<std::time::Duration> {
    let remaining = deadline
        .checked_sub(now)
        .filter(|remaining| *remaining > 0)?;
    Some(std::time::Duration::from_secs(
        remaining.min(MAX_SLEEP_SECONDS),
    ))
}

fn lease_records_exist() -> Result<bool, String> {
    match fs::read_dir(LEASE_ROOT) {
        Ok(mut entries) => Ok(entries.next().is_some()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("cannot inspect egress leases: {error}")),
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

    use std::sync::Mutex;
    use std::time::Duration;

    use windows_vm::{
        ApplianceConfig, DomainState, EgressLease, EgressPurpose, VmBackend, VmBackendError,
    };

    use super::{
        MAX_SLEEP_SECONDS, REAP_RETRY_SECONDS, ReapPass, parse_lease_record, reap_lease_records,
        sleep_step,
    };

    /// Records which leases the reaper detached; detaching fails on demand.
    #[derive(Default)]
    struct EgressBackend {
        detach_fails: bool,
        closed: Mutex<Vec<String>>,
    }

    impl VmBackend for EgressBackend {
        fn ensure_singleton(&self, _: &ApplianceConfig) -> Result<(), VmBackendError> {
            Ok(())
        }
        fn state(&self, _: &ApplianceConfig) -> Result<DomainState, VmBackendError> {
            Ok(DomainState::Running)
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

    fn write_record(root: &std::path::Path, id: &str, expires: u64) {
        std::fs::write(
            root.join(format!("{id}.json")),
            format!(
                "{{\"schema_version\":1,\"id\":\"{id}\",\"purpose\":\"windows_update\",\"expires_unix_seconds\":{expires}}}\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn reaper_sleeps_until_a_live_lease_expires_instead_of_polling() {
        let root = tempfile::tempdir().unwrap();
        write_record(root.path(), "egress-windows_update-1000", 1_600);
        let backend = EgressBackend::default();

        let pass = reap_lease_records(&backend, &config(), root.path(), 1_000).unwrap();

        assert_eq!(
            pass,
            ReapPass {
                closed_any: false,
                next_attention: Some(1_600),
            }
        );
        assert!(backend.closed.lock().unwrap().is_empty());
        assert!(root.path().join("egress-windows_update-1000.json").exists());
    }

    #[test]
    fn reaper_detaches_an_expired_lease_and_goes_idle() {
        let root = tempfile::tempdir().unwrap();
        write_record(root.path(), "egress-windows_update-1000", 1_600);
        let backend = EgressBackend::default();

        let pass = reap_lease_records(&backend, &config(), root.path(), 1_600).unwrap();

        assert_eq!(
            pass,
            ReapPass {
                closed_any: true,
                next_attention: None,
            }
        );
        assert_eq!(
            *backend.closed.lock().unwrap(),
            ["egress-windows_update-1000"]
        );
        assert!(!root.path().join("egress-windows_update-1000.json").exists());
    }

    #[test]
    fn reaper_retries_a_lease_whose_detach_failed() {
        let root = tempfile::tempdir().unwrap();
        write_record(root.path(), "egress-windows_update-1000", 1_600);
        let backend = EgressBackend {
            detach_fails: true,
            ..EgressBackend::default()
        };

        let pass = reap_lease_records(&backend, &config(), root.path(), 2_000).unwrap();

        assert_eq!(pass.next_attention, Some(2_000 + REAP_RETRY_SECONDS));
        assert!(!pass.closed_any);
        assert!(root.path().join("egress-windows_update-1000.json").exists());
    }

    #[test]
    fn reaper_keeps_an_unreadable_record_scheduled() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("egress-torn.json"),
            "{\"schema_version\":1",
        )
        .unwrap();
        std::fs::write(root.path().join("stray"), "").unwrap();
        let backend = EgressBackend::default();

        let pass = reap_lease_records(&backend, &config(), root.path(), 1_000).unwrap();

        assert_eq!(pass.next_attention, Some(1_000 + REAP_RETRY_SECONDS));
        assert!(backend.closed.lock().unwrap().is_empty());
    }

    #[test]
    fn reaper_is_idle_without_a_lease_directory() {
        let root = tempfile::tempdir().unwrap();
        let backend = EgressBackend::default();

        let pass =
            reap_lease_records(&backend, &config(), &root.path().join("leases"), 1_000).unwrap();

        assert_eq!(pass, ReapPass::default());
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
        assert!(parse_lease_record("{\"id\":\"egress-x\",\"purpose\":\"other\"}").is_none());
    }

    #[test]
    fn sleeps_are_bounded_so_an_expiry_passed_in_suspend_is_seen() {
        assert_eq!(sleep_step(1_000, 1_000), None);
        assert_eq!(sleep_step(1_001, 1_000), None);
        assert_eq!(sleep_step(1_000, 1_002), Some(Duration::from_secs(2)));
        assert_eq!(
            sleep_step(1_000, 1_600),
            Some(Duration::from_secs(MAX_SLEEP_SECONDS))
        );
    }
}
