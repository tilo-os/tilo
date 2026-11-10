//! `tilo-hosted-jail`: the one privileged step of a hosted-VM host. See
//! `legacy_vm::hosted::jail` for what it does and how it is installed.
//!
//! ```text
//! tilo-hosted-jail [--config FILE] start --slot N --vcpus V --memory-mib M (--boot|--restore)
//!                                   [--relay-endpoint ADDR:PORT]
//!                                   (descriptors 3, 4, 5: the VM's state, sockets, guest)
//! tilo-hosted-jail [--config FILE] stop --slot N
//! tilo-hosted-jail [--config FILE] release --slot N   (descriptors 3, 4)
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("tilo-hosted-jail: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "linux")]
fn run(args: Vec<String>) -> Result<(), String> {
    use legacy_vm::hosted::jail::{DEFAULT_CONFIG, JailConfig, privileged};
    use legacy_vm::hosted::network::Endpoint;

    let mut config = PathBuf::from(DEFAULT_CONFIG);
    let mut verb = None;
    let (mut slot, mut vcpus, mut memory, mut boot) = (None, None, None, None);
    let mut relay = None;
    let mut args = args.into_iter();
    let number = |value: Option<String>, flag: &str| -> Result<u32, String> {
        value
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| format!("{flag} takes a number"))
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config = PathBuf::from(args.next().ok_or("--config takes a file")?),
            "--slot" => slot = Some(number(args.next(), "--slot")?),
            "--vcpus" => vcpus = Some(number(args.next(), "--vcpus")?),
            "--memory-mib" => memory = Some(number(args.next(), "--memory-mib")?),
            "--relay-endpoint" => {
                let text = args.next().ok_or("--relay-endpoint takes ADDR:PORT")?;
                relay = Some(Endpoint::parse(&text)?);
            }
            "--boot" => boot = Some(true),
            "--restore" => boot = Some(false),
            "start" | "stop" | "release" if verb.is_none() => verb = Some(arg),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    let config = JailConfig::load(&config)?;
    let slot = slot.ok_or("--slot is required")?;
    if slot >= config.slots {
        return Err(format!(
            "slot {slot} is not one of the host's {}",
            config.slots
        ));
    }
    privileged::enter(&config).map_err(|e| e.to_string())?;
    match verb.as_deref() {
        Some("start") => {
            let start = privileged::Start {
                slot,
                vcpus: vcpus.ok_or("--vcpus is required")?.clamp(1, 64),
                memory_mib: memory
                    .ok_or("--memory-mib is required")?
                    .clamp(128, 256 * 1024),
                boot: boot.ok_or("--boot or --restore is required")?,
                relay,
            };
            Err(privileged::start(&config, &start).to_string())
        }
        Some("stop") => privileged::stop(&config, slot).map_err(|e| e.to_string()),
        Some("release") => privileged::release(&config, slot).map_err(|e| e.to_string()),
        _ => Err("start, stop or release".into()),
    }
}

#[cfg(not(target_os = "linux"))]
fn run(_: Vec<String>) -> Result<(), String> {
    Err("hosted VMs run on Linux".into())
}
