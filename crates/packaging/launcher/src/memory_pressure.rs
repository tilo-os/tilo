//! Running out of memory, handled before the kernel has to.
//!
//! Running out of memory must never freeze or crash the session. Long before
//! the kernel's OOM killer acts, a machine with compressed swap thrashes: it
//! spends its time paging, input stalls, frames stop. So the session's app
//! spawner watches memory pressure (PSI: `/proc/pressure/memory`, the app
//! tree's and each app's `memory.pressure`) and acts in two steps:
//!
//! 1. **Rising pressure:** reclaim from apps in the background tier
//!    (`memory.reclaim`), a quarter of what each holds, at most every few
//!    seconds each. The focused app and launches still starting are never
//!    touched, nor is the session (it is not in the app tree).
//! 2. **Critical pressure that stays:** end the one app or job growing out of
//!    control: the one whose memory grew most over the last
//!    [`GROWTH_WINDOW_TICKS`] samples, not the largest one. A job ends alone
//!    (a language server, a build); an app ends whole, with its jobs. The
//!    person is told which and why. Then the policy waits
//!    [`COOLDOWN_TICKS`] for the freed memory to show before it would end
//!    anything else.
//!
//! This module is the policy alone, a pure function of the readings it is
//! given ([`Policy::tick`]); `crate::memory_monitor` reads the cgroup tree and
//! carries the actions out.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use crate::app_cgroup::Leaf;
use crate::priority::Tier;

const MIB: u64 = 1 << 20;

/// `some avg10` (percent of the last ten seconds some task stalled on memory)
/// from which pressure is rising.
pub const RISING_SOME: f64 = 10.0;
/// `full avg10` from which pressure is rising.
pub const RISING_FULL: f64 = 3.0;
/// `some avg10` from which pressure is critical: tasks wait on memory half
/// the time.
pub const CRITICAL_SOME: f64 = 50.0;
/// `full avg10` from which pressure is critical: everything stalls together
/// for a seventh of the time, which a person sees as a frozen screen.
pub const CRITICAL_FULL: f64 = 15.0;
/// `some avg10` below which (with `full` below [`CALM_FULL`]) the machine
/// is calm again.
pub const CALM_SOME: f64 = 5.0;
/// `full avg10` below which (with `some` below [`CALM_SOME`]) the machine is
/// calm again.
pub const CALM_FULL: f64 = 1.0;

/// Consecutive critical samples before anything is ended: a moment's spike
/// (a build linking) is no reason to end anything.
pub const CRITICAL_TICKS: u32 = 3;
/// Consecutive critical samples after which the unit still growing fastest is
/// ended even when it grew less than [`RUNAWAY_GROWTH`].
pub const STALLED_TICKS: u32 = 10;
/// Samples over which growth is measured.
pub const GROWTH_WINDOW_TICKS: u64 = 20;
/// Growth within the window that marks a unit as running away.
pub const RUNAWAY_GROWTH: u64 = 256 * MIB;
/// Samples after an end before the policy would end anything else.
pub const COOLDOWN_TICKS: u64 = 10;
/// Samples between two reclaims from the same unit.
pub const RECLAIM_EVERY_TICKS: u64 = 5;
/// Reclaiming less than this is not worth the work.
pub const MIN_RECLAIM: u64 = 16 * MIB;
/// A unit that stalls on memory this much of the time already has too little:
/// taking more from it would only make it thrash.
pub const STARVED_SOME: f64 = 60.0;

/// Why the system ended an app or a job for memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The kernel's OOM killer chose it: memory ran out.
    Kernel,
    /// The session's pressure policy ended it: it kept taking more memory
    /// while the machine ran low.
    Runaway,
}

impl Cause {
    /// How the spawner's messages spell it.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Kernel => "kernel",
            Self::Runaway => "runaway",
        }
    }

    /// The cause a message names.
    #[must_use]
    pub fn from_wire(text: &[u8]) -> Option<Self> {
        match text {
            b"kernel" => Some(Self::Kernel),
            b"runaway" => Some(Self::Runaway),
            _ => None,
        }
    }
}

