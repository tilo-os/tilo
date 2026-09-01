use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::ExitCode;

use std::time::{SystemTime, UNIX_EPOCH};

use windows_vm::{
    ApplianceConfig, DomainState, EgressLease, EgressPurpose, GraphicsAssignment,
    GraphicsCapabilities, LibvirtBackend, StdCommandRunner, VmBackend,
};

const ROOT: &str = "/var/lib/qos/windows";
const UUID_FILE: &str = "/var/lib/qos/windows/domain.uuid";
const SYSTEM_DISK: &str = "/var/lib/qos/windows/system.qcow2";
const NVRAM: &str = "/var/lib/qos/windows/nvram.fd";
const LEASE_ROOT: &str = "/run/qos/windows-vm/leases";
const GRAPHICS_ASSIGNMENT: &str = "/var/lib/qos/windows/graphics-assignment.json";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("qos-windows-vm-control: {message}");
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
    // never creates a VF/mdev or unbinds the GPU currently displaying qOS.
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

    // The timer is installed on every qOS host. Before the singleton is
    // provisioned there can be no lease and no domain identity to inspect.
    if action == "egress-reap" && !Path::new(LEASE_ROOT).exists() {
        return Ok(());
    }

    require_regular_file(Path::new(UUID_FILE), "domain identity")?;
    require_regular_file(Path::new(SYSTEM_DISK), "system disk")?;
    let domain_uuid = fs::read_to_string(UUID_FILE)
        .map_err(|error| format!("cannot read the protected domain identity: {error}"))?;
    let config = ApplianceConfig::recommended(domain_uuid.trim(), SYSTEM_DISK, NVRAM);
    let backend = LibvirtBackend::new(StdCommandRunner);

    if !matches!(
        action.as_str(),
        "egress-open" | "egress-close" | "graphics-attach"
    ) && !rest.is_empty()
    {
        return Err(usage());
    }

    // A live-only NIC disappears on a domain restart, so retire its lease
    // records before any power transition. This also makes a later retry
    // unable to resurrect an already-expired capability.
    if matches!(
        action.as_str(),
        "shutdown" | "force-shutdown" | "restart" | "suspend"
    ) {
        close_all_leases(&backend, &config);
    }

    match action.as_str() {
        "status" => {
            backend.state(&config).map(|state| {
                println!("{{\"schema_version\":1,\"domain\":\"{}\"}}", domain_state_name(state));
            }).map_err(|e| e.to_string())
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
                return Err("usage: qos-windows-vm-control graphics-attach {sriov-vf|mdev} <canonical-id>".to_string());
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
                _ => return Err("usage: qos-windows-vm-control egress-open {activation|windows-update} [ttl-seconds]".to_string()),
            };
            let ttl = match rest.get(1) {
                None => 600,
                Some(value) => value.parse::<u64>().map_err(|_| "invalid egress TTL".to_string())?,
            };
            if rest.len() > 2 { return Err(usage()); }
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "system clock is before the Unix epoch".to_string())?.as_secs();
            let pending_id = format!("egress-pending-{}", std::process::id());
            write_lease(&pending_id, purpose, now.saturating_add(ttl))?;
            let lease = match backend.open_egress(&config, purpose, now, ttl) {
                Ok(lease) => lease,
                Err(error) => { remove_lease(&pending_id); return Err(error.to_string()); }
            };
            let pending_path = lease_path(&pending_id)?;
            let active_path = lease_path(&lease.id)?;
            if active_path.exists() {
                let _ = backend.close_egress(&config, &lease);
                remove_lease(&pending_id);
                return Err("an egress lease is already active".to_string());
            }
            fs::rename(pending_path, active_path).map_err(|error| format!("cannot commit the egress lease: {error}"))?;
            println!("{{\"schema_version\":1,\"id\":\"{}\",\"purpose\":\"{}\",\"expires_unix_seconds\":{}}}", lease.id, lease.purpose.as_str(), lease.expires_unix_seconds);
            Ok(())
        }
        "egress-close" => {
            let id = rest.first().cloned().ok_or_else(|| "usage: qos-windows-vm-control egress-close <lease-id>".to_string())?;
            if rest.len() != 1 { return Err(usage()); }
            let lease = EgressLease { id, purpose: EgressPurpose::WindowsUpdate, expires_unix_seconds: 0 };
            backend.close_egress(&config, &lease).map_err(|e| e.to_string())?;
            remove_lease(&lease.id);
            Ok(())
        }
        "egress-reap" => {
            if !rest.is_empty() { return Err(usage()); }
            reap_leases(&backend, &config)?;
            Ok(())
        }
        _ => return Err(usage()),
    } .map_err(|error| format!("cannot manage the singleton appliance: {error}"))?;

    Ok(())
}

fn usage() -> String {
    "usage: qos-windows-vm-control {status|graphics-capabilities|graphics-attach|graphics-detach|define|start|shutdown|force-shutdown|restart|suspend|resume|egress-open|egress-close|egress-reap}".to_string()
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
    file.write_all(&bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("cannot commit graphics assignment: {error}"))
}

fn read_graphics_assignment() -> Result<GraphicsAssignment, String> {
    let metadata = fs::symlink_metadata(GRAPHICS_ASSIGNMENT)
        .map_err(|_| "no qOS-managed Windows graphics partition is attached".to_string())?;
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
    let bytes = format!("{{\"schema_version\":1,\"id\":\"{id}\",\"purpose\":\"{}\",\"expires_unix_seconds\":{expires}}}\n", purpose.as_str()).into_bytes();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("cannot create egress lease: {error}"))?;
    file.write_all(&bytes)
        .map_err(|error| format!("cannot write egress lease: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("cannot sync egress lease: {error}"))
}

fn remove_lease(id: &str) {
    if let Ok(path) = lease_path(id) {
        let _ = fs::remove_file(path);
    }
}

fn reap_leases<R: VmBackend>(backend: &R, config: &ApplianceConfig) -> Result<(), String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_string())?
        .as_secs();
    let entries = match fs::read_dir(LEASE_ROOT) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("cannot inspect egress leases: {error}")),
    };
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot inspect egress lease: {error}"))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        let Some(id) = source
            .split("\"id\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
        else {
            continue;
        };
        let Some(purpose) = source
            .split("\"purpose\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
        else {
            continue;
        };
        let Some(expiry) = source
            .split("\"expires_unix_seconds\":")
            .nth(1)
            .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|v| v.parse::<u64>().ok())
        else {
            continue;
        };
        if expiry > now {
            continue;
        }
        let purpose = match purpose {
            "activation" => EgressPurpose::Activation,
            "windows_update" => EgressPurpose::WindowsUpdate,
            _ => continue,
        };
        let lease = EgressLease {
            id: id.to_string(),
            purpose,
            expires_unix_seconds: expiry,
        };
        if backend.close_egress(config, &lease).is_ok() {
            let _ = fs::remove_file(path);
        }
    }
    Ok(())
}

fn close_all_leases<R: VmBackend>(backend: &R, config: &ApplianceConfig) {
    let Ok(entries) = fs::read_dir(LEASE_ROOT) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        let Some(id) = source
            .split("\"id\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
        else {
            continue;
        };
        let lease = EgressLease {
            id: id.to_string(),
            purpose: EgressPurpose::WindowsUpdate,
            expires_unix_seconds: 0,
        };
        let _ = backend.close_egress(config, &lease);
        let _ = fs::remove_file(path);
    }
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
