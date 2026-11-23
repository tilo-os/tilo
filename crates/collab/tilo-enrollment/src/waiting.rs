//! One machine asking to join, and who answers it.
//!
//! A device that serves enrollments puts each machine that dials it to its
//! person before anything is admitted. This is that ask: what the machine
//! calls itself, what it is and the code both sides compute, and the person's
//! answer — at the console, or on their own devices through a session.

use anyhow::bail;
use serde::Serialize;

use tilo_identity::certification_ceremony::pairing_code;

/// One machine asking to join, as the person's device shows it: what it calls
/// itself, what it is, and the code both sides compute.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WaitingMachine {
    /// Names this ask in the person's answer.
    pub ask: String,
    pub device: String,
    pub service: String,
    pub label: String,
    pub hardware: String,
    pub code: String,
    /// The token's nonce, when the machine was handed one: what a planned
    /// machine's entry is known by.
    #[serde(default)]
    pub nonce: String,
}

impl WaitingMachine {
    #[must_use]
    pub fn of(device: [u8; 32], service: [u8; 32], label: &str, hardware: &str) -> Self {
        let mut digest = blake3::Hasher::new();
        digest.update(b"tilo-enrollment-ask-v1");
        digest.update(&device);
        digest.update(&service);
        let ask = hex::encode(&digest.finalize().as_bytes()[..16]);
        Self {
            ask,
            device: hex::encode(device),
            service: hex::encode(service),
            label: label.to_owned(),
            hardware: hardware.to_owned(),
            code: pairing_code(device, service),
            nonce: String::new(),
        }
    }

    /// The ask of a machine that came with a token, named by the token's
    /// nonce as well.
    #[must_use]
    pub fn with_nonce(mut self, nonce: [u8; 16]) -> Self {
        self.nonce = hex::encode(nonce);
        self
    }
}

/// The person's answer to one machine asking to join.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Decline,
}

/// Who answers the machines asking to join: the person at the console, or a
/// session that shows them on the person's own devices.
pub trait Person {
    /// Put `machine` to the person and answer what they said.
    ///
    /// # Errors
    ///
    /// When the answer cannot be read; serving then stops.
    fn decide(&mut self, machine: &WaitingMachine) -> anyhow::Result<Decision>;
}

/// The person at the console: one JSON line per machine, then their answer on
/// standard input (`approve <ask>`, `decline <ask>`).
pub struct Console;

impl Person for Console {
    fn decide(&mut self, machine: &WaitingMachine) -> anyhow::Result<Decision> {
        use std::io::Write as _;
        println!("{}", serde_json::to_string(machine)?);
        std::io::stdout().flush()?;
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            bail!("the person's answers stopped");
        }
        let mut words = line.split_whitespace();
        let decision = match words.next() {
            Some("approve") => Decision::Approve,
            Some("decline") => Decision::Decline,
            answer => bail!(
                "answer a machine with `approve <ask>` or `decline <ask>`, not {:?}",
                answer.unwrap_or("")
            ),
        };
        if words.next().is_some_and(|ask| ask != machine.ask) {
            bail!("the answer names another machine than the one waiting");
        }
        println!(
            "{}",
            serde_json::json!({
                "ask": machine.ask,
                "approved": decision == Decision::Approve,
            })
        );
        std::io::stdout().flush()?;
        Ok(decision)
    }
}

/// What this machine is, as the person's devices show it beside its name: its
/// processors, memory, first disk and system, each only when this machine can
/// say it. Bounded, so a machine cannot flood the sheet.
#[must_use]
pub fn hardware_summary() -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(cores) = processors() {
        parts.push(format!("{cores} cores"));
    }
    if let Some(gigabytes) = memory_gigabytes() {
        parts.push(format!("{gigabytes} GB memory"));
    }
    if let Some(gigabytes) = first_disk_gigabytes() {
        parts.push(format!("{gigabytes} GB disk"));
    }
    if let Some(system) = system_name() {
        parts.push(system);
    }
    parts.join(" · ").chars().take(200).collect()
}

fn processors() -> Option<u32> {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    let cores = cpuinfo
        .lines()
        .filter(|line| line.starts_with("processor"))
        .count();
    u32::try_from(cores).ok().filter(|cores| *cores > 0)
}

fn memory_gigabytes() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kibibytes: u64 = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    Some(kibibytes / 1_000_000)
}

fn first_disk_gigabytes() -> Option<u64> {
    for entry in std::fs::read_dir("/sys/block").ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Loop and optical devices are not the machine's disk.
        if name.starts_with("loop") || name.starts_with("sr") || name.starts_with("ram") {
            continue;
        }
        let Ok(sectors) = std::fs::read_to_string(entry.path().join("size")) else {
            continue;
        };
        let Ok(sectors) = sectors.trim().parse::<u64>() else {
            continue;
        };
        // A sector is 512 bytes; a gigabyte is 10^9 bytes here.
        let gigabytes = sectors.saturating_mul(512) / 1_000_000_000;
        if gigabytes > 0 {
            return Some(gigabytes);
        }
    }
    None
}

fn system_name() -> Option<String> {
    let release = std::fs::read_to_string("/etc/os-release").ok()?;
    release.lines().find_map(|line| {
        line.strip_prefix("PRETTY_NAME=")
            .map(|name| name.trim_matches('"').to_owned())
    })
}
