//! What a launched app gets, and where the OOM killer finds it.
//!
//! Every app the launcher starts is put in the startup tier
//! ([`Tier::Startup`]) and, once it has opened, in the foreground tier
//! ([`Tier::Foreground`]): a CPU and IO weight well above the default and a
//! memory-low floor, so apps outrank batch work and a starting app is not
//! paged out from under itself, nor served after anything else on the
//! machine while it opens. Moving an app between the tiers is the same write
//! ([`crate::app_cgroup::set_tier`]): the session puts the apps the person
//! can see in the foreground tier and the ones they cannot in the background
//! tier, whose weight is below default and whose floor is none, so the
//! machine gives memory back from those first under pressure. An app in any
//! tier still uses every idle cycle; the image's slices (`session.slice`,
//! `app.slice`, `background.slice`) decide who yields.
//!
//! The session itself carries [`Tier::Session`] in the cgroup it runs in: the
//! desk placing a window is work a person waits on, so it outranks every app.
//! The image sets that weight on the compositor's logind scope; a session on a
//! host whose units do not carries it itself
//! ([`crate::app_cgroup::claim_session_weight`]).
//!
//! The values are systemd resource-control properties, written onto the app's
//! own scope, where they are the cgroup files systemd would write
//! ([`crate::app_cgroup::property_writes`], see [`crate::scope`]).
//!
//! The session (the compositor and the services of the shell) is not an
//! OOM-killer candidate like an app: the display manager sets its score before
//! the login, where only a privileged process can
//! (`greetd.service.d/50-tilo-oom.conf`), and everything an app runs leaves
//! that protection first ([`leave_session_protection`]).
//!
//! Failure here is a log line: a missing cgroup file or a refusing manager
//! costs a boost, never the launch.

use std::time::Duration;

const GIB: u64 = 1 << 30;

/// CPU and IO weight of an app. The default is 100, so an app outweighs a
/// default-weighted sibling eight to one.
pub const FOREGROUND_WEIGHT: u64 = 800;
/// CPU and IO weight of an app that yields: below default.
pub const BACKGROUND_WEIGHT: u64 = 50;
/// CPU and IO weight of an app while it opens: the most a cgroup may carry,
/// so the app a person just asked for is served before every other app, the
/// session's desk and any batch work. It is the same weight in the shares a
/// person notices; it lasts [`STARTUP_WINDOW`] and then the app takes the
/// tier of what the person can see.
pub const STARTUP_WEIGHT: u64 = 10_000;
/// How long a launch keeps the startup weight, counted from the process it
/// started: the app's own start, its first frame's work and the desk's
/// placement of its window under load. Long enough that opening an app is
/// never the moment something else is served first, short enough that the
/// machine goes back to sharing while the person uses the app.
pub const STARTUP_WINDOW: Duration = Duration::from_secs(2);
/// CPU and IO weight of the session itself: the compositor and the desk
/// placing, drawing and answering the windows of the person's work. Higher
/// than an app, lower than one that is opening.
pub const SESSION_WEIGHT: u64 = 1_000;
/// `oom_score_adj` of an app: the kernel's default, the lowest an unprivileged
/// process may hand out once the session left its own.
pub const FOREGROUND_OOM_SCORE_ADJ: i32 = 0;

/// A resource-control property: its name and its value, as `busctl` passes
/// them to `StartTransientUnit` (`t`, a u64).
pub type Property = (&'static str, u64);

/// Which tier a cgroup is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// An app that is opening: served before everything else on the machine.
    Startup,
    /// An app the person can see.
    Foreground,
    /// An app nobody is looking at: yields CPU, IO and memory.
    Background,
    /// The session's own cgroup: the compositor and the desk.
    Session,
}

/// Leave the session's protection from the OOM killer: an app, or the
/// launcher about to start one, must be a candidate like any app. Raising
/// one's own `oom_score_adj` needs no privilege; this only ever raises it, to
/// the foreground tier's (every launch starts there).
pub fn leave_session_protection() {
    let path = "/proc/self/oom_score_adj";
    let Ok(current) = std::fs::read_to_string(path) else {
        return;
    };
    if current
        .trim()
        .parse::<i32>()
        .is_ok_and(|current| current < FOREGROUND_OOM_SCORE_ADJ)
        && let Err(error) = std::fs::write(path, FOREGROUND_OOM_SCORE_ADJ.to_string())
    {
        eprintln!("tilo-launcher: keeping the session's OOM protection: {error}");
    }
}

/// Physical memory in bytes from `/proc/meminfo` text.
#[must_use]
pub fn parse_mem_total(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    kib.checked_mul(1024)
}

/// Physical memory of this machine.
#[must_use]
pub fn mem_total_bytes() -> Option<u64> {
    parse_mem_total(&std::fs::read_to_string("/proc/meminfo").ok()?)
}