/// One PSI reading: the share of the last ten seconds in which some task
/// (`some`) or every task (`full`) stalled on memory, in percent.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Psi {
    pub some_avg10: f64,
    pub full_avg10: f64,
}

/// The `avg10` values of a PSI file (`some avg10=1.23 avg60=… total=…` and
/// a `full` line). `None` when the text is not one.
#[must_use]
pub fn parse_psi(text: &str) -> Option<Psi> {
    let avg10 = |kind: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(kind))
            .and_then(|rest| {
                rest.split_whitespace()
                    .find_map(|field| field.strip_prefix("avg10="))
            })
            .and_then(|value| value.parse::<f64>().ok())
    };
    Some(Psi {
        some_avg10: avg10("some ")?,
        // A file without `full` (CPU pressure on old kernels) stalls nothing whole.
        full_avg10: avg10("full ").unwrap_or(0.0),
    })
}

/// How much memory pressure there is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Calm,
    /// Neither calm nor rising: keep watching, do nothing.
    Elevated,
    Rising,
    Critical,
}

impl Level {
    /// The level a reading shows.
    #[must_use]
    pub fn of(psi: Psi) -> Self {
        if psi.full_avg10 >= CRITICAL_FULL || psi.some_avg10 >= CRITICAL_SOME {
            Self::Critical
        } else if psi.full_avg10 >= RISING_FULL || psi.some_avg10 >= RISING_SOME {
            Self::Rising
        } else if psi.full_avg10 < CALM_FULL && psi.some_avg10 < CALM_SOME {
            Self::Calm
        } else {
            Self::Elevated
        }
    }
}

/// One app's main cgroup or one of its jobs, as a sample sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct Unit {
    /// Its cgroup directory: the app's `main` leaf or a `job-<n>` leaf.
    pub dir: PathBuf,
    /// The app's cgroup, which holds both.
    pub app_dir: PathBuf,
    pub app_id: String,
    /// What a job is called (its program); `None` for an app's main leaf.
    pub label: Option<String>,
    pub leaf: Leaf,
    pub tier: Tier,
    /// What it holds that only it can give back: anonymous memory, in RAM or
    /// compressed in swap. Page cache is not growth: the kernel drops it.
    pub footprint: u64,
    /// Everything charged to it (`memory.current`): what a reclaim works on.
    pub charged: u64,
    pub pressure: Psi,
}

/// What the policy decided for one sample.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Write `bytes` to the cgroup's `memory.reclaim`.
    Reclaim { dir: PathBuf, bytes: u64 },
    /// End the unit, which grew by `growth` bytes over the window: a job
    /// alone, an app whole.
    End { unit: Unit, growth: u64 },
}

/// The pressure policy's memory between samples.
#[derive(Debug, Default)]
pub struct Policy {
    tick: u64,
    critical_for: u32,
    /// No end before this sample.
    cooldown_until: u64,
    /// Each unit's footprint over the window, oldest first.
    history: HashMap<PathBuf, VecDeque<(u64, u64)>>,
    /// The sample of each unit's last reclaim.
    reclaimed_at: HashMap<PathBuf, u64>,
}

impl Policy {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take one sample: the machine's pressure `system` and every unit.
    /// Returns what to do now; an empty list most of the time.
    pub fn tick(&mut self, system: Psi, units: &[Unit]) -> Vec<Action> {
        self.tick = self.tick.saturating_add(1);
        self.remember(units);
        let level = Level::of(system);
        self.critical_for = if level == Level::Critical {
            self.critical_for.saturating_add(1)
        } else {
            0
        };
        let mut actions = Vec::new();
        if level >= Level::Rising {
            actions.extend(self.reclaims(units));
        }
        if self.critical_for >= CRITICAL_TICKS
            && self.tick >= self.cooldown_until
            && let Some((unit, growth)) = self.runaway(units)
        {
            self.cooldown_until = self.tick.saturating_add(COOLDOWN_TICKS);
            self.critical_for = 0;
            self.history.remove(&unit.dir);
            actions.push(Action::End {
                unit: unit.clone(),
                growth,
            });
        }
        actions
    }

