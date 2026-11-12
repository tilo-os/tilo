//! A branch's first boot, beside its running source (`vm branch` without `--no-admit`).
//!
//! A branch gives itself a new device key at its first boot, and a workspace takes a new device
//! in only through its host. The source hosts every desk workspace it made, so a branch first
//! booted while its source is off waits for it ("waiting for <source> to take this device in"),
//! and one whose source was removed waits for good. So `vm branch` boots the source and its new
//! branches once, as many at a time as the host's memory holds, waits until each branch has
//! re-keyed and every workspace the source hosts has taken it in, and stops them again: the
//! source as it was before, the branches off.
//!
//! What the guests say is read through [`Guests`]: in production over SSH and tilo-vmd from the
//! guest itself (its machine record, and each session's `workspace_hosts`, which says of every
//! workspace it holds who hosts it and whether the host took this device in yet); faked in the
//! tests.

use std::collections::{BTreeMap, BTreeSet};
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

/// How often the guests are read while they come up.
const POLL: Duration = Duration::from_secs(5);

/// What a booted VM says of itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Guest {
    /// A digest of its device key, to tell a branch that re-keyed from one that kept its source's.
    pub key: Option<String>,
    /// Its machine record names its own UUID and no re-key is in progress.
    pub recorded: bool,
    /// Its sessions, by slot: what each holds, or `None` while it does not answer yet.
    pub slots: BTreeMap<String, Option<Vec<Held>>>,
}

/// A workspace a session holds, as its `workspace_hosts` says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Held {
    pub room: String,
    pub name: String,
    /// The host's name as its person gave it.
    pub host: String,
    /// This device is the workspace's host.
    pub hosted_here: bool,
    /// This device holds the workspace but its host has not taken it in yet.
    pub awaiting: bool,
}

/// Where `vm branch` learns what the host and the guests say.
pub(super) trait Guests {
    /// The host's memory free for VMs, in MiB; `None` where it cannot tell.
    fn available_memory_mb(&mut self) -> Option<u64>;
    /// What `vm` says of itself now; `None` while it does not answer. Only the sessions of
    /// `slots` are read (all but the `vm` slot's when `None`); with `wake`, a session that is not
    /// running is started.
    ///
    /// # Errors
    /// The guest answered with something tilo-vm cannot use.
    fn read(
        &mut self,
        vm: &str,
        slots: Option<&BTreeSet<String>>,
        wake: bool,
    ) -> Result<Option<Guest>, CliError>;
    /// Let time pass before the next read.
    fn pause(&mut self);
    /// Time since the admission started.
    fn elapsed(&self) -> Duration;
}

/// What still keeps a branch from being taken in.
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

/// Split `branches` into batches that fit beside the source in `available` MiB, keeping
/// [`HOST_RESERVE_MB`] for the host. `Err` names what does not fit.
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
    /// Check, before anything is made, that `src` can boot beside at least one branch.
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

    /// Boot `src` (unless it runs) and `names`, wait until each re-keyed and every workspace `src`
    /// hosts took it in, and stop what was booted. A branch taken in no longer `awaits_host`.
    pub(super) fn admit(
        &self,
        runner: &mut dyn Runner,
        guests: &mut dyn Guests,
        (src, rerun): (&str, &str),
        names: &[String],
        timeout: Duration,
    ) -> Result<Vec<String>, CliError> {
        let mut registry = self.registry()?;
        let source = self.resolve_vm(Some(src))?;
        let adapter = Self::adapter(&source)?;
        let was_running = adapter.stopped(&source.vm(), runner) == Some(false);
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
        // Back to how the person had it: the branches off, the source as it was.
        for name in &started {
            self.power_down(runner, name);
        }
        if !was_running {
            self.power_down(runner, src);
        }
        // In the order they were named, whichever was taken in first; also those an earlier batch
        // had taken in when a later one failed.
        let admitted: Vec<String> = names
            .iter()
            .filter(|n| taken.contains(n))
            .cloned()
            .collect();
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
        // The source up, with every session it has: those are the hosts.
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
            loop {
                let mut still = Vec::new();
                for name in &waiting {
                    let guest = guests.read(name, Some(&slots), true)?;
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

    /// Shut `name` down the way `vm stop` does without its guest's lock: ACPI, then `systemctl
    /// poweroff` over SSH, then a hard power-off. A VM that does not go down is said in a warning.
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

/// The guests as they are: read over SSH and tilo-vmd.
pub(super) struct LiveGuests {
    pub app: App,
    pub started: Instant,
    /// Sessions already started once, as `vm:slot`.
    pub woken: BTreeSet<String>,
}

/// What a guest's machine record says, as root: a digest of its device key, and whether the
/// record names this machine's UUID with no re-key journal left.
const MACHINE: &str = r#"cd /var/lib/tilo/device-private 2>/dev/null || exit 0
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
            // Still booting: SSH or tilo-vmd not up yet.
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
                    // A session the person keeps stopped on a running source hosts nothing now.
                    continue;
                }
                None
            };
            guest.slots.insert(slot.name.clone(), held);
        }
        Ok(Some(guest))
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
        let command = remote::ssh_command(
            &endpoint,
            vm.entry.key.as_deref(),
            &["sh".into(), "-c".into(), MACHINE.into()],
            false,
        );
        let mut runner = SystemRunner { cwd: None };
        Ok(runner.output(&command)?.stdout)
    }
}

/// What a session is asked of the workspaces it holds: who hosts each, and whether its host took
/// this device in yet (the session's `workspace_hosts`).
const HOSTS: [&str; 2] = ["raw", r#"{"op":"workspace_hosts"}"#];

/// The workspaces a session's `workspace_hosts` answer lists.
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

/// The host's memory free for VMs, in MiB: Linux's `MemAvailable`, macOS's free, inactive and
/// speculative pages. `None` elsewhere.
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

/// Free memory in MiB from `vm_stat`'s output: free, inactive and speculative pages.
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
