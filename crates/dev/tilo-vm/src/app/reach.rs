//! Reaching a device where it is now, waking it, and holding it awake.
//!
//! A device's recorded address is only a hint ([`crate::reach`]): every
//! command takes it from the registry as it is now, tries it, and when it
//! stops answering looks the device up by name and keeps the new address,
//! retrying for a minute over a short loss of connectivity. A device that is
//! not found is asleep or gone, and `vm wake` sends its magic packet. While
//! someone drives a machine its tilo-vmd holds it awake (`vm awake`).

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use serde_json::json;
use tilo_vm_protocol::{AWAKE_IDLE_SECONDS, Awake, AwakeLease};

use super::{App, Outcome, Resolved};
use crate::config::VmEntry;
use crate::error::CliError;
use crate::hypervisor::{self, DEVICE, Runner as _, SshEndpoint, run_all};
use crate::reach::{self, NetFacts, Probe, WakeInfo};
use crate::remote;

/// How long a command keeps trying a device that stopped answering: long
/// enough for a Wi-Fi roam or a DHCP renewal, short enough that a device
/// that is asleep is reported as such.
pub const REACH_BUDGET: Duration = Duration::from_secs(60);
/// One authenticated attempt at the device's SSH port.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const RETRY_PAUSE: Duration = Duration::from_secs(3);
/// How long `vm wake` waits for the woken machine's tilo-vmd.
const WAKE_WAIT: Duration = Duration::from_secs(150);
/// `vm wake` sends the packet again this often while it waits: one can be lost.
const RESEND_EVERY: Duration = Duration::from_secs(15);

fn device_action_outcome(response: serde_json::Value) -> Outcome {
    let failed = response.get("err").is_some();
    let mut outcome = Outcome::new(response.to_string(), response);
    outcome.failed = failed;
    outcome
}

fn unix_now() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp()).unwrap_or(0)
}

/// mDNS announces a machine by its host name's first label.
pub fn local_name(hostname: &str) -> String {
    let label = hostname.split('.').next().unwrap_or(hostname);
    format!("{label}.local")
}

