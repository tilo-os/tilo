//! The memory an app nobody can see gives back first, and the CPU an app
//! that is opening is served before everything else.
//!
//! Every app starts in the startup tier: the most CPU and IO weight a cgroup
//! may carry, so the app the person just asked for is served before every
//! other app, the session's desk and any batch work while it opens
//! (`launcher::priority`). Once it has had its startup window it takes
//! the tier of what the person can see: the session is what knows which
//! windows the desk shows, so it is what moves an app between the tiers.
//! While the desk shows a window of an app the app is in the foreground tier,
//! with a memory-low floor; an app with no window in view is in the
//! background tier, whose weight is below default and whose floor is none.
//! Both writes are the same ([`launcher::app_cgroup::set_tier`]): the files a
//! launch is born with.
//!
//! The cgroup the session itself runs in carries [`Tier::Session`]: the desk
//! placing and drawing a window is work a person waits on, so it outranks
//! every app. A launch a host gave no scope of its own runs in that same
//! cgroup: it is served by the session's weight while it opens, and the
//! session's weight stays once it has, so the app is never demoted by being
//! where the session is.
//!
//! Running low on memory is handled before the kernel's OOM killer has to be
//! ([`launcher::memory_pressure`]): under rising pressure the watch on this
//! session's apps reclaims a quarter of what a background app holds
//! (`memory.reclaim`), never from an app in the foreground tier; pressure
//! that stays critical ends the one app growing out of control, and the
//! person is told the session closed it rather than that it stopped working
//! ([`Cause::Runaway`], which the launch's end carries to
//! [`crate::Spawner::events`]). An app the kernel ended for memory says so
//! too ([`Cause::Kernel`]), from its cgroup's own events.
//!
//! The watch rides on the spawner: [`crate::Spawner::new`] starts it and
//! dropping the spawner stops it, so a session that ends leaves no thread
//! behind. Where the host has no cgroup of its own for a launch (a session
//! built in a test, without a user manager) there is nothing to move or to
//! reclaim from, and the watch finds no apps.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use launcher::memory_pressure::{Psi, Unit, parse_psi};
use launcher::priority::Tier;
use tilo_lock::MutexRecover as _;

use crate::spawner::{Cause, Running, running};

/// The machine's memory pressure.
const SYSTEM_PRESSURE: &str = "/proc/pressure/memory";

/// Why memory ended an app, by its cgroup, until the app's end is reported.
pub(crate) type Ends = Arc<Mutex<BTreeMap<PathBuf, Cause>>>;

fn recorded(ends: &Ends) -> MutexGuard<'_, BTreeMap<PathBuf, Cause>> {
    ends.lock_recover()
}

/// The cgroups whose tier the pass could not set, so a host that never lets
/// it write (a job's read-only cgroup filesystem, a session with no user
/// manager of its own) is reported once instead of on every pass: the pass
/// runs once a frame, and one line per launch per frame is a log nobody can
/// read a failure out of.
fn unreported() -> &'static Mutex<BTreeSet<PathBuf>> {
    static UNREPORTED: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();
    UNREPORTED.get_or_init(|| Mutex::new(BTreeSet::new()))
}

/// Whether this pass is the first to fail for `dir`: the failure is said
/// once, and again after a later pass moved the cgroup.
fn first_failure(dir: &Path) -> bool {
    unreported().lock_recover().insert(dir.to_path_buf())
}

/// The cgroup `dir` moved to its tier: a failure after this is a new one.
fn tier_set(dir: &Path) {
    unreported().lock_recover().remove(dir);
}

/// The apps the spawner `apps` runs, by their cgroup directory and the app's
/// name: what a launch is accounted to once the launcher moved it.
pub(crate) fn cgroups_of(apps: &Running) -> Vec<(PathBuf, String)> {
    let cgroups: Vec<(PathBuf, String)> = running(apps)
        .values()
        .filter_map(|app| Some((app.cgroup_dir()?, app.name.clone())))
        .collect();
    cgroups
}

/// One launch, as the tier pass reads it: the cgroup the process it started
/// runs in, and whether it is still opening.
struct Launch {
    dir: PathBuf,
    opening: bool,
}

/// Every launch of `apps`, with the cgroup its process is in right now: the
/// scope the launcher asked the user manager for, or the session's own cgroup
/// on a host that gave the launch none.
fn launches_of(apps: &Running) -> Vec<Launch> {
    running(apps)
        .values()
        .filter_map(|app| {
            Some(Launch {
                dir: launcher::app_cgroup::cgroup_of_pid(app.pid)?,
                opening: app.opening(),
            })
        })
        .collect()
}