    /// Whether `system` is calm: then sampling may stop until pressure
    /// rises again.
    #[must_use]
    pub fn calm(system: Psi) -> bool {
        Level::of(system) == Level::Calm
    }

    fn remember(&mut self, units: &[Unit]) {
        let now = self.tick;
        let oldest = now.saturating_sub(GROWTH_WINDOW_TICKS);
        let mut seen = std::collections::HashSet::new();
        for unit in units {
            seen.insert(unit.dir.clone());
            let samples = self.history.entry(unit.dir.clone()).or_default();
            samples.push_back((now, unit.footprint));
            while samples.front().is_some_and(|(at, _)| *at < oldest) {
                samples.pop_front();
            }
        }
        self.history.retain(|dir, _| seen.contains(dir));
        self.reclaimed_at.retain(|dir, _| seen.contains(dir));
    }

    /// How much `unit` grew over the window: now against its smallest.
    fn growth(&self, unit: &Unit) -> u64 {
        self.history
            .get(&unit.dir)
            .and_then(|samples| samples.iter().map(|(_, bytes)| *bytes).min())
            .map_or(0, |least| unit.footprint.saturating_sub(least))
    }

    fn reclaims(&mut self, units: &[Unit]) -> Vec<Action> {
        let mut actions = Vec::new();
        for unit in units {
            if unit.tier != Tier::Background || unit.pressure.some_avg10 >= STARVED_SOME {
                continue;
            }
            let due = self
                .reclaimed_at
                .get(&unit.dir)
                .is_none_or(|at| self.tick >= at.saturating_add(RECLAIM_EVERY_TICKS));
            let bytes = unit.charged.checked_div(4).unwrap_or(0);
            if !due || bytes < MIN_RECLAIM {
                continue;
            }
            self.reclaimed_at.insert(unit.dir.clone(), self.tick);
            actions.push(Action::Reclaim {
                dir: unit.dir.clone(),
                bytes,
            });
        }
        actions
    }