/// Properties that put a cgroup in `tier`. `total` is physical memory; an
/// app's floor is an eighth of it, at most 4 GiB, so the protected set fits
/// inside the ancestors' floors shipped with the image. An app that is
/// opening keeps the floor it will need once it has opened. The session's
/// tier moves weights only: its floor is the image's to set.
#[must_use]
pub fn tier_properties(tier: Tier, total: Option<u64>) -> Vec<Property> {
    match tier {
        Tier::Startup => {
            let mut props = vec![("CPUWeight", STARTUP_WEIGHT), ("IOWeight", STARTUP_WEIGHT)];
            if let Some(total) = total {
                props.push(("MemoryLow", foreground_floor(total)));
            }
            props
        }
        Tier::Session => vec![("CPUWeight", SESSION_WEIGHT), ("IOWeight", SESSION_WEIGHT)],
        Tier::Foreground => {
            let mut props = vec![
                ("CPUWeight", FOREGROUND_WEIGHT),
                ("IOWeight", FOREGROUND_WEIGHT),
            ];
            if let Some(total) = total {
                props.push(("MemoryLow", foreground_floor(total)));
            }
            props
        }
        Tier::Background => vec![
            ("CPUWeight", BACKGROUND_WEIGHT),
            ("IOWeight", BACKGROUND_WEIGHT),
            ("MemoryLow", 0),
        ],
    }
}

/// The memory floor of an app in the foreground tier: an eighth of the
/// machine, at most 4 GiB.
fn foreground_floor(total: u64) -> u64 {
    (total / 8).min(4 * GIB)
}

/// The `name signature value` busctl arguments of one property.
#[must_use]
pub fn dbus_property_args(property: &Property) -> [String; 3] {
    [property.0.into(), "t".into(), property.1.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_opening_app_outranks_the_session_and_every_app() {
        let startup = tier_properties(Tier::Startup, Some(32 * GIB));
        assert_eq!(
            startup,
            [
                ("CPUWeight", 10_000),
                ("IOWeight", 10_000),
                ("MemoryLow", 4 * GIB),
            ],
            "a launching app takes the most a cgroup may carry, and its floor"
        );
        let session = tier_properties(Tier::Session, Some(32 * GIB));
        assert_eq!(
            session,
            [("CPUWeight", 1_000), ("IOWeight", 1_000)],
            "the desk outranks every app, and its floor is the image's"
        );
        let foreground = tier_properties(Tier::Foreground, Some(32 * GIB));
        // The order a person's work is served in: what they just asked for,
        // then the desk showing it, then what they can see, then the rest.
        let cpu_weight = |tier| {
            tier_properties(tier, Some(32 * GIB))
                .into_iter()
                .find_map(|(name, value)| (name == "CPUWeight").then_some(value))
        };
        let order = [
            Tier::Startup,
            Tier::Session,
            Tier::Foreground,
            Tier::Background,
        ];
        for pair in order.windows(2) {
            let (above, below) = (cpu_weight(pair[0]), cpu_weight(pair[1]));
            assert!(
                above > below,
                "{:?} does not outrank {:?}: {above:?} against {below:?}",
                pair[0],
                pair[1]
            );
        }
        assert!(session.len() < foreground.len());
        for props in [&startup, &session, &foreground] {
            assert!(!props.iter().any(|(name, _)| name.contains("Max")));
        }
        assert_eq!(STARTUP_WINDOW, Duration::from_secs(2));
    }

    #[test]
    fn the_foreground_tier_boosts_without_capping() {
        let fg = tier_properties(Tier::Foreground, Some(32 * GIB));
        assert_eq!(
            fg,
            [
                ("CPUWeight", 800),
                ("IOWeight", 800),
                ("MemoryLow", 4 * GIB),
            ]
        );
        // An eighth of a small machine, not the cap.
        assert!(tier_properties(Tier::Foreground, Some(4 * GIB)).contains(&("MemoryLow", GIB / 2)));
        let bg = tier_properties(Tier::Background, Some(32 * GIB));
        assert!(bg.contains(&("MemoryLow", 0)));
        // Never a hard limit, in either direction.
        for (name, _) in fg.iter().chain(&bg) {
            assert!(!name.contains("Max") && !name.contains("High") && *name != "CPUQuota");
        }
    }

    #[test]
    fn an_app_cgroups_tier_is_the_cgroup_files_systemd_would_write() {
        let dir = tempfile::tempdir().unwrap();
        for file in ["cpu.weight", "io.weight", "io.bfq.weight", "memory.low"] {
            std::fs::write(dir.path().join(file), "").unwrap();
        }
        crate::app_cgroup::apply(
            dir.path(),
            &tier_properties(Tier::Background, Some(32 * GIB)),
        )
        .unwrap();
        let read = |file: &str| std::fs::read_to_string(dir.path().join(file)).unwrap();
        assert_eq!(read("cpu.weight"), "50");
        assert_eq!(read("io.weight"), "default 50");
        assert_eq!(read("io.bfq.weight"), "50");
        assert_eq!(read("memory.low"), "0");
    }

    #[test]
    fn physical_memory_comes_from_proc_meminfo() {
        assert_eq!(
            parse_mem_total("MemTotal:       32213144 kB\n"),
            Some(32_213_144 * 1024)
        );
        assert_eq!(parse_mem_total("MemFree: 1 kB\n"), None);
    }
}