/// Put each cgroup of `launches` in its tier, writing only the cgroups whose
/// tier changed, and answer how many moved.
///
/// A cgroup one of whose launches is still opening keeps the startup tier, so
/// two apps sharing a cgroup (a host with no user manager runs every launch in
/// the session's) do not demote each other. Afterwards the cgroup the session
/// itself runs in is the session's tier whatever the desk shows, and a
/// launch's own cgroup takes the tier of what the person can see. `total` is
/// this machine's memory, which a foreground app's floor is an eighth of; the
/// session's own cgroup sets weights only, since its floor is the image's.
fn put_in_tiers(
    launches: &[Launch],
    visible: &[PathBuf],
    own: Option<&Path>,
    total: Option<u64>,
) -> usize {
    let mut opening: BTreeMap<&Path, bool> = BTreeMap::new();
    for launch in launches {
        *opening.entry(launch.dir.as_path()).or_default() |= launch.opening;
    }
    let mut moved: usize = 0;
    for (dir, opening) in opening {
        let tier = if opening {
            Tier::Startup
        } else if own == Some(dir) {
            Tier::Session
        } else if visible.iter().any(|visible| visible == dir) {
            Tier::Foreground
        } else {
            Tier::Background
        };
        if launcher::app_cgroup::tier_of(dir) == Some(tier) {
            continue;
        }
        let total = (own != Some(dir)).then_some(total).flatten();
        match launcher::app_cgroup::set_tier(dir, tier, total) {
            Ok(()) => {
                moved = moved.saturating_add(1);
                tier_set(dir);
            }
            Err(error) => {
                if first_failure(dir) {
                    tracing::warn!(
                        dir = %dir.display(),
                        ?tier,
                        %error,
                        "a launch keeps its tier"
                    );
                }
            }
        }
    }
    moved
}

/// Put every cgroup a launch of `apps` runs in in its tier: the startup tier
/// while the launch is opening, then the tier of what the person can see —
/// and [`Tier::Session`] for `own`, the cgroup this session runs in.
pub(crate) fn tier_by_visibility(apps: &Running, on_screen: &[u32], own: Option<&Path>) {
    let visible: Vec<PathBuf> = on_screen
        .iter()
        .filter_map(|pid| launcher::app_cgroup::cgroup_of_pid(*pid))
        .collect();
    put_in_tiers(
        &launches_of(apps),
        &visible,
        own,
        launcher::priority::mem_total_bytes(),
    );
}

/// Why memory ended the app whose cgroup is `dir`, once: what the watch
/// recorded before its kill, or what the cgroup's own events say of a kill
/// the kernel made.
pub(crate) fn take_end(ends: &Ends, dir: &Path) -> Option<Cause> {
    let known = recorded(ends).remove(dir);
    known.or_else(|| {
        let events = launcher::app_cgroup::memory_events(dir);
        (events.oom_kill > 0 || events.oom_group_kill > 0).then_some(Cause::Kernel)
    })
}

/// What the memory watch reads of each app of `apps`: the tier its cgroup is
/// in, what it holds, and its own pressure.
pub(crate) fn units_of(apps: &[(PathBuf, String)]) -> Vec<Unit> {
    apps.iter()
        .filter_map(|(dir, app)| {
            let footprint = launcher::app_cgroup::footprint(dir)?;
            Some(Unit {
                dir: dir.clone(),
                app_id: app.clone(),
                // An app whose tier cannot be read is treated as foreground:
                // never reclaimed from.
                tier: launcher::app_cgroup::tier_of(dir).unwrap_or(Tier::Foreground),
                footprint,
                charged: read_u64(dir, "memory.current").unwrap_or(footprint),
                pressure: read_psi(&dir.join("memory.pressure")).unwrap_or_default(),
            })
        })
        .collect()
}

fn read_psi(path: &Path) -> Option<Psi> {
    parse_psi(&std::fs::read_to_string(path).ok()?)
}

