//! Reaching a device where it is now, waking it, and holding it awake. A
//! recorded address is only a hint: every command also looks the device up by
//! name, and its daemon's endpoint is dialed at every address found. While
//! someone drives a machine it is held awake.

use std::fmt::Write as _;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use serde_json::json;
use tilo_device_protocol::{AWAKE_IDLE_SECONDS, Awake, AwakeLease, DEVICE_ACCOUNT, Exec};

use super::{App, Outcome, Resolved};
use crate::config::{DAEMON_PORT, VmEntry};
use crate::error::CliError;
use crate::hypervisor::{DEVICE, SshEndpoint, run_all};
use crate::reach::{self, NetFacts, WakeInfo};
use crate::remote::Remote;

const WAKE_WAIT: Duration = Duration::from_secs(150);
/// `vm wake` sends the packet again this often while it waits: one can be lost.
const RESEND_EVERY: Duration = Duration::from_secs(15);

fn unix_now() -> u64 {
    tilo_clock::unix_now()
}

/// mDNS announces a machine by its host name's first label.
pub fn local_name(hostname: &str) -> String {
    let label = hostname.split('.').next().unwrap_or(hostname);
    format!("{label}.local")
}

fn until(seconds: u64) -> String {
    let now = unix_now();
    let local = i64::try_from(seconds)
        .ok()
        .and_then(|s| jiff::Timestamp::from_second(s).ok())
        .map(|time| time.to_zoned(jiff::tz::TimeZone::system()));
    let left = seconds.saturating_sub(now);
    let at = local.map_or_else(
        || seconds.to_string(),
        |time| {
            if left >= 86_400 {
                time.strftime("%Y-%m-%d %H:%M").to_string()
            } else {
                time.strftime("%H:%M").to_string()
            }
        },
    );
    let left = if left >= 5400 {
        format!("{}h{:02}m", left / 3600, (left % 3600) / 60)
    } else {
        format!("{} min", left.div_ceil(60))
    };
    format!("{at} (in {left})")
}

pub fn describe_lease(lease: &AwakeLease) -> String {
    match (lease.until, lease.held, &lease.problem) {
        (Some(end), true, _) if lease.hold_until.is_some() => {
            format!("held awake until {} (tilo-vm vm awake --for)", until(end))
        }
        (Some(end), true, _) => format!(
            "held awake until {}: {} minutes after the last tilo-vm request",
            until(end),
            AWAKE_IDLE_SECONDS / 60
        ),
        (Some(_), false, Some(problem)) => format!("should be held awake, but is not: {problem}"),
        (_, _, Some(problem)) => format!("not held awake: {problem}"),
        _ => "not held awake: it may suspend when idle; the next tilo-vm request holds it again"
            .into(),
    }
}

fn wake_line(entry: &VmEntry) -> String {
    match (&entry.mac, &entry.wake, &entry.hostname) {
        (Some(mac), Some(wake), hostname) => format!(
            "{}; MAC {mac}{}",
            wake.describe(),
            hostname.as_deref().map_or_else(String::new, |h| format!(
                ", found by name as {}",
                local_name(h)
            ))
        ),
        (Some(mac), None, _) => format!("MAC {mac}; its wake setting is not known"),
        _ => "no MAC recorded: tilo-vm vm wake cannot wake it".into(),
    }
}

impl App {
    /// The address tilo-vm reaches the VM's SSH server at, for `vm ssh`.
    pub(super) fn endpoint(&mut self, vm: &Resolved) -> Result<SshEndpoint, CliError> {
        Self::adapter(vm)?.ssh_endpoint(&vm.vm(), &mut self.runner)
    }

    /// Where the device's daemon may answer: a VM's forwarded port, and a device's recorded
    /// address and wherever its name resolves now, beside the addresses its route records.
    pub(super) fn daemon_addresses(&mut self, vm: &Resolved) -> Vec<String> {
        let device = vm.entry.hypervisor == DEVICE;
        let mut found = Vec::new();
        if let Ok(endpoint) = self.endpoint(vm) {
            let ip = endpoint
                .host
                .parse::<IpAddr>()
                .ok()
                .or_else(|| reach::resolve(&mut self.runner, &endpoint.host));
            let port = if device { DAEMON_PORT } else { endpoint.port };
            found.extend(ip.map(|ip| SocketAddr::new(ip, port)));
        }
        if device && let Some(hostname) = vm.entry.hostname.as_deref() {
            found.extend(
                reach::resolve(&mut self.runner, &local_name(hostname))
                    .map(|ip| SocketAddr::new(ip, DAEMON_PORT)),
            );
        }
        found
            .into_iter()
            .map(|address| address.to_string())
            .collect()
    }

