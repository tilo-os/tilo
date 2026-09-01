use std::env;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use std::time::{SystemTime, UNIX_EPOCH};

use windows_vm::{
    ApplianceConfig, DomainState, EgressLease, EgressPurpose, LibvirtBackend, StdCommandRunner,
    VmBackend,
};

const ROOT: &str = "/var/lib/qos/windows";
const UUID_FILE: &str = "/var/lib/qos/windows/domain.uuid";
const SYSTEM_DISK: &str = "/var/lib/qos/windows/system.qcow2";
const NVRAM: &str = "/var/lib/qos/windows/nvram.fd";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("qos-windows-vm-control: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut arguments = env::args_os().skip(1);
    let action = arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(usage)?;
    let rest = arguments
        .map(|value| value.into_string().map_err(|_| usage()))
        .collect::<Result<Vec<_>, _>>()?;

    require_regular_file(Path::new(UUID_FILE), "domain identity")?;
    require_regular_file(Path::new(SYSTEM_DISK), "system disk")?;
    let domain_uuid = fs::read_to_string(UUID_FILE)
        .map_err(|error| format!("cannot read the protected domain identity: {error}"))?;
    let config = ApplianceConfig::recommended(domain_uuid.trim(), SYSTEM_DISK, NVRAM);
    let backend = LibvirtBackend::new(StdCommandRunner);

    if !matches!(action.as_str(), "egress-open" | "egress-close") && !rest.is_empty() {
        return Err(usage());
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
            let lease = backend.open_egress(&config, purpose, now, ttl).map_err(|e| e.to_string())?;
            println!("{{\"schema_version\":1,\"id\":\"{}\",\"purpose\":\"{}\",\"expires_unix_seconds\":{}}}", lease.id, lease.purpose.as_str(), lease.expires_unix_seconds);
            Ok(())
        }
        "egress-close" => {
            let id = rest.first().cloned().ok_or_else(|| "usage: qos-windows-vm-control egress-close <lease-id>".to_string())?;
            if rest.len() != 1 { return Err(usage()); }
            let lease = EgressLease { id, purpose: EgressPurpose::WindowsUpdate, expires_unix_seconds: 0 };
            backend.close_egress(&config, &lease).map_err(|e| e.to_string())
        }
        _ => return Err(usage()),
    } .map_err(|error| format!("cannot manage the singleton appliance: {error}"))?;

    Ok(())
}

fn usage() -> String {
    "usage: qos-windows-vm-control {status|define|start|shutdown|force-shutdown|restart|suspend|resume|egress-open|egress-close}".to_string()
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