fn read_u64(dir: &Path, file: &str) -> Option<u64> {
    std::fs::read_to_string(dir.join(file))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The apps this session runs, as the memory watch reads them.
pub(crate) struct Source {
    pub(crate) running: Running,
}

impl launcher::memory_monitor::Source for Source {
    fn system(&mut self) -> Option<Psi> {
        read_psi(Path::new(SYSTEM_PRESSURE))
    }

    fn units(&mut self) -> Vec<Unit> {
        units_of(&cgroups_of(&self.running))
    }
}

/// What the watch does about what the policy decides.
pub(crate) struct Effects {
    pub(crate) ends: Ends,
}

impl launcher::memory_monitor::Effects for Effects {
    fn reclaim(&mut self, dir: &Path, bytes: u64) {
        if let Err(error) = launcher::app_cgroup::reclaim(dir, bytes) {
            tracing::warn!(dir = %dir.display(), %error, "memory was not reclaimed");
        }
    }

    fn end(&mut self, unit: &Unit, growth: u64) {
        let mib = growth.checked_shr(20).unwrap_or(0);
        tracing::warn!(
            app = %unit.app_id,
            mib,
            "memory is running out; closing the app"
        );
        // Recorded first: the spawner asks as soon as the launcher is reaped.
        recorded(&self.ends).insert(unit.dir.clone(), Cause::Runaway);
        if let Err(error) = launcher::app_cgroup::kill(&unit.dir) {
            tracing::warn!(app = %unit.app_id, %error, "the app did not end");
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion"
)]
mod tests {
    use super::*;
    use launcher::memory_pressure::{Action, Policy};

    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;

    const CALM: Psi = Psi {
        some_avg10: 0.5,
        full_avg10: 0.0,
    };
    const RISING: Psi = Psi {
        some_avg10: 20.0,
        full_avg10: 4.0,
    };

    /// An app's cgroup as the launcher leaves it: the tier files, its memory
    /// and its own pressure.
    fn app_cgroup(at: &Path, tier: Tier, holds: u64) -> (PathBuf, String) {
        std::fs::create_dir_all(at).unwrap();
        for file in ["cpu.weight", "io.weight", "io.bfq.weight", "memory.low"] {
            std::fs::write(at.join(file), "").unwrap();
        }
        launcher::app_cgroup::set_tier(at, tier, Some(32 * GIB)).unwrap();
        std::fs::write(at.join("memory.stat"), format!("anon {holds}\nfile 7\n")).unwrap();
        std::fs::write(at.join("memory.current"), format!("{holds}\n")).unwrap();
        std::fs::write(
            at.join("memory.pressure"),
            "some avg10=0.00 avg60=0.00 avg300=0.00 total=1\n\
             full avg10=0.00 avg60=0.00 avg300=0.00 total=1\n",
        )
        .unwrap();
        (at.to_path_buf(), String::from("so.tilo.probe"))
    }

    fn weight(app: &(PathBuf, String)) -> String {
        std::fs::read_to_string(app.0.join("cpu.weight")).unwrap()
    }

    /// A launch of `app` whose process runs in its cgroup, still opening or
    /// past its startup window.
    fn launch(app: &(PathBuf, String), opening: bool) -> Launch {
        Launch {
            dir: app.0.clone(),
            opening,
        }
    }

    fn tiered(app: &(PathBuf, String), visible: bool, opening: bool, own: Option<&Path>) -> usize {
        let visible = if visible {
            std::slice::from_ref(&app.0)
        } else {
            &[]
        };
        put_in_tiers(&[launch(app, opening)], visible, own, Some(32 * GIB))
    }

    #[test]
    fn pressure_reclaims_from_the_app_nobody_can_see_before_the_one_in_front() {
        let home = tempfile::tempdir().unwrap();
        let in_front = app_cgroup(&home.path().join("app-in-front"), Tier::Startup, 3 * GIB);
        let hidden = app_cgroup(&home.path().join("app-hidden"), Tier::Startup, 2 * GIB);
        let apps = [in_front.clone(), hidden.clone()];

        // The desk shows a window of the first app and none of the other, and
        // both have opened: the pass moves one up and the other down, writing
        // the launch's own tier files.
        let moved = put_in_tiers(
            &[launch(&in_front, false), launch(&hidden, false)],
            std::slice::from_ref(&in_front.0),
            None,
            Some(32 * GIB),
        );
        assert_eq!(moved, 2, "both launches take the tier of what is on screen");
        assert_eq!(weight(&in_front), "800");
        assert_eq!(weight(&hidden), "50", "an app nobody sees yields");
        assert_eq!(
            std::fs::read_to_string(hidden.0.join("memory.low")).unwrap(),
            "0"
        );

        // The watch samples what those cgroups say, and rising pressure takes
        // from the app nobody can see and never from the one in front.
        let units = units_of(&apps);
        assert_eq!(units.len(), 2);
        let tiers: Vec<Tier> = units.iter().map(|unit| unit.tier).collect();
        assert_eq!(tiers, [Tier::Foreground, Tier::Background]);
        let mut policy = Policy::new();
        let (mut reclaimed, mut ended) = (Vec::new(), Vec::new());
        for system in [CALM, RISING, RISING] {
            for action in policy.tick(system, &units) {
                match action {
                    Action::Reclaim { dir, .. } => reclaimed.push(dir),
                    Action::End { unit, .. } => ended.push(unit.dir),
                }
            }
        }
        assert_eq!(
            reclaimed,
            [hidden.0],
            "the app nobody can see gives memory back, the one in front keeps it"
        );
        assert!(ended.is_empty(), "rising pressure ends nothing");
    }

    #[test]
    fn an_app_that_comes_back_to_the_front_is_restored() {
        let home = tempfile::tempdir().unwrap();
        let app = app_cgroup(&home.path().join("app-probe"), Tier::Startup, GIB);
        assert_eq!(tiered(&app, false, false, None), 1);
        assert_eq!(weight(&app), "50");
        // Its window is on screen again: the same pass, the other way.
        assert_eq!(tiered(&app, true, false, None), 1);
        assert_eq!(weight(&app), "800");
        // Already there: the pass writes nothing.
        assert_eq!(tiered(&app, true, false, None), 0);
        assert!(
            has_floor(&app.0.join("memory.low")),
            "a foreground app keeps its floor"
        );
    }

    #[test]
    fn an_app_that_is_opening_is_served_before_everything_else() {
        let home = tempfile::tempdir().unwrap();
        let app = app_cgroup(&home.path().join("app-probe"), Tier::Foreground, GIB);
        // Opening, and the desk already shows its window: it keeps the
        // startup tier until its startup window has passed.
        assert_eq!(tiered(&app, true, true, None), 1);
        assert_eq!(weight(&app), "10000");
        assert!(
            has_floor(&app.0.join("memory.low")),
            "an app that is opening keeps the floor it opens with"
        );
        assert_eq!(tiered(&app, true, true, None), 0, "nothing to write");
        // Opened: the tier of what the person can see.
        assert_eq!(tiered(&app, true, false, None), 1);
        assert_eq!(weight(&app), "800");
    }

    #[test]
    fn a_launch_in_the_sessions_cgroup_keeps_the_sessions_weight_once_it_has_opened() {
        let home = tempfile::tempdir().unwrap();
        // A host with no user manager runs the launch in the session's own
        // cgroup: while it opens, that cgroup is the launch's; afterwards it is
        // the desk's, and never the background tier of an app nobody sees.
        let session = app_cgroup(&home.path().join("session.scope"), Tier::Session, GIB);
        let own = session.0.clone();
        std::fs::write(session.0.join("memory.low"), "0").unwrap();
        assert_eq!(tiered(&session, false, true, Some(&own)), 1);
        assert_eq!(weight(&session), "10000");
        assert_eq!(tiered(&session, false, false, Some(&own)), 1);
        assert_eq!(weight(&session), "1000", "the desk keeps its own weight");
        assert!(!has_floor(&session.0.join("memory.low")));
        // A second app opening in the same cgroup is not demoted by the first
        // one having opened.
        let apps = [launch(&session, false), launch(&session, true)];
        assert_eq!(
            put_in_tiers(&apps, &[], Some(&own), Some(32 * GIB)),
            1,
            "still opening, so still the startup tier"
        );
        assert_eq!(weight(&session), "10000");
    }

    fn has_floor(path: &Path) -> bool {
        std::fs::read_to_string(path).unwrap_or_default() != "0"
    }

    #[test]
    fn an_app_the_watch_closed_says_why_once_and_the_kernels_kill_says_so_too() {
        let home = tempfile::tempdir().unwrap();
        let ends: Ends = Ends::default();
        let closed = app_cgroup(&home.path().join("app-closed"), Tier::Background, GIB);
        assert_eq!(take_end(&ends, &closed.0), None);
        recorded(&ends).insert(closed.0.clone(), Cause::Runaway);
        assert_eq!(take_end(&ends, &closed.0), Some(Cause::Runaway));
        assert_eq!(take_end(&ends, &closed.0), None, "asked once");

        let killed = app_cgroup(&home.path().join("app-killed"), Tier::Foreground, GIB);
        std::fs::write(
            killed.0.join("memory.events"),
            "low 0\nhigh 0\nmax 0\noom 1\noom_kill 4\noom_group_kill 0\n",
        )
        .unwrap();
        assert_eq!(take_end(&ends, &killed.0), Some(Cause::Kernel));
    }

    #[test]
    fn an_app_that_holds_more_than_it_gives_back_is_read_whole() {
        let home = tempfile::tempdir().unwrap();
        let app = app_cgroup(&home.path().join("app-probe"), Tier::Background, 2 * GIB);
        std::fs::write(app.0.join("memory.swap.current"), format!("{}\n", 64 * MIB)).unwrap();
        let units = units_of(std::slice::from_ref(&app));
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].footprint, 2 * GIB + 64 * MIB, "swapped-out too");
        assert_eq!(units[0].charged, 2 * GIB);
        assert_eq!(units[0].app_id, "so.tilo.probe");
        // A cgroup whose app went away is no unit, and reading it is no error.
        std::fs::remove_dir_all(&app.0).unwrap();
        assert!(units_of(&[app]).is_empty());
    }
}