    pub(super) fn update_entry(
        &self,
        name: &str,
        change: impl FnOnce(&mut VmEntry),
    ) -> Result<bool, CliError> {
        let mut registry = self.registry()?;
        let Some(entry) = registry.vms.get_mut(name) else {
            return Ok(false);
        };
        let before = entry.clone();
        change(entry);
        if *entry == before {
            return Ok(false);
        }
        registry.save(&self.registry_path)?;
        Ok(true)
    }

    /// What waking the device needs (its hostname, MAC and wake setting), read on it.
    pub(super) fn net_facts(remote: &mut Remote, vm: &Resolved) -> Option<NetFacts> {
        let host = vm.entry.ssh.as_deref().and_then(SshEndpoint::parse)?.host;
        let stdout = device_sh(remote, &reach::net_probe(&host)).ok()?;
        let facts = reach::parse_net_probe(&stdout);
        facts.hostname.is_some().then_some(facts)
    }

    pub(super) fn record_net(&self, name: &str, facts: &NetFacts) -> Result<bool, CliError> {
        self.update_entry(name, |entry| apply_net(entry, facts))
    }

    pub(super) fn refresh_net(&mut self, remote: &mut Remote, vm: &Resolved) -> Option<String> {
        if vm.entry.hypervisor != DEVICE {
            return None;
        }
        if let Some(facts) = Self::net_facts(remote, vm)
            && let Err(error) = self.record_net(&vm.name, &facts)
        {
            self.warnings.push(format!(
                "could not record {}'s address facts: {error}",
                vm.name
            ));
        }
        let entry = self.resolve_vm(Some(&vm.name)).ok()?.entry;
        Some(wake_line(&entry))
    }