/// When the lease ends, in this machine's time, and how far off that is.
fn until(seconds: u64) -> String {
    let now = unix_now();
    let local = i64::try_from(seconds)
        .ok()
        .and_then(|s| chrono::DateTime::from_timestamp(s, 0))
        .map(|time| time.with_timezone(&chrono::Local));
    let left = seconds.saturating_sub(now);
    let at = local.map_or_else(
        || seconds.to_string(),
        |time| {
            if left >= 86_400 {
                time.format("%Y-%m-%d %H:%M").to_string()
            } else {
                time.format("%H:%M").to_string()
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

/// The lease in one line, as `vm status` and `vm awake` say it.
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

/// Whether the device can be reached beyond the LAN, in one line.
pub fn describe_peer(entry: &VmEntry, enrolled: bool) -> String {
    match &entry.peer {
        Some(route) => format!(
            "over the peer network as device {} ({}){}",
            route.device.get(..12).unwrap_or(&route.device),
            route.relay.as_deref().map_or_else(
                || "public relays".to_owned(),
                |relay| format!("relay {relay}")
            ),
            if enrolled {
                ""
            } else {
                "; enrol this machine for it once: tilo-vm vm enrol-admin"
            }
        ),
        None => {
            "not recorded: its image serves no device-admin route (a current dev image does)".into()
        }
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
    pub(super) fn vm_device_actions(
        &mut self,
        name: Option<&str>,
        command: crate::cli::DeviceActionCommand,
    ) -> Result<Outcome, CliError> {
        let payload = crate::device_actions::request(command, std::io::stdin().lock())?;
        let vm = self.resolve_vm(name)?;
        let mut remote = self.connect(&vm)?;
        Ok(device_action_outcome(remote.call(payload)?))
    }
    fn probe_device(&mut self, vm: &Resolved, endpoint: &SshEndpoint) -> Probe {
        reach::probe(
            &mut self.runner,
            endpoint,
            vm.entry.key.as_deref(),
            PROBE_TIMEOUT,
        )
    }

    /// Where to reach `vm` over SSH now. A device is taken at its address
    /// as the registry has it now (an earlier step may have moved it), and
    /// found again by name when that stops answering
    /// ([`Self::reach_device`]).
    pub(super) fn endpoint(&mut self, vm: &Resolved) -> Result<SshEndpoint, CliError> {
        if vm.entry.hypervisor != DEVICE {
            return Self::adapter(vm)?.ssh_endpoint(&vm.vm(), &mut self.runner);
        }
        self.reach_device(&vm.name, REACH_BUDGET, true)
    }

    /// A device's SSH endpoint once its SSH port answers. When the recorded
    /// address gives no route or no answer, the device is looked up as
    /// `<hostname>.local`, and a new address that answers is recorded; until
    /// `budget` runs out the address and the name are tried again, so a
    /// Wi-Fi roam does not fail the command. After that it is asleep or gone.
    ///
    /// # Errors
    /// [`CliError::DeviceAsleep`], or a registry that has no such device.
    pub(super) fn reach_device(
        &mut self,
        name: &str,
        budget: Duration,
        say: bool,
    ) -> Result<SshEndpoint, CliError> {
        let started = Instant::now();
        let mut said = !say || self.json;
        loop {
            let vm = self.resolve_vm(Some(name))?;
            let endpoint = hypervisor::adapter(DEVICE)?.ssh_endpoint(&vm.vm(), &mut self.runner)?;
            // A wrapper in TILO_VM_SSH decides how the device is reached.
            if std::env::var_os("TILO_VM_SSH").is_some() {
                return Ok(endpoint);
            }
            let detail = match self.probe_device(&vm, &endpoint) {
                Probe::There | Probe::Unknown => return Ok(endpoint),
                Probe::Lost(detail) => detail,
            };
            if let Some(moved) = self.find_by_name(&vm, &endpoint)? {
                return Ok(moved);
            }
            if started.elapsed().saturating_add(RETRY_PAUSE) >= budget {
                return Err(asleep(&vm, &endpoint, &detail));
            }
            if !said {
                eprintln!(
                    "tilo-vm: {name} does not answer at {} ({detail}); trying it and its name for up to {}s",
                    endpoint.host,
                    budget.as_secs()
                );
                said = true;
            }
            std::thread::sleep(RETRY_PAUSE);
        }
    }

    /// Look the device up as `<hostname>.local`; when it answers at another
    /// address, record that one and say so.
    fn find_by_name(
        &mut self,
        vm: &Resolved,
        endpoint: &SshEndpoint,
    ) -> Result<Option<SshEndpoint>, CliError> {
        let Some(hostname) = vm.entry.hostname.as_deref() else {
            return Ok(None);
        };
        let name = local_name(hostname);
        let Some(address) = reach::resolve(&mut self.runner, &name) else {
            return Ok(None);
        };
        let host = address.to_string();
        if host == endpoint.host {
            return Ok(None);
        }
        let moved = SshEndpoint {
            host,
            ..endpoint.clone()
        };
        if self.probe_device(vm, &moved) != Probe::There {
            return Ok(None);
        }
        self.update_entry(&vm.name, |entry| entry.ssh = Some(moved.ssh_spec()))?;
        let note = format!(
            "{} moved from {} to {}: found it as {name}, and recorded the new address in {}",
            vm.name,
            endpoint.host,
            moved.host,
            self.registry_path.display()
        );
        if !self.json {
            eprintln!("tilo-vm: {note}");
        }
        self.warnings.push(note);
        Ok(Some(moved))
    }

    /// Change one registry entry; `true` when that changed the file.
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

    /// Ask a device for its hostname, MAC and wake setting.
    pub(super) fn net_facts(
        &mut self,
        endpoint: &SshEndpoint,
        key: Option<&str>,
    ) -> Option<NetFacts> {
        let line = remote::ssh_command(endpoint, key, &[reach::net_probe(&endpoint.host)], false);
        let output = self.runner.output(&line).ok().filter(|o| o.success)?;
        let facts = reach::parse_net_probe(&output.stdout);
        facts.hostname.is_some().then_some(facts)
    }

    /// Record what [`Self::net_facts`] found; a MAC that was not found
    /// keeps the recorded one.
    pub(super) fn record_net(&self, name: &str, facts: &NetFacts) -> Result<bool, CliError> {
        self.update_entry(name, |entry| apply_net(entry, facts))
    }

    /// Refresh a device's net facts; its wake line for `vm status`.
    pub(super) fn refresh_net(&mut self, vm: &Resolved) -> Option<String> {
        if vm.entry.hypervisor != DEVICE {
            return None;
        }
        let endpoint = self.endpoint(vm).ok()?;
        if let Some(facts) = self.net_facts(&endpoint, vm.entry.key.as_deref())
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
        let status = self.connect(&vm)?.call(tilo_vm_protocol::NetworkWake {
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
        let recorded = hypervisor::adapter(DEVICE)?.ssh_endpoint(&vm.vm(), &mut self.runner)?;
        if std::env::var_os("TILO_VM_SSH").is_none()
            && self.probe_device(&vm, &recorded) == Probe::There
        {
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
            match self.reach_device(&vm.name, Duration::from_secs(10), false) {
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

    /// Wait for tilo-vmd after a wake; the hello takes the keep-awake lease.
    fn await_daemon(
        &mut self,
        vm: &Resolved,
        started: Instant,
        how: &str,
    ) -> Result<Outcome, CliError> {
        let remote = loop {
            match self.connect(vm) {
                Ok(remote) => break remote,
                Err(CliError::Unreachable { .. }) if started.elapsed() < WAKE_WAIT => {
                    std::thread::sleep(Duration::from_secs(2));
                }
                Err(CliError::Unreachable { detail, .. }) => {
                    return Err(CliError::Unreachable {
                        vm: vm.name.clone(),
                        detail: format!(
                            "{how}, but tilo-vmd did not answer within {}s: {detail}",
                            WAKE_WAIT.as_secs()
                        ),
                        debug: Vec::new(),
                    });
                }
                Err(error) => return Err(error),
            }
        };
        let seconds = started.elapsed().as_secs_f64();
        let mut text = format!(
            "{} is awake ({how}); tilo-vmd {} answered after {seconds:.0}s",
            vm.name, remote.hello.daemon_version
        );
        if let Some(lease) = &remote.hello.awake {
            let _ = write!(text, "\n  {}", describe_lease(lease));
        }
        Ok(Outcome::new(
            text,
            json!({
                "vm": vm.name,
                "seconds": seconds,
                "how": how,
                "awake": remote.hello.awake,
            }),
        )
        .next(&["tilo-vm status"]))
    }
}

/// Copy what a device reported into its entry.
pub fn apply_net(entry: &mut VmEntry, facts: &NetFacts) {
    if let Some(hostname) = &facts.hostname {
        entry.hostname = Some(hostname.clone());
    }
    if facts.mac.is_some() {
        entry.mac.clone_from(&facts.mac);
        entry.wake.clone_from(&facts.wake);
    }
    if facts.peer.is_some() {
        entry.peer.clone_from(&facts.peer);
    }
}

fn asleep(vm: &Resolved, endpoint: &SshEndpoint, detail: &str) -> CliError {
    let lookup = match &vm.entry.hostname {
        Some(hostname) => format!(", and {} does not find it elsewhere", local_name(hostname)),
        None => ", and no hostname is recorded to look it up by".into(),
    };
    CliError::DeviceAsleep {
        vm: vm.name.clone(),
        address: endpoint.host.clone(),
        detail: detail.to_owned(),
        lookup,
        wakeable: vm.entry.mac.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lease reads as when it ends and why, or why it is not held.
    #[test]
    fn the_lease_says_until_when_and_why() {
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

    /// A device's wake line names what wakes it, or why `vm wake` cannot.
    #[test]
    fn a_devices_net_facts_are_recorded_and_described() {
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
        // No MAC this time (it answered on loopback): the recorded one stays.
        apply_net(
            &mut entry,
            &NetFacts {
                hostname: Some("yoga".into()),
                ..NetFacts::default()
            },
        );
        assert_eq!(entry.mac.as_deref(), Some("02:11:22:33:44:55"));
        let asleep = asleep(
            &Resolved {
                name: "yoga".into(),
                entry,
                source: crate::config::Source::Argument,
            },
            &SshEndpoint::parse("me@10.0.0.223").unwrap(),
            "No route to host",
        );
        assert!(asleep.to_string().contains("asleep or gone"), "{asleep}");
        assert!(
            asleep.hint().contains("tilo-vm vm wake yoga"),
            "{}",
            asleep.hint()
        );
    }
}
