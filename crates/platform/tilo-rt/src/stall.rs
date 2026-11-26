//! What a process was doing when it stalled.
//!
//! A server that stops answering for half a minute says nothing on its own: its log shows the
//! silence and the reconnects after it, never what held it. A process that [`watch`]es names the
//! work it does ([`doing`]) and keeps a heartbeat on its runtime ([`heartbeat`]); when a piece of
//! work takes longer than the watch's threshold, or the heartbeat stops, a thread of its own logs
//! the work outstanding, each thread's state as the kernel has it, and every thread's stack,
//! read with `eu-stack` (or `gdb`) on this process. Once per stall: it logs again only after the
//! process answered again, and at most once in [`QUIET`].
//!
//! Nothing is watched in a process that never called [`watch`]: [`doing`] then costs a load.
//! A page has no threads to watch with and never watches: what watches is not compiled there,
//! because std has no clock in a browser page and a read of one that could never run is still a
//! read the page's lint refuses.
#![cfg_attr(
    target_family = "wasm",
    allow(
        dead_code,
        reason = "a page has no thread to watch with; it names its work all the same"
    )
)]

use std::borrow::Cow;
#[cfg(not(target_family = "wasm"))]
use std::collections::BTreeMap;
#[cfg(not(target_family = "wasm"))]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(not(target_family = "wasm"))]
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
#[cfg(not(target_family = "wasm"))]
use std::time::Instant;

#[cfg(not(target_family = "wasm"))]
use tilo_lock::MutexRecover as _;

/// The least time between two reports: a stall that clears and comes back at once is one stall.
pub const QUIET: Duration = Duration::from_secs(30);

/// How often the watch looks, and the heartbeat beats.
const LOOK: Duration = Duration::from_millis(500);

/// The most of the stacks' text one report carries.
#[cfg(target_os = "linux")]
const MOST_STACK_TEXT: usize = 256 * 1024;

/// The longest a stack reader may take before it is given up.
#[cfg(target_os = "linux")]
const STACK_READ: Duration = Duration::from_secs(20);

#[cfg(not(target_family = "wasm"))]
static WATCH: OnceLock<Watch> = OnceLock::new();