    pub(super) fn vm_network_wake(
        &mut self,
        name: Option<&str>,
        profile: &str,
        enabled: Option<bool>,
    ) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(name)?;
        let status = self.connect(&vm)?.call(tilo_device_protocol::NetworkWake {
            profile: profile.into(),
            enabled,
        })?;
        let text = format!(
            "{}: magic-packet wake saved={}, driver applied={:?}, supported={:?}. Physical wake remains unverified.{}",
            vm.name,
            status.configured,
            status.applied,
            status.supported,
            status
                .problem
                .as_ref()
                .map_or(String::new(), |problem| format!(" {problem}")),
        );
        let mut outcome = Outcome::new(text, json!({ "vm": vm.name, "network_wake": status }));
        outcome.failed = status.problem.is_some();
        Ok(outcome)
    }

    pub(super) fn vm_awake(
        &mut self,
        name: Option<&str>,
        hold: Option<u64>,
        off: bool,
    ) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(name)?;
        let mut remote = self.connect(&vm)?;
        let lease = remote.call(Awake { seconds: hold, off })?;
        let mut text = if off && lease.until.is_none() {
            format!(
                "{} may suspend again once idle: its keep-awake lease is released. The next tilo-vm command takes it again for {} minutes",
                vm.name,
                AWAKE_IDLE_SECONDS / 60
            )
        } else {
            format!("{}: {}", vm.name, describe_lease(&lease))
        };
        if hold.is_none() && !off && lease.until.is_some() {
            let _ = write!(
                text,
                "\nlonger without requests: tilo-vm vm awake {} --for 2h",
                vm.name
            );
        }
        let mut outcome = Outcome::new(text, json!({ "vm": vm.name, "awake": lease }));
        outcome.failed = !off && !lease.held;
        Ok(outcome.next(&[&format!("tilo-vm vm status {}", vm.name)]))
    }

    pub(super) fn vm_wake(&mut self, name: Option<&str>) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(name)?;
        let adapter = Self::adapter(&vm)?;
        let started = Instant::now();
        if adapter.name() != DEVICE {
            let Some(commands) = adapter.wake(&vm.vm()) else {
                return Err(CliError::Unsupported {
                    what: format!("waking a VM with the {} adapter", adapter.name()),
                    alternative: format!(
                        "a VM that is off: tilo-vm vm start {}; a suspended one: resume it in the hypervisor's own UI",
                        vm.name
                    ),
                });
            };
            match adapter.suspended(&vm.vm(), &mut self.runner) {
                Some(true) => run_all(&mut self.runner, &commands)?,
                _ if adapter.running(&vm.vm(), &mut self.runner) == Some(false) => {
                    return Err(CliError::Unsupported {
                        what: format!("waking VM {}: it is off, not asleep", vm.name),
                        alternative: format!("tilo-vm vm start {}", vm.name),
                    });
                }
                _ => {}
            }
            return self.await_daemon(&vm, started, "woke it through the hypervisor");
        }
        if self.connect(&vm).is_ok() {
            return self.await_daemon(&vm, started, "it was awake already");
        }
        let mac_text = vm.entry.mac.clone().ok_or_else(|| CliError::Unsupported {
            what: format!("waking {}: no MAC is recorded for it", vm.name),
            alternative: format!(
                "wake it at its power button or keyboard this once, then tilo-vm vm status {}: that records its MAC, hostname and wake setting",
                vm.name
            ),
        })?;
        let mac = reach::parse_mac(&mac_text).ok_or_else(|| CliError::Config {
            path: self.registry_path.display().to_string(),
            detail: format!(
                "device '{}' has mac = \"{mac_text}\", not aa:bb:cc:dd:ee:ff",
                vm.name
            ),
        })?;
        let targets = reach::wake_targets(
            vm.entry
                .wake
                .as_ref()
                .and_then(|wake| wake.broadcast.as_deref()),
        );
        if !vm.entry.wake.as_ref().is_some_and(WakeInfo::reliable) {
            let note = format!(
                "{} cannot be counted on to wake from here ({}): if it does not, someone has to wake it at the machine. To keep it from sleeping while you work: tilo-vm vm awake {} --for 2h",
                vm.name,
                vm.entry.wake.as_ref().map_or_else(
                    || "its wake setting is not known".into(),
                    WakeInfo::describe
                ),
                vm.name
            );
            if !self.json {
                eprintln!("tilo-vm: {note}");
            }
            self.warnings.push(note);
        }
        let sent = reach::send_magic_packet(mac, &targets)
            .map_err(CliError::io("send the magic packet"))?;
        let sent = format!("for {mac_text} to {}", sent.join(" and "));
        if !self.json {
            eprintln!(
                "tilo-vm: sent {}'s magic packet {sent}; waiting up to {}s for it",
                vm.name,
                WAKE_WAIT.as_secs()
            );
        }
        let mut last_sent = Instant::now();
        loop {
            match self.connect(&vm) {
                Ok(_) => break,
                Err(CliError::DeviceAsleep { .. }) if started.elapsed() < WAKE_WAIT => {
                    if last_sent.elapsed() >= RESEND_EVERY {
                        let _ = reach::send_magic_packet(mac, &targets);
                        last_sent = Instant::now();
                    }
                }
                Err(CliError::DeviceAsleep { .. }) => {
                    return Err(CliError::WakeTimeout {
                        vm: vm.name.clone(),
                        sent,
                        seconds: WAKE_WAIT.as_secs(),
                        wake: vm.entry.wake.as_ref().map(WakeInfo::describe),
                    });
                }
                Err(error) => return Err(error),
            }
        }
        self.await_daemon(&vm, started, &format!("sent its magic packet {sent}"))
    }

    fn await_daemon(
        &mut self,
        vm: &Resolved,
        started: Instant,
        how: &str,
    ) -> Result<Outcome, CliError> {
        let remote = loop {
            match self.connect(vm) {
                Ok(remote) => break remote,
                Err(CliError::Unreachable { .. } | CliError::DeviceAsleep { .. })
                    if started.elapsed() < WAKE_WAIT =>
                {
                    std::thread::sleep(Duration::from_secs(2));
                }
                Err(
                    CliError::Unreachable { detail, .. } | CliError::DeviceAsleep { detail, .. },
                ) => {
                    return Err(CliError::Unreachable {
                        vm: vm.name.clone(),
                        detail: format!(
                            "{how}, but its daemon did not answer within {}s: {detail}",
                            WAKE_WAIT.as_secs()
                        ),
                        debug: Vec::new(),
                    });
                }
                Err(error) => return Err(error),
            }
        };
        let seconds = started.elapsed().as_secs_f64();
        let text = format!(
            "{} is awake ({how}); its daemon {} answered after {seconds:.0}s",
            vm.name, remote.status.daemon_version
        );
        Ok(Outcome::new(
            text,
            json!({ "vm": vm.name, "seconds": seconds, "how": how }),
        )
        .next(&["tilo-vm status"]))
    }
}

