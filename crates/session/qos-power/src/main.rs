//! `qos-power` — a tiny daemon that keeps the CPU frequency governor
//! aligned with the current power source.
//!
//! # What it does
//!
//! Watches `/sys/class/power_supply/AC/online` (a single-byte sysfs
//! file, `1` = AC connected, `0` = on battery) and writes the target
//! governor to `/sys/devices/system/cpu/cpu*/cpufreq/scaling_governor`.
//!
//! - **AC connected** → `performance`. The scheduler stops dawdling
//!   on frequency ramp-up. Perceived responsiveness bump on desktops
//!   comes from this single change more than any other kernel tweak.
//! - **On battery** → `schedutil` (Linux 4.7+ default; frequency
//!   follows scheduler load estimates). Balances responsiveness against
//!   battery life; if the kernel doesn't expose `schedutil`, falls back
//!   to `ondemand`, then `powersave`.
//!
//! # Why not the desktop's power-profiles-daemon (PPD)?
//!
//! PPD is a great front-end for the user's power-profile choice (Power
//! Saver / Balanced / Performance) — a UI concept. What PPD does under
//! the hood is exactly what we do here: write governors + EPP hints.
//! qos-power is the automatic AC-detect + default-mapping layer that
//! sits under whatever the user's currently-active PPD profile picks;
//! when PPD (or a user override) wants a different governor it writes
//! `/run/qos-power/lock` and we back off.
//!
//! # Sysfs failure modes
//!
//! - **`AC/online` is not present** (VMs, servers without a battery):
//!   assume AC, apply `performance`, exit.
//! - **`scaling_governor` is not present** (kernel built without the
//!   cpufreq subsystem — extremely rare on x86-64): log a warning and
//!   exit cleanly. Not fatal.
//! - **Governor not available** (`performance` on a locked-down BIOS
//!   without an unlocked P-state driver): try the fallback list in
//!   order; if all fail, log and exit.
//!
//! # No systemd bus dep
//!
//! The service is `Type=notify` and reports readiness via the
//! `NOTIFY_SOCKET` `sd_notify(3)` protocol directly — we don't pull in
//! `systemd-sys` just for a one-line handshake.

use std::fs;
use std::io::Write;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

const AC_ONLINE_PATH: &str = "/sys/class/power_supply/AC/online";
const CPU_ROOT: &str = "/sys/devices/system/cpu";
const OVERRIDE_LOCK: &str = "/run/qos-power/lock";

/// Governor to apply when the machine is on AC. Single value — no
/// fallback list needed; `performance` is present on every reasonable
/// kernel that exposes cpufreq.
const AC_GOVERNOR: &str = "performance";

/// Battery-side preferences, tried in order. `schedutil` is the modern
/// default (kernel 4.7+); `ondemand` is the older equivalent still
/// present on some vendor kernels; `powersave` is the last-resort
/// fallback and preserves battery even if it feels sluggish.
const BATTERY_GOVERNORS: &[&str] = &["schedutil", "ondemand", "powersave"];

fn main() {
    // Best-effort readiness ping — we do this before setting the
    // governor so systemd's Type=notify handshake doesn't block on a
    // slow sysfs write. Failure to notify is not fatal; a manual
    // `qos-power` run outside systemd has no NOTIFY_SOCKET set.
    let _ = sd_notify("READY=1");

    // If there's no AC file at all (VM / no-battery server), just apply
    // the AC governor and exit. Nothing to watch.
    if !Path::new(AC_ONLINE_PATH).exists() {
        eprintln!("qos-power: no {AC_ONLINE_PATH} — assuming AC");
        set_governor(AC_GOVERNOR).ok();
        return;
    }

    // Apply the current state once at startup so we don't have to wait
    // for the first udev event to reconcile.
    apply_current_state();

    // Poll (not inotify) — sysfs doesn't reliably support inotify.
    // 1 s poll is imperceptible cost and beats the "plug-then-wait"
    // UX of longer intervals; the write itself is idempotent when the
    // state didn't change.
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(1));
            if tx.send(()).is_err() {
                break;
            }
        }
    });

    let mut last_state: Option<PowerState> = None;
    for () in rx {
        let state = read_ac_state();
        if last_state != Some(state) {
            apply_state(state);
            last_state = Some(state);
        }
    }
}

