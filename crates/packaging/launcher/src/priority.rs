//! What a launched app gets, and where the OOM killer finds it.
//!
//! Every app the launcher starts is put in the foreground tier
//! ([`Tier::Foreground`]): a CPU and IO weight well above the default and a
//! memory-low floor, so apps outrank batch work and a starting app is not
//! paged out from under itself. Moving an app between the tiers is the same
//! write ([`crate::app_cgroup::set_tier`]): the session puts the apps the
//! person can see in the foreground tier and the ones they cannot in the
//! background tier, whose weight is below default and whose floor is none, so
//! the machine gives memory back from those first under pressure. An app in
//! either tier still uses every idle cycle; the image's slices
//! (`session.slice`, `app.slice`, `background.slice`) decide who yields.
//!
//! The values are systemd resource-control properties, written onto the app's
//! cgroup in the session's delegated app tree, where they are the cgroup files
//! systemd would write ([`crate::app_cgroup::property_writes`]), or onto the
//! app's own scope when a launcher had to ask the manager for one (see
//! [`crate::scope`]).
//!
//! Each tier also has an `oom_score_adj` ([`oom_score_adj`]): an app is a
//! candidate like any other, and a job it runs is a little likelier to be
//! chosen than the app that started it. The session (the compositor and the
//! services of the shell) is not: the display manager sets its score before
//! the login, where only a privileged process can
//! (`greetd.service.d/50-tilo-oom.conf`), and everything an app runs leaves
//! that protection first ([`leave_session_protection`]).
//!
//! Failure here is a log line: a missing cgroup file or a refusing manager
//! costs a boost, never the launch.

const GIB: u64 = 1 << 30;

/// CPU and IO weight of an app. The default is 100, so an app outweighs a
/// default-weighted sibling eight to one.
pub const FOREGROUND_WEIGHT: u64 = 800;
/// CPU and IO weight of an app that yields: below default.
pub const BACKGROUND_WEIGHT: u64 = 50;
/// `oom_score_adj` of an app: the kernel's default, the lowest an unprivileged
/// process may hand out once the session left its own.
pub const FOREGROUND_OOM_SCORE_ADJ: i32 = 0;
/// `oom_score_adj` of an app that yields: half the scale, so it is chosen
/// before an app that does not unless it holds far more.
pub const BACKGROUND_OOM_SCORE_ADJ: i32 = 500;
/// What an app's independent job adds to its app's `oom_score_adj`: a runaway
/// job goes before the app that started it.
pub const JOB_OOM_SCORE_EXTRA: i32 = 100;

/// A resource-control property: its name and its value, as `busctl` passes
/// them to `StartTransientUnit` (`t`, a u64).
pub type Property = (&'static str, u64);

/// Which tier an app's cgroup is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Foreground,
    Background,
}

#[must_use]
pub const fn oom_score_adj(tier: Tier, leaf: crate::app_cgroup::Leaf) -> i32 {
    let app = match tier {
        Tier::Foreground => FOREGROUND_OOM_SCORE_ADJ,
        Tier::Background => BACKGROUND_OOM_SCORE_ADJ,
    };
    match leaf {
        crate::app_cgroup::Leaf::Main => app,
        crate::app_cgroup::Leaf::Job => app.saturating_add(JOB_OOM_SCORE_EXTRA),
    }
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

/// Properties that put an app scope in `tier`. `total` is physical memory;
/// an app's floor is an eighth of it, at most 4 GiB, so the protected set
/// fits inside the ancestors' floors shipped with the image.
#[must_use]
pub fn tier_properties(tier: Tier, total: Option<u64>) -> Vec<Property> {
    match tier {
        Tier::Foreground => {
            let mut props = vec![
                ("CPUWeight", FOREGROUND_WEIGHT),
                ("IOWeight", FOREGROUND_WEIGHT),
            ];
            if let Some(total) = total {
                props.push(("MemoryLow", (total / 8).min(4 * GIB)));
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

/// The `name signature value` busctl arguments of one property.
#[must_use]
pub fn dbus_property_args(property: &Property) -> [String; 3] {
    [property.0.into(), "t".into(), property.1.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_oom_killer_prefers_an_apps_job_to_the_app() {
        use crate::app_cgroup::Leaf;
        let app = oom_score_adj(Tier::Foreground, Leaf::Main);
        assert_eq!(app, 0);
        assert_eq!(oom_score_adj(Tier::Foreground, Leaf::Job), 100);
        assert!(oom_score_adj(Tier::Foreground, Leaf::Job) > app);
        for tier in [Tier::Foreground, Tier::Background] {
            for leaf in [Leaf::Main, Leaf::Job] {
                let adj = oom_score_adj(tier, leaf);
                assert!(adj > -1000 && adj <= 1000, "{tier:?} {leaf:?}");
                // Unprivileged: never below what an app is born with.
                assert!(adj >= FOREGROUND_OOM_SCORE_ADJ);
            }
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
