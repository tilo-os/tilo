//! A branch's first boot, beside its running source: a workspace takes a new
//! device in only through its host, so `vm branch` boots the source and its
//! branches once, waits until every workspace the source hosts has taken each
//! branch in, and stops them again.

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use serde_json::Value;
use tilo_vm_protocol::{SlotKind, SlotRestart, Status};

use super::{App, Resolved, StopGraces, control_json, stop_escalating};
use crate::config::Registry;
use crate::error::CliError;
use crate::hypervisor::{Runner, SystemRunner, Vm, run_all};
use crate::remote;

/// What the host keeps for itself beside the VMs it boots, in MiB.
pub(super) const HOST_RESERVE_MB: u64 = 2048;

const POLL: Duration = Duration::from_secs(5);

/// How long a branch waits for its host, its sessions up and naming it, before tilo-vm asks
/// whether it reaches its source on the host network at all.
pub(super) const REACH_AFTER: Duration = Duration::from_secs(60);

/// What a branch says of the host network ([`crate::lan`]): its own address on it, and whether
/// it reached the address asked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Reach {
    pub address: Option<String>,
    pub reached: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Guest {
    pub key: Option<String>,
    pub recorded: bool,
    pub slots: BTreeMap<String, Option<Vec<Held>>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Held {
    pub room: String,
    pub name: String,
    pub host: String,
    pub hosted_here: bool,
    pub awaiting: bool,
}

pub(super) trait Guests {
    /// The host's memory free for VMs, in MiB; `None` where it cannot tell.
    fn available_memory_mb(&mut self) -> Option<u64>;
    fn read(
        &mut self,
        vm: &str,
        slots: Option<&BTreeSet<String>>,
        wake: bool,
    ) -> Result<Option<Guest>, CliError>;
    /// What `vm` says of the host network, asked to reach `to` on it; `None` when it does not
    /// answer.
    fn reach(&mut self, vm: &str, to: Ipv4Addr) -> Result<Option<Reach>, CliError>;
    fn pause(&mut self);
    fn elapsed(&self) -> Duration;
}

/// Whether all that keeps `guest` from being taken in is its host: it answers, re-keyed, and
/// each session holds what `expected` says, some of it not taken in yet.
fn waits_for_host(guest: Option<&Guest>, expected: &BTreeMap<String, Vec<Held>>) -> bool {
    let Some(guest) = guest.filter(|g| g.recorded) else {
        return false;
    };
    let mut waits = false;
    for (slot, rooms) in expected {
        let Some(Some(held)) = guest.slots.get(slot) else {
            return false;
        };
        for want in rooms {
            match held.iter().find(|h| h.room == want.room) {
                None => return false,
                Some(h) => waits |= h.awaiting,
            }
        }
    }
    waits
}

fn pending(guest: Option<&Guest>, expected: &BTreeMap<String, Vec<Held>>) -> Option<String> {
    let Some(guest) = guest else {
        return Some("it does not answer yet".into());
    };
    if !guest.recorded {
        return Some("it has not given itself a new device key yet".into());
    }
    for (slot, rooms) in expected {
        let Some(Some(held)) = guest.slots.get(slot) else {
            return Some(format!("its session in slot {slot} does not answer yet"));
        };
        for want in rooms {
            match held.iter().find(|h| h.room == want.room) {
                None => {
                    return Some(format!(
                        "its session in slot {slot} does not hold {} yet",
                        want.name
                    ));
                }
                Some(h) if h.awaiting => {
                    return Some(format!(
                        "{} (slot {slot}) has not taken it in yet",
                        want.name
                    ));
                }
                Some(_) => {}
            }
        }
    }
    None
}

pub(super) fn batches(
    source: (&str, u64),
    branches: &[(String, u64)],
    available: Option<u64>,
) -> Result<Vec<Vec<String>>, String> {
    let names = || {
        branches
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>()
    };
    let Some(available) = available else {
        return Ok(vec![names()]);
    };
    let budget = available
        .checked_sub(HOST_RESERVE_MB)
        .and_then(|left| left.checked_sub(source.1))
        .ok_or_else(|| {
            format!(
                "{} needs {} MiB to boot and the host has {available} MiB free (keeping {HOST_RESERVE_MB} MiB for itself)",
                source.0, source.1
            )
        })?;
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut used = 0_u64;
    for (name, memory) in branches {
        if *memory > budget {
            return Err(format!(
                "{name} needs {memory} MiB beside {}, and {budget} MiB are free beside it",
                source.0
            ));
        }
        match (out.last_mut(), used.checked_add(*memory)) {
            (Some(batch), Some(sum)) if sum <= budget => {
                batch.push(name.clone());
                used = sum;
            }
            _ => {
                out.push(vec![name.clone()]);
                used = *memory;
            }
        }
    }
    Ok(out)
}

impl App {
    pub(super) fn admission_fits(
        guests: &mut dyn Guests,
        registry: &Registry,
        src: &str,
        rerun: &str,
    ) -> Result<(), CliError> {
        let Some(source) = registry.vms.get(src) else {
            return Ok(());
        };
        let memory = |entry| u64::from(Vm { name: src, entry }.memory_mb());
        batches(
            (src, memory(source)),
            &[(src.to_owned(), memory(source))],
            guests.available_memory_mb(),
        )
        .map(|_| ())
        .map_err(|why| no_room(src, &why, rerun))
    }

    pub(super) fn admit(
        &self,
        runner: &mut dyn Runner,
        guests: &mut dyn Guests,
        (src, rerun): (&str, &str),
        names: &[String],
        timeout: Duration,
    ) -> Result<Vec<String>, CliError> {
        let source = self.resolve_vm(Some(src))?;
        let adapter = Self::adapter(&source)?;
        let was_running = adapter.stopped(&source.vm(), runner) == Some(false);
        self.share_a_network(runner, (src, rerun), names, was_running)?;
        let source = self.resolve_vm(Some(src))?;
        let registry = self.registry()?;
        let memory = |name: &str| {
            registry
                .vms
                .get(name)
                .map_or(0, |entry| u64::from(Vm { name, entry }.memory_mb()))
        };
        let sizes: Vec<(String, u64)> = names.iter().map(|n| (n.clone(), memory(n))).collect();
        let own = if was_running { 0 } else { memory(src) };
        let plan = batches((src, own), &sizes, guests.available_memory_mb())
            .map_err(|why| no_room(src, &why, rerun))?;
        let mut started: Vec<String> = Vec::new();
        if !was_running {
            run_all(runner, &adapter.start(&source.vm())).map_err(|error| {
                refuse_admit(
                    format!("{src} did not boot: {error}"),
                    format!(
                        "{}   |   or keep the branches unbooted: {rerun} --no-admit",
                        error.hint()
                    ),
                )
            })?;
        }
        let mut taken = Vec::new();
        let result = self.admit_batches(
            runner,
            guests,
            (src, rerun),
            &plan,
            timeout,
            (&mut started, &mut taken),
            !was_running,
        );
        for name in &started {
            self.power_down(runner, name);
        }
        if !was_running {
            self.power_down(runner, src);
        }
        let admitted: Vec<String> = names
            .iter()
            .filter(|n| taken.contains(n))
            .cloned()
            .collect();
        let mut registry = self.registry()?;
        for name in &admitted {
            if let Some(entry) = registry.vms.get_mut(name) {
                entry.awaits_host = None;
            }
        }
        registry.save(&self.registry_path)?;
        result.map(|()| admitted)
    }

    fn admit_batches(
        &self,
        runner: &mut dyn Runner,
        guests: &mut dyn Guests,
        (src, rerun): (&str, &str),
        plan: &[Vec<String>],
        timeout: Duration,
        (started, admitted): (&mut Vec<String>, &mut Vec<String>),
        wake_source: bool,
    ) -> Result<(), CliError> {
        let deadline = guests.elapsed().saturating_add(timeout);
        let source = loop {
            let guest = guests.read(src, None, wake_source)?;
            if let Some(guest) = guest.filter(|g| g.slots.values().all(Option::is_some)) {
                break guest;
            }
            if guests.elapsed() >= deadline {
                return Err(timed_out(
                    src,
                    &[(
                        src.to_owned(),
                        "it, or one of its sessions, does not answer".into(),
                    )],
                    rerun,
                ));
            }
            guests.pause();
        };
        let expected: BTreeMap<String, Vec<Held>> = source
            .slots
            .iter()
            .filter_map(|(slot, held)| {
                let hosted: Vec<Held> = held
                    .iter()
                    .flatten()
                    .filter(|h| h.hosted_here)
                    .cloned()
                    .collect();
                (!hosted.is_empty()).then(|| (slot.clone(), hosted))
            })
            .collect();
        let slots: BTreeSet<String> = expected.keys().cloned().collect();
        for batch in plan {
            for name in batch {
                let vm = self.resolve_vm(Some(name))?;
                run_all(runner, &Self::adapter(&vm)?.start(&vm.vm())).map_err(|error| {
                    refuse_admit(
                        format!("{name} did not boot: {error}"),
                        format!("{}; then rerun: {rerun}", error.hint()),
                    )
                })?;
                started.push(name.clone());
            }
            let deadline = guests.elapsed().saturating_add(timeout);
            let mut waiting: Vec<String> = batch.clone();
            let mut waits_since: BTreeMap<String, Duration> = BTreeMap::new();
            let mut probed: BTreeSet<String> = BTreeSet::new();
            loop {
                let mut still = Vec::new();
                for name in &waiting {
                    let guest = guests.read(name, Some(&slots), true)?;
                    if waits_for_host(guest.as_ref(), &expected) {
                        let since = *waits_since
                            .entry(name.clone())
                            .or_insert_with(|| guests.elapsed());
                        if guests.elapsed().saturating_sub(since) >= REACH_AFTER
                            && probed.insert(name.clone())
                        {
                            self.reaches_source(guests, (src, name), rerun)?;
                        }
                    }
                    if let (Some(guest), Some(theirs)) = (&guest, &source.key)
                        && guest.recorded
                        && guest.key.as_ref() == Some(theirs)
                    {
                        return Err(refuse_admit(
                            format!("{name} kept {src}'s device key"),
                            format!(
                                "{src} never recorded its machine, so its branches cannot tell they were branched: boot {src} once on a current image (tilo-vm vm upgrade {src}), then branch it again (tilo-vm guide concepts)"
                            ),
                        ));
                    }
                    if let Some(why) = pending(guest.as_ref(), &expected) {
                        still.push((name.clone(), why));
                    } else {
                        admitted.push(name.clone());
                    }
                }
                if still.is_empty() {
                    break;
                }
                if guests.elapsed() >= deadline {
                    return Err(timed_out(src, &still, rerun));
                }
                waiting = still.into_iter().map(|(name, _)| name).collect();
                guests.pause();
            }
            for name in batch {
                self.power_down(runner, name);
                started.retain(|n| n != name);
            }
        }
        Ok(())
    }

    /// Put the source and the branches on the network this host's VMs share before any boots,
    /// and refuse at once when one is not on it: the source's workspaces could take a branch in
    /// only over an internet relay, which admission must not need.
    fn share_a_network(
        &self,
        runner: &mut dyn Runner,
        (src, rerun): (&str, &str),
        names: &[String],
        source_running: bool,
    ) -> Result<(), CliError> {
        for name in std::iter::once(src).chain(names.iter().map(String::as_str)) {
            let vm = self.resolve_vm(Some(name))?;
            let adapter = Self::adapter(&vm)?;
            let before = vm.entry.lan.clone();
            let (vm, _) = self.join_lan(runner, adapter.as_ref(), vm)?;
            if let Err(why) = adapter.host_network(&vm.entry) {
                return Err(refuse_admit(
                    format!("{name} is on no network with {src} on this host: {why}"),
                    format!(
                        "branch it without booting the branches: {rerun} --no-admit (each is taken in at its first boot while {src} runs where it can reach {src})"
                    ),
                ));
            }
            if name == src && source_running && before != vm.entry.lan {
                return Err(refuse_admit(
                    format!(
                        "{src} runs without its NIC on {}: it was made before that network",
                        crate::lan::describe()
                    ),
                    format!(
                        "restart it, which adds the NIC: tilo-vm vm restart {src}, then rerun: {rerun}"
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Ask branch `name`, whose sessions wait for their host, whether it reaches `src` on the
    /// host network at all: one that does not is named now, with the network, not at the end of
    /// the timeout.
    fn reaches_source(
        &self,
        guests: &mut dyn Guests,
        (src, name): (&str, &str),
        rerun: &str,
    ) -> Result<(), CliError> {
        let lan_of = |vm: &str| {
            self.resolve_vm(Some(vm))
                .ok()
                .and_then(|vm| crate::lan::mac(&vm.entry).and_then(crate::lan::address))
        };
        let (Some(theirs), Some(own)) = (lan_of(src), lan_of(name)) else {
            return Ok(());
        };
        let Some(reach) = guests.reach(name, theirs)? else {
            return Ok(());
        };
        if reach.reached {
            return Ok(());
        }
        let said = reach.address.map_or_else(
            || format!("{name} has no address on it (it should be {own})"),
            |address| format!("{name} is {address} on it"),
        );
        Err(refuse_admit(
            format!(
                "{name} waits for {src} to take it in, and does not reach {src} ({theirs}) on {}: {said}",
                crate::lan::describe()
            ),
            format!(
                "an image older than that network gives its NIC no address: boot {src} on a current image (tilo-vm vm upgrade {src}) and branch it again   |   or check the NIC in both: tilo-vm vm ssh {name} -- ip -4 addr show dev tilolan0   |   or branch without booting: {rerun} --no-admit"
            ),
        ))
    }

    fn power_down(&self, runner: &mut dyn Runner, name: &str) {
        let Ok(vm) = self.resolve_vm(Some(name)) else {
            return;
        };
        let Ok(adapter) = Self::adapter(&vm) else {
            return;
        };
        let poweroff = adapter.ssh_endpoint(&vm.vm(), runner).ok().map(|endpoint| {
            remote::ssh_command(
                &endpoint,
                vm.entry.key.as_deref(),
                &["systemctl".into(), "poweroff".into()],
                false,
            )
        });
        let down = stop_escalating(
            runner,
            adapter.as_ref(),
            &vm.vm(),
            poweroff.as_ref(),
            None,
            StopGraces::default(),
        );
        if !matches!(down, Ok((Some(true) | None, _))) && !self.json {
            eprintln!(
                "tilo-vm: {name} did not shut down; stop it with tilo-vm vm stop {name} --force"
            );
        }
    }
}

fn refuse_admit(what: String, fix: String) -> CliError {
    CliError::Branch { what, fix }
}

fn no_room(src: &str, why: &str, rerun: &str) -> CliError {
    refuse_admit(
        format!(
            "{src}: its branches are booted beside it once, to be taken into its workspaces, but {why}"
        ),
        format!(
            "free memory on this host (stop other VMs) and rerun: {rerun}   |   or make the branches without booting them: {rerun} --no-admit (each is then taken in at its first boot while {src} runs)"
        ),
    )
}

fn timed_out(src: &str, still: &[(String, String)], rerun: &str) -> CliError {
    let what = still
        .iter()
        .map(|(name, why)| format!("{name}: {why}"))
        .collect::<Vec<_>>()
        .join("; ");
    refuse_admit(
        format!("{src}'s branches were not taken in in time: {what}"),
        format!(
            "the branches are made and stopped, {src} is as it was; see what the branch's session says of its workspaces (tilo-vm vm start <branch>, then tilo-vm control --slot <slot> raw '{{\"op\":\"workspace_hosts\"}}'), and rerun to try again: {rerun}   (--admit-timeout gives it longer)"
        ),
    )
}

/// What a branch is asked of the host network, as root: its own address on `tilolan0`, and
/// whether `TARGET` answers it.
pub(super) const REACH: &str = r#"a=$(ip -4 -o addr show dev tilolan0 2>/dev/null | awk '{print $4}' | head -n 1)
[ -n "$a" ] && echo "SELF=$a"
if ping -c 3 -W 2 TARGET >/dev/null 2>&1; then echo REACHED=yes; fi
exit 0"#;

/// [`REACH`] for `to`, as the one shell line SSH hands the guest's login shell.
pub(super) fn reach_line(to: Ipv4Addr) -> Vec<String> {
    vec![format!(
        "sh -c {}",
        crate::vmbuild::sh_quote(&REACH.replace("TARGET", &to.to_string()))
    )]
}

pub(super) fn read_reach(said: &str) -> Reach {
    let mut reach = Reach::default();
    for line in said.lines().map(str::trim) {
        if let Some(address) = line.strip_prefix("SELF=") {
            reach.address = Some(address.to_owned());
        }
        reach.reached |= line == "REACHED=yes";
    }
    reach
}

pub(super) struct LiveGuests {
    pub app: App,
    pub started: Instant,
    pub woken: BTreeSet<String>,
}

pub(super) const MACHINE: &str = r#"cd /var/lib/tilo/device-private 2>/dev/null || exit 0
[ -f keypair.bin ] && echo "KEY=$(sha256sum < keypair.bin | cut -c1-16)"
u=$(tr -d '-' < /sys/class/dmi/id/product_uuid | tr 'A-F' 'a-f')
r=$([ -f machine.bin ] && od -An -tx1 -v machine.bin | tr -d ' \n')
if [ ! -f branch.bin ] && [ -n "$r" ] && [ "$u" = "$r" ]; then echo RECORDED=yes; fi"#;

impl Guests for LiveGuests {
    fn available_memory_mb(&mut self) -> Option<u64> {
        available_memory_mb()
    }

    fn read(
        &mut self,
        vm: &str,
        slots: Option<&BTreeSet<String>>,
        wake: bool,
    ) -> Result<Option<Guest>, CliError> {
        let resolved = self.app.resolve_vm(Some(vm))?;
        let mut remote = match self.app.connect(&resolved) {
            Ok(remote) => remote,
            Err(CliError::Unreachable { .. } | CliError::DaemonMissing { .. }) => return Ok(None),
            Err(error) => return Err(error),
        };
        let Ok(status) = remote.call(Status { slot: None }) else {
            // A daemon that drops the call mid-boot is read again; one that never answers is
            // what the admission's timeout names.
            return Ok(None);
        };
        let mut guest = Guest::default();
        let facts = self.machine(&resolved)?;
        for line in facts.lines() {
            if let Some(key) = line.strip_prefix("KEY=") {
                guest.key = Some(key.trim().to_owned());
            }
            guest.recorded |= line.trim() == "RECORDED=yes";
        }
        let mut worst = Duration::ZERO;
        for slot in status.slots.iter().filter(|s| s.kind != SlotKind::Vm) {
            if slots.is_some_and(|only| !only.contains(&slot.name)) {
                continue;
            }
            let held = if slot.session == "active" {
                control_json(
                    &mut remote,
                    &slot.name,
                    &HOSTS.map(str::to_owned),
                    &mut worst,
                )
                .ok()
                .map(|answer| held_of(&answer))
            } else {
                if wake && self.woken.insert(format!("{vm}:{}", slot.name)) {
                    remote.call(SlotRestart {
                        name: slot.name.clone(),
                    })?;
                }
                if !wake && slots.is_none() {
                    continue;
                }
                None
            };
            guest.slots.insert(slot.name.clone(), held);
        }
        Ok(Some(guest))
    }

    fn reach(&mut self, vm: &str, to: Ipv4Addr) -> Result<Option<Reach>, CliError> {
        let resolved = self.app.resolve_vm(Some(vm))?;
        let Ok(endpoint) = self.app.endpoint(&resolved) else {
            return Ok(None);
        };
        let command = remote::ssh_command(
            &endpoint,
            resolved.entry.key.as_deref(),
            &reach_line(to),
            false,
        );
        let mut runner = SystemRunner { cwd: None };
        let output = runner.output(&command)?;
        Ok(output.success.then(|| read_reach(&output.stdout)))
    }

    fn pause(&mut self) {
        std::thread::sleep(POLL);
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

impl LiveGuests {
    fn machine(&mut self, vm: &Resolved) -> Result<String, CliError> {
        let endpoint = self.app.endpoint(vm)?;
        let command =
            remote::ssh_command(&endpoint, vm.entry.key.as_deref(), &machine_line(), false);
        let mut runner = SystemRunner { cwd: None };
        Ok(runner.output(&command)?.stdout)
    }
}

/// [`MACHINE`] as the one shell line SSH hands the guest's login shell: ssh joins the remote
/// words with spaces, so `sh`, `-c` and the script as three words ran `sh -c cd` and the rest of
/// the script in the login shell's own directory, where it found no record and said nothing.
pub(super) fn machine_line() -> Vec<String> {
    vec![format!("sh -c {}", crate::vmbuild::sh_quote(MACHINE))]
}

const HOSTS: [&str; 2] = ["raw", r#"{"op":"workspace_hosts"}"#];

pub(super) fn held_of(answer: &Value) -> Vec<Held> {
    answer
        .pointer("/answer/workspaces")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let room = row.get("room")?.as_str()?.to_owned();
            let text = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or_default();
            let flag = |key: &str| row.get(key).and_then(Value::as_bool).unwrap_or(false);
            Some(Held {
                name: Some(text("name"))
                    .filter(|name| !name.is_empty())
                    .map_or_else(|| room.clone(), str::to_owned),
                host: text("host").to_owned(),
                hosted_here: flag("hosted_here"),
                awaiting: flag("awaiting_host"),
                room,
            })
        })
        .collect()
}

fn available_memory_mb() -> Option<u64> {
    if cfg!(target_os = "linux") {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kib: u64 = text
            .lines()
            .find_map(|l| l.strip_prefix("MemAvailable:"))?
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse()
            .ok()?;
        return Some(kib >> 10);
    }
    if cfg!(target_os = "macos") {
        let output = std::process::Command::new("vm_stat").output().ok()?;
        return vm_stat_free_mb(&String::from_utf8_lossy(&output.stdout));
    }
    None
}

pub(super) fn vm_stat_free_mb(text: &str) -> Option<u64> {
    let page: u64 = text
        .lines()
        .next()?
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let pages = |name: &str| -> u64 {
        text.lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|rest| rest.trim().trim_end_matches('.').parse().ok())
            .unwrap_or(0)
    };
    let total = pages("Pages free:")
        .saturating_add(pages("Pages inactive:"))
        .saturating_add(pages("Pages speculative:"));
    Some(total.saturating_mul(page) >> 20)
}
