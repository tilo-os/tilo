use std::env;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use windows_vm::{ApplianceConfig, LibvirtBackend, StdCommandRunner, VmBackend};

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
    let mut arguments = env::args_os();
    let _program = arguments.next();
    let action = arguments.next().and_then(|value| value.into_string().ok());
    if arguments.next().is_some() || !matches!(action.as_deref(), Some("define" | "start")) {
        return Err("usage: qos-windows-vm-control {define|start}".to_string());
    }

    require_regular_file(Path::new(UUID_FILE), "domain identity")?;
    require_regular_file(Path::new(SYSTEM_DISK), "system disk")?;
    let domain_uuid = fs::read_to_string(UUID_FILE)
        .map_err(|error| format!("cannot read the protected domain identity: {error}"))?;
    let config = ApplianceConfig::recommended(domain_uuid.trim(), SYSTEM_DISK, NVRAM);
    let backend = LibvirtBackend::new(StdCommandRunner);

    let result = if action.as_deref() == Some("define") {
        backend.ensure_singleton(&config)
    } else {
        backend.start(&config)
    };
    result.map_err(|error| format!("cannot manage the singleton appliance: {error}"))?;

    Ok(())
}

fn require_regular_file(path: &Path, label: &str) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("missing or unreadable {label} below {ROOT}: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err(format!("{label} below {ROOT} is not a regular file"));
    }
    Ok(())
}