#[cfg(not(target_family = "wasm"))]
struct Watch {
    /// How long a piece of work may take, or the heartbeat stay quiet, before it is a stall.
    threshold: Duration,
    /// When the watch started: the clock [`Self::beat`] and the work's starts count from.
    started: Instant,
    /// The work under way, by its number: what it is and when it began.
    doing: Mutex<BTreeMap<u64, (Cow<'static, str>, Duration)>>,
    next: AtomicU64,
    /// When the heartbeat last beat, in milliseconds since [`Self::started`]; 0 before it ever
    /// did, which is no stall.
    beat: AtomicU64,
    beating: AtomicBool,
}

/// Start watching this process for stalls of `threshold` and longer. Further calls do nothing.
/// A process on the web has no threads to watch with, and is never watched.
pub fn watch(threshold: Duration) {
    #[cfg(not(target_family = "wasm"))]
    {
        let mut first = false;
        let watch = WATCH.get_or_init(|| {
            first = true;
            Watch {
                threshold,
                started: Instant::now(),
                doing: Mutex::default(),
                next: AtomicU64::new(0),
                beat: AtomicU64::new(0),
                beating: AtomicBool::new(false),
            }
        });
        if first {
            let started = crate::spawn_thread("stall-watch", move || watch.look());
            if let Err(error) = started {
                tracing::warn!(%error, "the stall watch could not start");
            }
        }
    }
    #[cfg(target_family = "wasm")]
    let _ = threshold;
}

/// Keep a heartbeat on the runtime `handle` drives: a task that marks every half second that
/// the runtime ran it. A runtime whose workers are all held stops beating, and the watch says
/// so. Nothing without [`watch`].
pub fn heartbeat(handle: &tokio::runtime::Handle) {
    #[cfg(not(target_family = "wasm"))]
    {
        let Some(watch) = WATCH.get() else {
            return;
        };
        if watch.beating.swap(true, Ordering::AcqRel) {
            return;
        }
        handle.spawn(async move {
            // A runtime that shuts down drops this task: its heartbeat ends with it, and the
            // next runtime given one beats in its place. One left marked as beating would read
            // as a runtime quiet for ever, and hide every later stall behind that one.
            let _ends = Unbeat(watch);
            loop {
                watch
                    .beat
                    .store(watch.since_start().max(1), Ordering::Release);
                crate::sleep(LOOK).await;
            }
        });
    }
    #[cfg(target_family = "wasm")]
    let _ = handle;
}

/// What ends a runtime's heartbeat when its task is dropped with its runtime.
#[cfg(not(target_family = "wasm"))]
struct Unbeat(&'static Watch);

#[cfg(not(target_family = "wasm"))]
impl Drop for Unbeat {
    fn drop(&mut self) {
        self.0.beat.store(0, Ordering::Release);
        self.0.beating.store(false, Ordering::Release);
    }
}

/// The work `what` is under way until the answer is dropped: past the watch's threshold it is
/// a stall, and named in the report. Nothing without [`watch`].
#[must_use = "the work is watched until this is dropped"]
pub fn doing(what: impl Into<Cow<'static, str>>) -> Doing {
    #[cfg(not(target_family = "wasm"))]
    let number = WATCH.get().map(|watch| {
        let number = watch.next.fetch_add(1, Ordering::Relaxed);
        watch
            .doing
            .lock_recover()
            .insert(number, (what.into(), watch.started.elapsed()));
        number
    });
    #[cfg(target_family = "wasm")]
    let number = {
        // A page watches nothing and never reports a stall: the name the work goes by is only
        // dropped here.
        let _named: Cow<'static, str> = what.into();
        None::<u64>
    };
    Doing(number)
}

/// A piece of work under watch ([`doing`]), until it is dropped.
pub struct Doing(Option<u64>);

impl Drop for Doing {
    fn drop(&mut self) {
        #[cfg(not(target_family = "wasm"))]
        if let (Some(number), Some(watch)) = (self.0, WATCH.get()) {
            watch.doing.lock_recover().remove(&number);
        }
    }
}

#[cfg(not(target_family = "wasm"))]
impl Watch {
    fn since_start(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// The stall now, said as its lines: the work longer than the threshold, and how long the
    /// heartbeat was quiet when it is longer; `None` when nothing is stalled.
    fn stalled(&self) -> Option<Vec<String>> {
        let now = self.started.elapsed();
        let mut lines: Vec<String> = self
            .doing
            .lock_recover()
            .values()
            .filter_map(|(what, began)| {
                let took = now.saturating_sub(*began);
                (took >= self.threshold).then(|| format!("{what}: {} ms so far", took.as_millis()))
            })
            .collect();
        let beat = self.beat.load(Ordering::Acquire);
        let quiet = self.since_start().saturating_sub(beat);
        if beat > 0 && Duration::from_millis(quiet) >= self.threshold {
            lines.push(format!("the runtime's heartbeat: quiet for {quiet} ms"));
        }
        (!lines.is_empty()).then_some(lines)
    }

    fn look(&self) {
        let mut reported: Option<Instant> = None;
        let mut in_stall = false;
        loop {
            std::thread::sleep(LOOK);
            let Some(lines) = self.stalled() else {
                in_stall = false;
                continue;
            };
            if in_stall || reported.is_some_and(|at| at.elapsed() < QUIET) {
                continue;
            }
            in_stall = true;
            reported = Some(Instant::now());
            tracing::warn!(
                outstanding = %lines.join("; "),
                threads = %threads(),
                stacks = %stacks(),
                "this process stalled; what it was doing"
            );
        }
    }
}

/// Each thread of this process with its state and what the kernel says it waits in: what a
/// stall looks like without a stack reader.
fn threads() -> String {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return "unreadable".into();
    };
    let mut lines: Vec<String> = tasks
        .flatten()
        .filter_map(|task| {
            let dir = task.path();
            let stat = std::fs::read_to_string(dir.join("stat")).ok()?;
            // The name is in parentheses and may hold blanks; the state follows it.
            let (name, rest) = stat.split_once(" (")?.1.rsplit_once(") ")?;
            let state = rest.split(' ').next().unwrap_or("?");
            let wchan = std::fs::read_to_string(dir.join("wchan")).unwrap_or_default();
            let tid = task.file_name().to_string_lossy().into_owned();
            Some(format!("{tid} {name} {state} {wchan}"))
        })
        .collect();
    lines.sort();
    lines.join(" | ")
}

/// Every thread's stack, read by a debugger this process lets attach to it for the read.
#[cfg(target_os = "linux")]
fn stacks() -> String {
    let pid = std::process::id().to_string();
    // A process may only be traced by its ancestors unless it says otherwise.
    // SAFETY: PR_SET_PTRACER takes a pid (here: any) and touches no memory.
    unsafe {
        libc::prctl(libc::PR_SET_PTRACER, libc::PR_SET_PTRACER_ANY, 0, 0, 0);
    }
    let readers: [(&str, Vec<&str>); 2] = [
        ("eu-stack", vec!["-p", &pid]),
        (
            "gdb",
            vec!["-p", &pid, "-batch", "-ex", "thread apply all bt"],
        ),
    ];
    let mut said = Vec::new();
    for (reader, args) in readers {
        match read_with(reader, &args) {
            Ok(text) => {
                said.clear();
                said.push(text);
                break;
            }
            Err(why) => said.push(format!("{reader}: {why}")),
        }
    }
    // SAFETY: as above; 0 takes the permission back.
    unsafe {
        libc::prctl(libc::PR_SET_PTRACER, 0, 0, 0, 0);
    }
    let mut text = said.join("\n");
    if text.len() > MOST_STACK_TEXT {
        let end = (0..=MOST_STACK_TEXT)
            .rev()
            .find(|end| text.is_char_boundary(*end))
            .unwrap_or(0);
        text.truncate(end);
        text.push_str("\n…");
    }
    text
}

#[cfg(not(target_os = "linux"))]
fn stacks() -> String {
    "no stack reader on this platform".into()
}

/// What `reader` with `args` printed, given up after [`STACK_READ`].
#[cfg(target_os = "linux")]
fn read_with(reader: &str, args: &[&str]) -> Result<String, String> {
    use std::io::Read as _;
    let mut child = std::process::Command::new(reader)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let began = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            break status;
        }
        if began.elapsed() >= STACK_READ {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("gave up after {} s", STACK_READ.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut out);
    }
    if out.trim().is_empty() || !status.success() && !out.contains("TID") && !out.contains("Thread")
    {
        let mut err = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_string(&mut err);
        }
        return Err(format!("{status}: {}", err.trim()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Work longer than the threshold is a stall, named with how long it ran; work that ended
    /// is not, and neither is a heartbeat that never beat.
    #[test]
    fn work_past_the_threshold_is_a_stall_and_names_itself() {
        let watch = Watch {
            threshold: Duration::from_millis(50),
            started: Instant::now(),
            doing: Mutex::default(),
            next: AtomicU64::new(0),
            beat: AtomicU64::new(0),
            beating: AtomicBool::new(false),
        };
        watch
            .doing
            .lock_recover()
            .insert(0, ("a commit of the tree".into(), Duration::ZERO));
        assert!(watch.stalled().is_none(), "not yet past the threshold");
        std::thread::sleep(Duration::from_millis(80));
        let lines = watch.stalled().expect("past the threshold it is a stall");
        assert!(lines[0].starts_with("a commit of the tree: "), "{lines:?}");
        watch.doing.lock_recover().clear();
        assert!(watch.stalled().is_none(), "work that ended is no stall");
        watch.beat.store(1, Ordering::Release);
        let lines = watch.stalled().expect("a quiet heartbeat is a stall");
        assert!(
            lines[0].starts_with("the runtime's heartbeat: quiet for"),
            "{lines:?}"
        );
    }

    /// A runtime that shuts down takes its heartbeat with it, and is no stall from then on; the
    /// next runtime given one beats in its place, so a stall of that one is heard.
    #[test]
    fn a_heartbeat_ends_with_its_runtime_and_the_next_one_beats() {
        watch(Duration::from_secs(3600));
        let watching = WATCH.get().unwrap();
        // A runtime driven for a moment, while its heartbeat is waited for.
        let beats = |runtime: &tokio::runtime::Runtime| {
            heartbeat(runtime.handle());
            std::thread::scope(|scope| {
                scope.spawn(|| runtime.block_on(crate::sleep(Duration::from_millis(300))));
                let started = Instant::now();
                while watching.beat.load(Ordering::Acquire) == 0 {
                    assert!(started.elapsed() < Duration::from_secs(10), "no beat");
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
        };
        let runtime = || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
        };
        let first = runtime();
        beats(&first);
        first.shutdown_timeout(Duration::from_secs(1));
        assert_eq!(
            watching.beat.load(Ordering::Acquire),
            0,
            "a runtime that shut down still beats"
        );
        assert!(!watching.beating.load(Ordering::Acquire));
        let next = runtime();
        beats(&next);
        next.shutdown_timeout(Duration::from_secs(1));
    }
}