    /// The unit growing out of control, with its growth: the most growth of
    /// at least [`RUNAWAY_GROWTH`], or, once pressure stayed critical for
    /// [`STALLED_TICKS`], the most growth at all. Ties go to a job before an
    /// app and to the background before the focused app.
    fn runaway<'a>(&self, units: &'a [Unit]) -> Option<(&'a Unit, u64)> {
        let floor = if self.critical_for >= STALLED_TICKS {
            1
        } else {
            RUNAWAY_GROWTH
        };
        units
            .iter()
            .map(|unit| (unit, self.growth(unit)))
            .filter(|(_, growth)| *growth >= floor)
            .max_by_key(|(unit, growth)| {
                (
                    *growth,
                    unit.leaf == Leaf::Job,
                    unit.tier == Tier::Background,
                )
            })
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "test footprints are small multiples of a mebibyte"
)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    /// A scripted stand-in for the kernel's PSI files and the app tree: what
    /// each sample reads.
    struct FakeSource {
        system: Vec<Psi>,
        units: Vec<Vec<Unit>>,
    }

    impl FakeSource {
        /// Run the policy over every scripted sample; what it did, by sample.
        fn run(&self, policy: &mut Policy) -> Vec<Vec<Action>> {
            self.system
                .iter()
                .zip(&self.units)
                .map(|(system, units)| policy.tick(*system, units))
                .collect()
        }
    }

    fn unit(name: &str, leaf: Leaf, tier: Tier, footprint: u64) -> Unit {
        let app_dir = PathBuf::from(format!("/tree/app-{name}-1"));
        Unit {
            dir: match leaf {
                Leaf::Main => app_dir.join("main"),
                Leaf::Job => app_dir.join("job-1"),
            },
            app_dir,
            app_id: name.into(),
            label: (leaf == Leaf::Job).then(|| "rust-analyzer".into()),
            leaf,
            tier,
            footprint,
            charged: footprint,
            pressure: Psi::default(),
        }
    }

    const CALM: Psi = Psi {
        some_avg10: 0.5,
        full_avg10: 0.0,
    };
    const RISING: Psi = Psi {
        some_avg10: 20.0,
        full_avg10: 4.0,
    };
    const CRITICAL: Psi = Psi {
        some_avg10: 80.0,
        full_avg10: 40.0,
    };

    fn ended(actions: &[Vec<Action>]) -> Vec<(usize, String, u64)> {
        actions
            .iter()
            .enumerate()
            .flat_map(|(at, actions)| {
                actions.iter().filter_map(move |action| match action {
                    Action::End { unit, growth } => {
                        Some((at, unit.dir.display().to_string(), *growth))
                    }
                    Action::Reclaim { .. } => None,
                })
            })
            .collect()
    }

    #[test]
    fn psi_files_are_read_as_the_kernel_writes_them() {
        let text = "some avg10=12.50 avg60=3.00 avg300=0.70 total=123456\n\
                    full avg10=4.25 avg60=1.00 avg300=0.10 total=6543\n";
        assert_eq!(
            parse_psi(text),
            Some(Psi {
                some_avg10: 12.5,
                full_avg10: 4.25
            })
        );
        assert_eq!(
            parse_psi("some avg10=1.00 avg60=0 avg300=0 total=1\n"),
            Some(Psi {
                some_avg10: 1.0,
                full_avg10: 0.0
            })
        );
        assert_eq!(parse_psi("garbage"), None);
        assert_eq!(Level::of(CALM), Level::Calm);
        assert_eq!(Level::of(RISING), Level::Rising);
        assert_eq!(Level::of(CRITICAL), Level::Critical);
        assert_eq!(
            Level::of(Psi {
                some_avg10: 7.0,
                full_avg10: 0.0
            }),
            Level::Elevated
        );
        assert_eq!(
            Cause::from_wire(Cause::Runaway.wire().as_bytes()),
            Some(Cause::Runaway)
        );
        assert_eq!(
            Cause::from_wire(Cause::Kernel.wire().as_bytes()),
            Some(Cause::Kernel)
        );
    }

    #[test]
    fn rising_pressure_reclaims_from_the_background_and_never_from_the_focused_app() {
        let units = vec![
            unit("mail", Leaf::Main, Tier::Background, 2 * GIB),
            unit("notes", Leaf::Main, Tier::Foreground, 3 * GIB),
            unit("tiny", Leaf::Main, Tier::Background, 8 * MIB),
        ];
        let source = FakeSource {
            system: vec![CALM, RISING, RISING, RISING, RISING, RISING, RISING],
            units: vec![units; 7],
        };
        let mut policy = Policy::new();
        let actions = source.run(&mut policy);
        assert!(actions[0].is_empty(), "calm: nothing");
        assert_eq!(
            actions[1],
            [Action::Reclaim {
                dir: PathBuf::from("/tree/app-mail-1/main"),
                bytes: GIB / 2
            }],
            "a quarter of the background app, nothing of the focused one or of a tiny one"
        );
        for later in &actions[2..6] {
            assert!(later.is_empty(), "not again before it is due: {later:?}");
        }
        assert_eq!(actions[6].len(), 1, "again once it is due");
        assert!(ended(&actions).is_empty(), "rising pressure ends nothing");
    }

    #[test]
    fn a_starved_background_app_is_left_alone() {
        let mut starved = unit("mail", Leaf::Main, Tier::Background, 2 * GIB);
        starved.pressure.some_avg10 = 90.0;
        let mut policy = Policy::new();
        assert!(policy.tick(RISING, &[starved]).is_empty());
    }

    #[test]
    fn critical_pressure_ends_the_unit_growing_fastest_not_the_largest() {
        // The browser is the largest and steady; the language server grows
        // 200 MiB a sample; the focused editor grows a little.
        let samples: Vec<Vec<Unit>> = (0..6_u64)
            .map(|at| {
                vec![
                    unit("browser", Leaf::Main, Tier::Background, 6 * GIB),
                    unit("editor", Leaf::Job, Tier::Foreground, GIB + at * 200 * MIB),
                    unit("editor", Leaf::Main, Tier::Foreground, GIB + at * 10 * MIB),
                ]
            })
            .collect();
        let source = FakeSource {
            system: vec![CALM, CRITICAL, CRITICAL, CRITICAL, CRITICAL, CRITICAL],
            units: samples,
        };
        let mut policy = Policy::new();
        let actions = source.run(&mut policy);
        let ends = ended(&actions);
        assert_eq!(
            ends,
            [(3, "/tree/app-editor-1/job-1".to_string(), 600 * MIB)],
            "after three critical samples the runaway job ends alone: {actions:?}"
        );
        assert!(
            actions[4]
                .iter()
                .chain(&actions[5])
                .all(|action| !matches!(action, Action::End { .. })),
            "then the policy waits for the freed memory to show"
        );
    }

    #[test]
    fn a_spike_ends_nothing_and_a_cooldown_follows_every_end() {
        let growing = |at: u64| vec![unit("build", Leaf::Job, Tier::Background, at * 300 * MIB)];
        // Two critical samples, then calm: no end.
        let mut policy = Policy::new();
        for (at, system) in [CRITICAL, CRITICAL, CALM, CRITICAL, CRITICAL]
            .into_iter()
            .enumerate()
        {
            let actions = policy.tick(system, &growing(u64::try_from(at).unwrap()));
            assert!(
                actions
                    .iter()
                    .all(|action| !matches!(action, Action::End { .. })),
                "sample {at}: {actions:?}"
            );
        }
        // Critical that stays: one end, then nothing for the cooldown (the
        // samples up to the one where it lapses).
        let mut policy = Policy::new();
        let mut ends = Vec::new();
        for at in 0..(u64::from(CRITICAL_TICKS) - 1 + COOLDOWN_TICKS) {
            for action in policy.tick(CRITICAL, &growing(at)) {
                if let Action::End { .. } = action {
                    ends.push(at);
                }
            }
        }
        assert_eq!(ends, [u64::from(CRITICAL_TICKS) - 1]);
    }

    #[test]
    fn pressure_that_stays_critical_ends_whatever_still_grows() {
        // Nothing runs away, but the editor keeps creeping up while the
        // machine is stuck: after STALLED_TICKS it is ended, the steady
        // larger app is not.
        let mut policy = Policy::new();
        let mut ends = Vec::new();
        for at in 0..u64::from(STALLED_TICKS) {
            let units = [
                unit("browser", Leaf::Main, Tier::Background, 6 * GIB),
                unit("editor", Leaf::Main, Tier::Foreground, GIB + at * MIB),
            ];
            for action in policy.tick(CRITICAL, &units) {
                if let Action::End { unit, .. } = action {
                    ends.push((at, unit.app_id));
                }
            }
        }
        assert_eq!(ends, [(u64::from(STALLED_TICKS) - 1, "editor".to_string())]);
        // Nothing grows at all: nothing is ended by the policy.
        let mut policy = Policy::new();
        for _ in 0..(2 * STALLED_TICKS) {
            let actions = policy.tick(
                CRITICAL,
                &[unit("browser", Leaf::Main, Tier::Background, 6 * GIB)],
            );
            assert!(actions.iter().all(|a| !matches!(a, Action::End { .. })));
        }
    }
}