/// Run `script` with `sh` in the device's own account and return what it printed, when it exits 0.
pub(super) fn device_sh(remote: &mut Remote, script: &str) -> Result<String, CliError> {
    let ran = remote.call(Exec {
        account: DEVICE_ACCOUNT.into(),
        command: vec!["sh".into(), "-c".into(), script.into()],
        timeout: tilo_device_protocol::EXEC_TIMEOUT_SECONDS,
        max_bytes: tilo_device_protocol::EXEC_MAX_BYTES,
    })?;
    let stdout = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &ran.stdout)
        .map_err(|error| CliError::CommandFailed {
            command: "exec".into(),
            status: "unreadable output".into(),
            stderr: error.to_string(),
        })?;
    let stdout = String::from_utf8_lossy(&stdout).into_owned();
    if ran.code == 0 {
        Ok(stdout)
    } else {
        Err(CliError::CommandFailed {
            command: "sh -c".into(),
            status: format!("exit status: {}", ran.code),
            stderr: stdout,
        })
    }
}

pub fn apply_net(entry: &mut VmEntry, facts: &NetFacts) {
    if let Some(hostname) = &facts.hostname {
        entry.hostname = Some(hostname.clone());
    }
    if facts.mac.is_some() {
        entry.mac.clone_from(&facts.mac);
        entry.wake.clone_from(&facts.wake);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn reach_describes_the_lease_and_a_devices_net_facts;

        phase the_lease_says_until_when_and_why;
        {
            let now = unix_now();
            let idle = describe_lease(&AwakeLease {
                until: Some(now + 1800),
                hold_until: None,
                held: true,
                problem: None,
            });
            assert!(idle.contains("(in 30 min)"), "{idle}");
            assert!(
                idle.contains("30 minutes after the last tilo-vm request"),
                "{idle}"
            );
            let held = describe_lease(&AwakeLease {
                until: Some(now + 7200),
                hold_until: Some(now + 7200),
                held: true,
                problem: None,
            });
            assert!(
                held.contains("(in 2h00m)") && held.contains("--for"),
                "{held}"
            );
            let broken = describe_lease(&AwakeLease {
                until: Some(now + 1800),
                hold_until: None,
                held: false,
                problem: Some("tilo-vm-awake.service is not running".into()),
            });
            assert!(
                broken.starts_with("should be held awake, but is not"),
                "{broken}"
            );
            assert!(describe_lease(&AwakeLease::default()).starts_with("not held awake"));
            assert_eq!(local_name("yoga.lan"), "yoga.local");
            assert_eq!(local_name("opi"), "opi.local");
        }

        phase a_devices_net_facts_are_recorded_and_described;
        {
            let mut entry = VmEntry::default();
            assert!(wake_line(&entry).starts_with("no MAC recorded"));
            let facts = reach::parse_net_probe(
                "hostname=yoga\ninterface=wlan0\nmac=02:11:22:33:44:55\nbroadcast=10.0.0.255\nkind=wifi\nphy=phy0\n@@wowlan\nWoWLAN is disabled.\n",
            );
            apply_net(&mut entry, &facts);
            assert_eq!(entry.hostname.as_deref(), Some("yoga"));
            assert_eq!(entry.mac.as_deref(), Some("02:11:22:33:44:55"));
            let line = wake_line(&entry);
            assert!(
                line.contains("not known to wake") && line.contains("yoga.local"),
                "{line}"
            );
            apply_net(
                &mut entry,
                &NetFacts {
                    hostname: Some("yoga".into()),
                    ..NetFacts::default()
                },
            );
            assert_eq!(entry.mac.as_deref(), Some("02:11:22:33:44:55"));
        }
    }
}