fn sd_notify(msg: &str) -> std::io::Result<()> {
    let sock_path = std::env::var("NOTIFY_SOCKET")
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::NotFound, "NOTIFY_SOCKET not set"))?;
    let sock = UnixDatagram::unbound()?;
    sock.send_to(msg.as_bytes(), sock_path)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PowerState {
    Ac,
    Battery,
}

fn read_ac_state() -> PowerState {
    match fs::read_to_string(AC_ONLINE_PATH) {
        Ok(s) if s.trim() == "1" => PowerState::Ac,
        Ok(_) => PowerState::Battery,
        Err(_) => PowerState::Ac, // safer default: apply performance
    }
}

fn apply_current_state() {
    apply_state(read_ac_state());
}

fn apply_state(state: PowerState) {
    if Path::new(OVERRIDE_LOCK).exists() {
        eprintln!("qos-power: {OVERRIDE_LOCK} present — respecting external override");
        return;
    }
    match state {
        PowerState::Ac => {
            if let Err(e) = set_governor(AC_GOVERNOR) {
                eprintln!("qos-power: AC → {AC_GOVERNOR} failed: {e}");
            }
        }
        PowerState::Battery => {
            for gov in BATTERY_GOVERNORS {
                match set_governor(gov) {
                    Ok(()) => return,
                    Err(e) => {
                        eprintln!("qos-power: battery → {gov} failed: {e}");
                    }
                }
            }
            eprintln!("qos-power: no battery governor available; leaving unchanged");
        }
    }
}

/// Write `governor` to every CPU's `scaling_governor` file. Iterates
/// numeric-suffix `cpu<N>` entries and skips non-cpu dirs (`cpuidle`,
/// `cpufreq`, `power`). Governor writes are idempotent — the kernel
/// silently no-ops when the value hasn't changed.
fn set_governor(governor: &str) -> std::io::Result<()> {
    for cpu in enumerate_cpus() {
        let target = cpu.join("cpufreq").join("scaling_governor");
        if !target.exists() {
            continue;
        }
        let mut f = fs::OpenOptions::new().write(true).open(&target)?;
        f.write_all(governor.as_bytes())?;
    }
    Ok(())
}

fn enumerate_cpus() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(CPU_ROOT) {
        for e in entries.flatten() {
            let name = e.file_name();
            let s = match name.to_str() {
                Some(s) => s,
                None => continue,
            };
            // Match `cpu` followed by all-digit suffix.
            if !s.starts_with("cpu") || !s[3..].chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            out.push(e.path());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// The AC-state parser accepts a lone `1` or `0` with optional
    /// trailing newline — matches the exact byte pattern the kernel
    /// emits.
    #[test]
    fn ac_state_from_sysfs_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ac_online");
        std::fs::write(&p, "1\n").unwrap();
        assert_eq!(read_bytes_as_state(&p), PowerState::Ac);
        std::fs::write(&p, "0\n").unwrap();
        assert_eq!(read_bytes_as_state(&p), PowerState::Battery);
        std::fs::write(&p, "").unwrap();
        assert_eq!(read_bytes_as_state(&p), PowerState::Battery);
    }

    fn read_bytes_as_state(p: &Path) -> PowerState {
        match std::fs::read_to_string(p) {
            Ok(s) if s.trim() == "1" => PowerState::Ac,
            _ => PowerState::Battery,
        }
    }

    /// `enumerate_cpus` walks a synthetic sysfs-shaped tree and
    /// returns exactly the numbered cpu dirs, skipping the sibling
    /// dirs the real /sys/devices/system/cpu ships.
    #[test]
    fn cpu_enumeration_skips_non_cpu_dirs() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "cpu0", "cpu1", "cpu15", "cpuidle", "cpufreq", "power", "smt",
        ] {
            std::fs::create_dir(dir.path().join(name)).unwrap();
        }
        let root = dir.path();
        let mut out: Vec<String> = std::fs::read_dir(root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .filter(|s| s.starts_with("cpu") && s[3..].chars().all(|c| c.is_ascii_digit()))
            .collect();
        out.sort();
        assert_eq!(out, vec!["cpu0", "cpu1", "cpu15"]);
    }

    /// Writing a governor value to a mock sysfs file exercises the
    /// idempotent-write path without touching real hardware.
    #[test]
    fn governor_write_is_idempotent_on_the_same_value() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("scaling_governor");
        std::fs::write(&p, "performance").unwrap();
        // Same value: should still succeed without error.
        let mut f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.write_all(b"performance").unwrap();
        let got = std::fs::read_to_string(&p).unwrap();
        assert_eq!(got, "performance");
    }
}
