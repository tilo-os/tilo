//! Interactive prioritisation: what the person is looking at wins.
//!
//! The kernel has no notion of "the app the person is using", so the desktop
//! supplies it. Three mechanisms, all expressed as systemd resource-control
//! properties on units the launcher already creates (see [`crate::scope`]),
//! never as a second cgroup hierarchy:
//!
//! * **Foreground boost.** Every launch starts in the foreground tier
//!   ([`Tier::Foreground`]): high CPU and IO weight and a memory-low floor, so
//!   the app the person just opened is never starved while it starts. When the
//!   compositor moves keyboard focus, [`notify_focus`] promotes the focused
//!   app's scope and demotes the previously focused one ([`Tier::Background`]).
//!   Demotion lowers weights and drops the floor; it never caps, so an
//!   unfocused app still uses every idle cycle, it just yields.
//! * **Session protection.** The compositor lives in its logind session scope
//!   (moving it out would break logind's pid-to-session lookup, which libseat
//!   relies on). [`protect_session`] asks the system manager to give that
//!   scope the top weights, a hard memory minimum, a low-floor, and no swap, so
//!   its hot path never takes a swap-in.
//! * **Realtime presentation.** [`request_realtime`] puts the calling thread
//!   (the compositor's present thread) in `SCHED_RR`, directly when the
//!   process may, otherwise through `RealtimeKit`.
//!
//! Everything here runs off the caller's thread and treats failure as a log
//! line: a missing manager, a refusing policy or a unit that already exited
//! costs a boost, never the session.

use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, OnceLock};

use crate::scope::BUSCTL;

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

/// CPU and IO weight of a focused app. The default is 100, so a focused app
/// outweighs a default-weighted sibling eight to one.
pub const FOREGROUND_WEIGHT: u64 = 800;
/// CPU and IO weight of an unfocused app: it keeps running, below default.
pub const BACKGROUND_WEIGHT: u64 = 50;
/// CPU and IO weight of the compositor's session scope.
pub const SESSION_WEIGHT: u64 = 1000;
/// `SCHED_RR` priority of the present thread. Low in the realtime range: it
/// must beat every normal task and nothing that matters more.
pub const PRESENT_RT_PRIORITY: i32 = 5;
/// Nice value of the compositor's main loop, which dispatches input.
pub const MAIN_LOOP_NICE: i32 = -8;

/// A systemd property value. Resource-control properties are `t` (u64) except
/// the OOM preference, a string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    U64(u64),
    Str(&'static str),
}

/// A property name and value as `SetUnitProperties` / `StartTransientUnit` take them.
pub type Property = (&'static str, Value);

/// Which side of the focus line an app scope is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Foreground,
    Background,
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
/// the foreground floor is an eighth of it, at most 4 GiB, so the protected
/// set fits inside the ancestors' floors shipped with the image.
#[must_use]
pub fn tier_properties(tier: Tier, total: Option<u64>) -> Vec<Property> {
    match tier {
        Tier::Foreground => {
            let mut props = vec![
                ("CPUWeight", Value::U64(FOREGROUND_WEIGHT)),
                ("IOWeight", Value::U64(FOREGROUND_WEIGHT)),
            ];
            if let Some(total) = total {
                props.push(("MemoryLow", Value::U64((total / 8).min(4 * GIB))));
            }
            props
        }
        Tier::Background => vec![
            ("CPUWeight", Value::U64(BACKGROUND_WEIGHT)),
            ("IOWeight", Value::U64(BACKGROUND_WEIGHT)),
            ("MemoryLow", Value::U64(0)),
        ],
    }
}

/// Properties for the compositor's session scope: top weights, a hard floor
/// for its working set, a softer one around it, and no swap so the hot path
/// never faults a page back in from zram.
#[must_use]
pub fn session_properties(total: Option<u64>) -> Vec<Property> {
    let mut props = vec![
        ("CPUWeight", Value::U64(SESSION_WEIGHT)),
        ("IOWeight", Value::U64(SESSION_WEIGHT)),
        ("MemorySwapMax", Value::U64(0)),
    ];
    if let Some(total) = total {
        props.push(("MemoryMin", Value::U64((total / 32).min(256 * MIB))));
        props.push(("MemoryLow", Value::U64((total / 16).min(GIB))));
    }
    props
}

/// The `name signature value` busctl arguments of one property.
#[must_use]
pub fn dbus_property_args(property: &Property) -> [String; 3] {
    match &property.1 {
        Value::U64(v) => [property.0.into(), "t".into(), v.to_string()],
        Value::Str(s) => [property.0.into(), "s".into(), (*s).into()],
    }
}

/// Which bus a manager answers on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bus {
    User,
    System,
}

impl Bus {
    const fn flag(self) -> &'static str {
        match self {
            Self::User => "--user",
            Self::System => "--system",
        }
    }
}

/// `busctl` arguments for `SetUnitProperties(unit, runtime=true, properties)`.
/// Runtime properties vanish with the unit and never touch the disk.
#[must_use]
pub fn set_properties_args(bus: Bus, unit: &str, props: &[Property]) -> Vec<String> {
    let mut args: Vec<String> = [
        bus.flag(),
        "--timeout=2",
        "call",
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager",
        "SetUnitProperties",
        "sba(sv)",
    ]
    .map(String::from)
    .to_vec();
    args.push(unit.into());
    args.push("true".into());
    args.push(props.len().to_string());
    args.extend(props.iter().flat_map(dbus_property_args));
    args
}

/// The app scope (`app-….scope` under `app.slice`) a `/proc/<pid>/cgroup`
/// text places a process in, with the unit name as systemd spells it.
#[must_use]
pub fn app_scope_of(cgroup: &str) -> Option<String> {
    cgroup.lines().find_map(|line| {
        let path = line.rsplit(':').next()?;
        let mut parts = path.split('/');
        parts.find(|part| *part == "app.slice")?;
        let scope = parts.next()?;
        (scope.starts_with("app-") && scope.ends_with(".scope")).then(|| scope.to_string())
    })
}

/// The logind session scope (`session-N.scope`) a cgroup text places a
/// process in.
#[must_use]
pub fn session_scope_of(cgroup: &str) -> Option<String> {
    cgroup.lines().find_map(|line| {
        line.rsplit('/')
            .find(|part| {
                part.strip_prefix("session-")
                    .and_then(|rest| rest.strip_suffix(".scope"))
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            })
            .map(str::to_string)
    })
}

/// How the priority logic reaches systemd and the process table. Production
/// uses [`BusctlControl`]; tests substitute a recorder.
pub trait UnitControl: Send + 'static {
    /// The app scope `pid` runs in, if any.
    fn app_scope_of_pid(&self, pid: u32) -> Option<String>;
    /// Whether the process still exists.
    fn pid_alive(&self, pid: u32) -> bool;
    /// Apply runtime properties to a user-manager unit.
    ///
    /// # Errors
    ///
    /// The manager's reason when it refuses or is unreachable.
    fn set_properties(&self, unit: &str, props: &[Property]) -> Result<(), String>;
}

/// [`UnitControl`] over `/proc` and the system's `busctl`, the same front end
/// the launcher uses to create scopes.
pub struct BusctlControl;

impl UnitControl for BusctlControl {
    fn app_scope_of_pid(&self, pid: u32) -> Option<String> {
        app_scope_of(&std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?)
    }

    fn pid_alive(&self, pid: u32) -> bool {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }

    fn set_properties(&self, unit: &str, props: &[Property]) -> Result<(), String> {
        run_busctl(&set_properties_args(Bus::User, unit, props))
    }
}

fn run_busctl(args: &[String]) -> Result<(), String> {
    let output = Command::new(BUSCTL)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("running {BUSCTL}: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// What the compositor tells the focus policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The compositor started the launcher with this pid. Its scope is born
    /// in the foreground tier and is protected until its window appears.
    Launch(u32),
    /// The launcher with this pid exited.
    Exited(u32),
    /// A toplevel of the client with this pid was created.
    Window(u32),
    /// Keyboard focus moved to the client with this pid, or to shell chrome
    /// (`None`).
    Focus(Option<u32>),
    /// The compositor finished the tick in which windows appeared and decided
    /// where focus goes: any launch whose window did not get focus yields now.
    Settle,
}

/// A launch waiting for its first window.
struct Launch {
    pid: u32,
    seq: u64,
    /// What had focus when it began: coming back to it (a search closing, the
    /// app it was started from) is not the person moving on.
    focused_at: Option<String>,
}

/// Focus-driven tiering. Invariant after every decision ([`FocusBoost::handle`]
/// with a focus or settle event): the scopes in the foreground tier are the
/// focused app and the launches still waiting for their first window, nothing
/// else. There is no clock: a pending launch, however slow to start under
/// load, keeps its boost until its first window appears (focus then decides),
/// its process exits, or the person focuses a different window that already
/// existed when the launch began, meaning they moved on.
pub struct FocusBoost<C> {
    control: C,
    total: Option<u64>,
    /// The app that last held keyboard focus. Chrome keeps it: the person is
    /// still working with it (search, a switcher).
    focused: Option<String>,
    /// Every scope believed to be in the foreground tier (every scope is born
    /// there).
    foreground: HashSet<String>,
    /// The app that last received focus, boosted or not yet.
    seen_focus: Option<String>,
    /// Launches whose window has not appeared.
    launches: Vec<Launch>,
    /// Event counter ordering launches and windows.
    seq: u64,
    /// The `seq` of each scope's latest window, while a launch is pending: a
    /// window newer than a launch is not an existing one.
    window_seq: HashMap<String, u64>,
}

impl<C: UnitControl> FocusBoost<C> {
    #[must_use]
    pub fn new(control: C, total: Option<u64>) -> Self {
        Self {
            control,
            total,
            focused: None,
            foreground: HashSet::new(),
            seen_focus: None,
            launches: Vec::new(),
            seq: 0,
            window_seq: HashMap::new(),
        }
    }

    /// The scope focus last settled on.
    #[must_use]
    pub fn focused(&self) -> Option<&str> {
        self.focused.as_deref()
    }

    /// The scopes in the foreground tier.
    #[must_use]
    pub fn foreground(&self) -> Vec<&str> {
        let mut scopes: Vec<&str> = self.foreground.iter().map(String::as_str).collect();
        scopes.sort_unstable();
        scopes
    }

    /// Apply `events` in order, then make the tiers match: boost the newly
    /// focused scope, demote everything that is neither focused nor a launch
    /// still waiting for its window. A boost the manager refuses is retried at
    /// the next focus; a demotion of a scope that already exited is moot.
    pub fn handle(&mut self, events: impl IntoIterator<Item = Event>) {
        let mut boost = None;
        // Focus, Settle and an exit are where the picture changes; a window on
        // its own is followed by the compositor's focus decision in the same
        // tick, so demoting on it would only flicker the tier.
        let mut decided = false;
        for event in events {
            self.seq += 1;
            match event {
                Event::Launch(pid) => self.launches.push(Launch {
                    pid,
                    seq: self.seq,
                    focused_at: self.seen_focus.clone(),
                }),
                Event::Exited(pid) => {
                    self.launches.retain(|launch| launch.pid != pid);
                    decided = true;
                }
                Event::Window(pid) => {
                    if let Some(scope) = self.control.app_scope_of_pid(pid) {
                        let control = &self.control;
                        self.launches.retain(|launch| {
                            control.app_scope_of_pid(launch.pid).as_deref() != Some(&scope)
                        });
                        if !self.launches.is_empty() {
                            self.window_seq.insert(scope.clone(), self.seq);
                        }
                        self.foreground.insert(scope);
                    }
                }
                Event::Focus(pid) => {
                    decided = true;
                    // A pid outside any app scope (the shell's own surfaces) changes nothing.
                    if let Some(scope) = pid.and_then(|pid| self.control.app_scope_of_pid(pid)) {
                        self.moved_on_to(&scope);
                        self.seen_focus = Some(scope.clone());
                        boost = Some(scope);
                    }
                }
                Event::Settle => decided = true,
            }
        }
        if let Some(scope) = boost
            && self.focused.as_deref() != Some(scope.as_str())
        {
            let props = tier_properties(Tier::Foreground, self.total);
            match self.control.set_properties(&scope, &props) {
                Ok(()) => {
                    self.foreground.insert(scope.clone());
                    self.focused = Some(scope);
                }
                Err(error) => eprintln!("tilo-priority: boosting {scope}: {error}"),
            }
        }
        if decided {
            self.reconcile();
        }
    }

    /// Focus reached `scope`. Pending launches are released when it is a
    /// different window that already existed when they began.
    fn moved_on_to(&mut self, scope: &str) {
        let control = &self.control;
        let window_seq = &self.window_seq;
        self.launches.retain(|launch| {
            control.app_scope_of_pid(launch.pid).as_deref() == Some(scope)
                || launch.focused_at.as_deref() == Some(scope)
                || window_seq.get(scope).is_some_and(|opened| *opened > launch.seq)
        });
    }

    fn reconcile(&mut self) {
        let control = &self.control;
        // A launch's scope is born in the foreground tier whether or not a
        // window ever follows; learn it so that it can be demoted.
        self.foreground
            .extend(self.launches.iter().filter_map(|launch| control.app_scope_of_pid(launch.pid)));
        self.launches.retain(|launch| control.pid_alive(launch.pid));
        if self.launches.is_empty() {
            self.window_seq.clear();
        }
        let mut keep: HashSet<String> = self
            .launches
            .iter()
            .filter_map(|launch| control.app_scope_of_pid(launch.pid))
            .collect();
        keep.extend(self.focused.clone());
        let demote = tier_properties(Tier::Background, self.total);
        let mut victims: Vec<String> = self.foreground.difference(&keep).cloned().collect();
        victims.sort_unstable();
        for scope in victims {
            // A scope that already exited makes this fail; nothing to demote then.
            let _ = self.control.set_properties(&scope, &demote);
            self.foreground.remove(&scope);
        }
    }
}

/// Runs a [`FocusBoost`] on its own thread so the compositor never waits on
/// systemd. Events queue without blocking; a backlog is applied in one pass.
pub struct FocusWorker {
    tx: Sender<Event>,
}

impl FocusWorker {
    /// Start the worker thread.
    #[must_use]
    pub fn spawn<C: UnitControl>(control: C) -> Self {
        let (tx, rx): (Sender<Event>, Receiver<Event>) = channel();
        let spawned = std::thread::Builder::new()
            .name("tilo-focus-boost".into())
            .spawn(move || {
                let mut boost = FocusBoost::new(control, mem_total_bytes());
                loop {
                    let Ok(first) = rx.recv() else { break };
                    let mut batch = vec![first];
                    batch.extend(rx.try_iter());
                    boost.handle(batch);
                }
            });
        if let Err(error) = spawned {
            eprintln!("tilo-priority: no focus-boost thread: {error}");
        }
        Self { tx }
    }

    /// Queue an event; never blocks.
    pub fn notify(&self, event: Event) {
        let _ = self.tx.send(event);
    }
}

static WORKER: OnceLock<Mutex<Option<FocusWorker>>> = OnceLock::new();
static WINDOWS_OPENED: AtomicBool = AtomicBool::new(false);

/// Turn on focus boosting for this process, the compositor. Left off in tests
/// and tools so they never retune the scope of the terminal they run in.
pub fn enable_focus_boost() {
    let slot = WORKER.get_or_init(|| Mutex::new(None));
    if let Ok(mut slot) = slot.lock()
        && slot.is_none()
    {
        *slot = Some(FocusWorker::spawn(BusctlControl));
    }
}

fn send(event: Event) {
    if let Some(slot) = WORKER.get()
        && let Ok(slot) = slot.lock()
        && let Some(worker) = slot.as_ref()
    {
        worker.notify(event);
    }
}

/// Keyboard focus moved to the client with `pid` (from `SO_PEERCRED`).
/// A no-op unless [`enable_focus_boost`] ran.
pub fn notify_focus(pid: u32) {
    send(Event::Focus(Some(pid)));
}

/// The compositor started a launcher with `pid`.
pub fn notify_launch(pid: u32) {
    send(Event::Launch(pid));
}

/// The launcher with `pid` exited.
pub fn notify_launch_exited(pid: u32) {
    send(Event::Exited(pid));
}

/// A toplevel of the client `pid` was created. Pair with [`settle_windows`]
/// once the tick has decided focus.
pub fn notify_window(pid: u32) {
    if WORKER.get().is_some() {
        WINDOWS_OPENED.store(true, Ordering::Release);
        send(Event::Window(pid));
    }
}

/// Call after the compositor reconciled keyboard focus: if windows appeared
/// since the last call, the launches among them that did not get focus yield.
pub fn settle_windows() {
    if WINDOWS_OPENED.swap(false, Ordering::AcqRel) {
        send(Event::Settle);
    }
}

/// Ask the system manager to protect the login session scope this process
/// runs in. Asynchronous. Authorised by the image's polkit rule, which names
/// only `session-N.scope` units; a process outside a login session does
/// nothing.
pub fn protect_session() {
    let Ok(cgroup) = std::fs::read_to_string("/proc/self/cgroup") else {
        return;
    };
    let Some(scope) = session_scope_of(&cgroup) else {
        return;
    };
    let props = session_properties(mem_total_bytes());
    // Separate call: a manager that lacks the property must not cost the rest.
    let oom = [("ManagedOOMPreference", Value::Str("avoid"))];
    let spawned = std::thread::Builder::new()
        .name("tilo-session-protect".into())
        .spawn(move || {
            for props in [&props[..], &oom[..]] {
                if let Err(error) = run_busctl(&set_properties_args(Bus::System, &scope, props)) {
                    eprintln!("tilo-priority: protecting {scope}: {error}");
                }
            }
        });
    if let Err(error) = spawned {
        eprintln!("tilo-priority: no session-protect thread: {error}");
    }
}

/// `busctl` arguments for `RealtimeKit`'s `MakeThreadRealtime`.
#[must_use]
pub fn rtkit_args(tid: u64, priority: i32) -> Vec<String> {
    [
        "--system",
        "--timeout=2",
        "call",
        "org.freedesktop.RealtimeKit1",
        "/org/freedesktop/RealtimeKit1",
        "org.freedesktop.RealtimeKit1",
        "MakeThreadRealtime",
        "tu",
    ]
    .map(String::from)
    .into_iter()
    .chain([tid.to_string(), priority.to_string()])
    .collect()
}

/// Put the calling thread in `SCHED_RR` at `priority`, with reset-on-fork so
/// nothing it spawns inherits the policy. Direct when the process may
/// (`CAP_SYS_NICE` or `RLIMIT_RTPRIO`), otherwise requested from `RealtimeKit`
/// on a helper thread so the caller never waits on the bus.
///
/// A realtime thread that spins starves everything below it, so the grant is
/// bounded twice. `RLIMIT_RTTIME` makes the kernel send `SIGXCPU` once the
/// thread has run [`RT_SOFT_LIMIT_US`] without blocking; the handler installed
/// here drops that thread back to `SCHED_OTHER` and logs it, so a spin costs
/// latency, not the session. The hard limit, [`RT_HARD_LIMIT_US`] (the ceiling
/// `RealtimeKit` accepts), only matters if the handler cannot run: then the
/// kernel kills the process rather than let the machine freeze.
/// `TILO_PRESENT_RT=0` opts out.
#[cfg(target_os = "linux")]
pub fn request_realtime(priority: i32) {
    if std::env::var_os("TILO_PRESENT_RT").is_some_and(|v| v == "0") {
        return;
    }
    if !install_realtime_guard() {
        return;
    }
    let param = libc::sched_param {
        sched_priority: priority,
    };
    // SAFETY: plain syscall on a stack struct; pid 0 is the calling thread.
    let direct =
        unsafe { libc::sched_setscheduler(0, libc::SCHED_RR | libc::SCHED_RESET_ON_FORK, &param) };
    if direct == 0 {
        return;
    }
    if !in_login_session() {
        return;
    }
    let tid = current_tid();
    let _ = std::thread::Builder::new()
        .name("tilo-rtkit".into())
        .spawn(move || {
            if let Err(error) = run_busctl(&rtkit_args(tid, priority)) {
                eprintln!("tilo-priority: realtime for thread {tid}: {error}");
            }
        });
}

/// Soft `RLIMIT_RTTIME`: far above any frame (a present thread blocks on the
/// GPU and the display every few milliseconds), far below a visible stall.
#[cfg(target_os = "linux")]
const RT_SOFT_LIMIT_US: libc::rlim_t = 50_000;
/// Hard `RLIMIT_RTTIME`; `RealtimeKit` refuses anything above 200 ms.
#[cfg(target_os = "linux")]
const RT_HARD_LIMIT_US: libc::rlim_t = 200_000;

/// Times the `SIGXCPU` handler dropped a thread out of realtime.
#[cfg(target_os = "linux")]
static RT_DEMOTIONS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// `SIGXCPU` handler: the kernel signals the offending thread, so dropping
/// the calling thread to `SCHED_OTHER` ends the spin's priority. Only
/// async-signal-safe calls. Reset-on-fork must stay set: the kernel refuses
/// an unprivileged thread clearing it.
#[cfg(target_os = "linux")]
extern "C" fn on_realtime_overrun(_signal: libc::c_int) {
    const MESSAGE: &[u8] = b"tilo-priority: a realtime thread ran too long without blocking; demoted to SCHED_OTHER\n";
    let param = libc::sched_param { sched_priority: 0 };
    // SAFETY: both are async-signal-safe plain syscalls on valid memory.
    unsafe {
        libc::sched_setscheduler(0, libc::SCHED_OTHER | libc::SCHED_RESET_ON_FORK, &param);
        libc::write(2, MESSAGE.as_ptr().cast(), MESSAGE.len());
    }
    RT_DEMOTIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Install the overrun handler and the limits once; whether they are in force.
#[cfg(target_os = "linux")]
fn install_realtime_guard() -> bool {
    static GUARD: OnceLock<bool> = OnceLock::new();
    *GUARD.get_or_init(|| {
        // SAFETY: a zeroed sigaction is valid; the handler is async-signal-safe.
        let installed = unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_realtime_overrun as extern "C" fn(libc::c_int) as usize;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&raw mut action.sa_mask);
            libc::sigaction(libc::SIGXCPU, &raw const action, std::ptr::null_mut()) == 0
        };
        if !installed {
            return false;
        }
        let limit = libc::rlimit {
            rlim_cur: RT_SOFT_LIMIT_US,
            rlim_max: RT_HARD_LIMIT_US,
        };
        // SAFETY: plain syscall on a stack struct.
        unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &raw const limit) == 0 }
    })
}

#[cfg(target_os = "linux")]
fn in_login_session() -> bool {
    std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .is_some_and(|cgroup| session_scope_of(&cgroup).is_some())
}

#[cfg(target_os = "linux")]
fn current_tid() -> u64 {
    // SAFETY: gettid takes no arguments.
    u64::try_from(unsafe { libc::gettid() }).unwrap_or(0)
}

/// `busctl` arguments for `RealtimeKit`'s `MakeThreadHighPriority`.
#[must_use]
pub fn rtkit_nice_args(tid: u64, nice: i32) -> Vec<String> {
    [
        "--system",
        "--timeout=2",
        "call",
        "org.freedesktop.RealtimeKit1",
        "/org/freedesktop/RealtimeKit1",
        "org.freedesktop.RealtimeKit1",
        "MakeThreadHighPriority",
        "ti",
    ]
    .map(String::from)
    .into_iter()
    .chain([tid.to_string(), nice.to_string()])
    .collect()
}

/// Give the calling thread a better nice value, directly or through
/// `RealtimeKit`. The compositor dispatches input on its main loop (libinput
/// is polled by the same calloop that serves clients), so there is no input
/// thread to put in realtime; this raises the main loop instead. Nice is
/// inherited by children of the thread, so call it only on the main thread,
/// which never forks launches (those run on worker threads).
/// `TILO_MAIN_NICE=0` opts out.
#[cfg(target_os = "linux")]
pub fn request_main_loop_nice(nice: i32) {
    if std::env::var_os("TILO_MAIN_NICE").is_some_and(|v| v == "0") {
        return;
    }
    let tid = current_tid();
    // SAFETY: plain syscall; a thread id names that thread alone on Linux.
    let direct = unsafe {
        libc::setpriority(
            libc::PRIO_PROCESS as _,
            libc::id_t::try_from(tid).unwrap_or(0),
            nice,
        )
    };
    if direct == 0 || !in_login_session() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("tilo-rtkit".into())
        .spawn(move || {
            if let Err(error) = run_busctl(&rtkit_nice_args(tid, nice)) {
                eprintln!("tilo-priority: nice {nice} for thread {tid}: {error}");
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    type Calls = Rc<RefCell<Vec<(String, Vec<Property>)>>>;

    /// A recording stand-in for systemd. `Rc` is not `Send`, so the trait's
    /// `Send` bound is satisfied by a wrapper that is only used in-thread.
    struct Mock {
        scopes: HashMap<u32, String>,
        dead: Rc<RefCell<Vec<u32>>>,
        calls: Calls,
        fail: Option<String>,
    }
    struct Unsend(Mock);
    // SAFETY: the tests below never move a mock to another thread.
    unsafe impl Send for Unsend {}
    impl UnitControl for Unsend {
        fn app_scope_of_pid(&self, pid: u32) -> Option<String> {
            self.0.scopes.get(&pid).cloned()
        }
        fn pid_alive(&self, pid: u32) -> bool {
            !self.0.dead.borrow().contains(&pid)
        }
        fn set_properties(&self, unit: &str, props: &[Property]) -> Result<(), String> {
            if self.0.fail.as_deref() == Some(unit) {
                return Err("Unit not found".into());
            }
            self.0.calls.borrow_mut().push((unit.into(), props.to_vec()));
            Ok(())
        }
    }

    struct Rig {
        boost: FocusBoost<Unsend>,
        calls: Calls,
    }

    impl Rig {
        fn new(fail: Option<&str>) -> Self {
            let calls: Calls = Rc::default();
            let scopes = [(10, "a"), (20, "b"), (30, "c")]
                .into_iter()
                .map(|(p, s)| (p, format!("app-tilo-{s}-{p}.scope")))
                .collect();
            let mock = Mock {
                scopes,
                dead: Rc::default(),
                calls: calls.clone(),
                fail: fail.map(str::to_string),
            };
            Self {
                boost: FocusBoost::new(Unsend(mock), Some(32 * GIB)),
                calls,
            }
        }
        fn send(&mut self, events: &[Event]) {
            self.boost.handle(events.iter().copied());
        }
        /// A launched app, in the foreground tier from birth, opens a window.
        fn open(&mut self, pid: u32) {
            self.send(&[Event::Launch(pid), Event::Window(pid)]);
        }
    }

    fn scope(pid: u32, name: &str) -> String {
        format!("app-tilo-{name}-{pid}.scope")
    }

    #[test]
    fn tiers_boost_and_demote_without_capping() {
        let fg = tier_properties(Tier::Foreground, Some(32 * GIB));
        assert_eq!(
            fg,
            [
                ("CPUWeight", Value::U64(800)),
                ("IOWeight", Value::U64(800)),
                ("MemoryLow", Value::U64(4 * GIB)),
            ]
        );
        // An eighth of a small machine, not the cap.
        assert!(tier_properties(Tier::Foreground, Some(4 * GIB))
            .contains(&("MemoryLow", Value::U64(GIB / 2))));
        let bg = tier_properties(Tier::Background, Some(32 * GIB));
        assert!(bg.contains(&("MemoryLow", Value::U64(0))));
        // Never a hard limit, in either direction.
        for (name, _) in fg.iter().chain(&bg) {
            assert!(!name.contains("Max") && !name.contains("High") && *name != "CPUQuota");
        }
    }

    #[test]
    fn focus_boosts_the_new_app_and_demotes_the_old_one() {
        let mut rig = Rig::new(None);
        rig.send(&[Event::Focus(Some(10))]);
        rig.send(&[Event::Focus(Some(10))]);
        assert_eq!(rig.calls.borrow().len(), 1, "refocusing the same app is free");
        rig.send(&[Event::Focus(Some(20))]);
        assert_eq!(rig.boost.foreground(), [scope(20, "b")]);
        let calls = rig.calls.borrow();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[1], (scope(20, "b"), tier_properties(Tier::Foreground, Some(32 * GIB))));
        assert_eq!(calls[2], (scope(10, "a"), tier_properties(Tier::Background, Some(32 * GIB))));
    }

    #[test]
    fn chrome_and_foreign_pids_leave_the_boost_alone() {
        let mut rig = Rig::new(None);
        rig.send(&[Event::Focus(Some(10))]);
        rig.send(&[Event::Focus(None), Event::Focus(Some(999))]);
        assert_eq!(rig.boost.foreground(), [scope(10, "a")]);
        assert_eq!(rig.calls.borrow().len(), 1);
    }

    #[test]
    fn a_launch_stays_foreground_until_its_window_and_then_follows_focus() {
        let mut rig = Rig::new(None);
        rig.open(10);
        rig.send(&[Event::Focus(Some(10)), Event::Settle]);
        // B launches while A is focused: both are foreground, B is about to be focused.
        rig.send(&[Event::Launch(20), Event::Settle]);
        assert_eq!(rig.boost.foreground(), [scope(10, "a"), scope(20, "b")]);
        // Its window opens and takes focus: A yields.
        rig.send(&[Event::Window(20), Event::Focus(Some(20)), Event::Settle]);
        assert_eq!(rig.boost.foreground(), [scope(20, "b")]);
    }

    #[test]
    fn a_launch_whose_window_does_not_get_focus_yields_when_it_settles() {
        let mut rig = Rig::new(None);
        rig.open(10);
        rig.send(&[Event::Focus(Some(10)), Event::Settle]);
        // The window appears but the compositor leaves focus on A.
        rig.send(&[Event::Launch(20), Event::Window(20), Event::Settle]);
        assert_eq!(rig.boost.foreground(), [scope(10, "a")]);
        assert_eq!(
            rig.calls.borrow().last().unwrap(),
            &(scope(20, "b"), tier_properties(Tier::Background, Some(32 * GIB)))
        );
    }

    #[test]
    fn focus_moving_elsewhere_demotes_a_windowed_unfocused_launch() {
        let mut rig = Rig::new(None);
        rig.open(10);
        rig.open(20);
        rig.send(&[Event::Focus(Some(30))]);
        assert_eq!(rig.boost.foreground(), [scope(30, "c")]);
    }

    #[test]
    fn a_slow_launch_keeps_its_boost_however_long_it_takes() {
        let mut rig = Rig::new(None);
        rig.open(10);
        rig.send(&[Event::Focus(Some(10)), Event::Settle]);
        rig.send(&[Event::Launch(20), Event::Settle]);
        // Nothing else happens, for as long as the app needs: no clock demotes it.
        for _ in 0..1000 {
            rig.send(&[Event::Settle]);
        }
        assert_eq!(rig.boost.foreground(), [scope(10, "a"), scope(20, "b")]);
    }

    #[test]
    fn focusing_a_different_existing_window_releases_a_pending_launch() {
        let mut rig = Rig::new(None);
        rig.open(10);
        rig.open(30);
        rig.send(&[Event::Focus(Some(10)), Event::Settle]);
        rig.send(&[Event::Launch(20), Event::Settle]);
        // Coming back to what had focus at launch is not moving on.
        rig.send(&[Event::Focus(None), Event::Focus(Some(10)), Event::Settle]);
        assert_eq!(rig.boost.foreground(), [scope(10, "a"), scope(20, "b")]);
        // The person goes to another window that was already there.
        rig.send(&[Event::Focus(Some(30))]);
        assert_eq!(rig.boost.foreground(), [scope(30, "c")]);
        // The launch's own window now opening and taking focus boosts it afresh.
        rig.send(&[Event::Window(20), Event::Focus(Some(20)), Event::Settle]);
        assert_eq!(rig.boost.foreground(), [scope(20, "b")]);
    }

    #[test]
    fn another_launchs_new_window_taking_focus_does_not_release_a_pending_launch() {
        let mut rig = Rig::new(None);
        rig.open(10);
        rig.send(&[Event::Focus(Some(10)), Event::Settle]);
        rig.send(&[Event::Launch(20), Event::Launch(30)]);
        rig.send(&[Event::Window(30), Event::Focus(Some(30)), Event::Settle]);
        assert_eq!(rig.boost.foreground(), [scope(20, "b"), scope(30, "c")]);
    }

    #[test]
    fn a_launch_that_exits_without_a_window_stops_being_protected() {
        let mut rig = Rig::new(None);
        rig.send(&[Event::Launch(10), Event::Settle]);
        assert_eq!(rig.boost.foreground(), [scope(10, "a")]);
        rig.send(&[Event::Exited(10)]);
        assert!(rig.boost.foreground().is_empty());
    }

    #[test]
    fn at_most_one_scope_is_boosted_after_every_decision() {
        // Three apps launch and open windows; after each the compositor puts
        // focus on any of the three or on chrome. Every choice sequence.
        let pids = [10, 20, 30];
        for choices in 0..4_u32.pow(3) {
            let mut rig = Rig::new(None);
            for (step, pid) in pids.into_iter().enumerate() {
                let choice = (choices / 4_u32.pow(u32::try_from(step).unwrap())) % 4;
                rig.send(&[Event::Launch(pid), Event::Window(pid)]);
                let focus = usize::try_from(choice).ok().and_then(|i| i.checked_sub(1)).map(|i| pids[i]);
                rig.send(&[Event::Focus(focus), Event::Settle]);
                assert!(rig.boost.foreground().len() <= 1, "{choices} {step}: {:?}", rig.boost.foreground());
            }
        }
    }

    #[test]
    fn a_refused_boost_is_retried_on_the_next_focus_and_a_dead_demotion_is_ignored() {
        let mut rig = Rig::new(Some("app-tilo-b-20.scope"));
        rig.send(&[Event::Focus(Some(10))]);
        rig.send(&[Event::Focus(Some(20))]);
        assert_eq!(rig.boost.focused(), Some("app-tilo-a-10.scope"));
        let mut rig = Rig::new(Some("app-tilo-a-10.scope"));
        rig.send(&[Event::Focus(Some(10))]);
        assert_eq!(rig.boost.focused(), None);
        rig.send(&[Event::Focus(Some(20))]);
        assert_eq!(rig.boost.focused(), Some("app-tilo-b-20.scope"));
        assert_eq!(rig.calls.borrow().len(), 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_realtime_overrun_signal_drops_the_thread_to_the_normal_policy() {
        use std::sync::atomic::Ordering;
        assert!(install_realtime_guard());
        let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        // SAFETY: plain syscall on a stack struct.
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_RTTIME, &raw mut limit) }, 0);
        assert!(limit.rlim_cur <= RT_SOFT_LIMIT_US && limit.rlim_cur < limit.rlim_max);
        assert!(limit.rlim_max <= RT_HARD_LIMIT_US);
        std::thread::spawn(|| {
            let before = RT_DEMOTIONS.load(Ordering::Relaxed);
            let param = libc::sched_param { sched_priority: PRESENT_RT_PRIORITY };
            // SAFETY: plain syscalls on this thread.
            let realtime = unsafe {
                libc::sched_setscheduler(0, libc::SCHED_RR | libc::SCHED_RESET_ON_FORK, &param)
            } == 0;
            // The kernel delivers the limit's signal to the offending thread.
            // SAFETY: signalling this very thread.
            assert_eq!(unsafe { libc::pthread_kill(libc::pthread_self(), libc::SIGXCPU) }, 0);
            assert_eq!(RT_DEMOTIONS.load(Ordering::Relaxed), before + 1);
            // SAFETY: plain syscall.
            let policy = unsafe { libc::sched_getscheduler(0) } & !libc::SCHED_RESET_ON_FORK;
            assert_eq!(policy, libc::SCHED_OTHER, "realtime was granted: {realtime}");
        })
        .join()
        .expect("the process survives the signal and the thread is demoted");
    }

    #[test]
    fn scopes_are_found_by_cgroup_path() {
        let app = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-tilo-a\\x2db-7.scope\n";
        assert_eq!(app_scope_of(app).as_deref(), Some("app-tilo-a\\x2db-7.scope"));
        let nested = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-x-1.scope/sub\n";
        assert_eq!(app_scope_of(nested).as_deref(), Some("app-x-1.scope"));
        assert_eq!(
            app_scope_of("0::/user.slice/user-1000.slice/session-2.scope\n"),
            None
        );
        assert_eq!(
            app_scope_of("0::/user.slice/user-1000.slice/user@1000.service/background.slice/app-x-1.scope\n"),
            None
        );
        assert_eq!(
            session_scope_of("0::/user.slice/user-1000.slice/session-2.scope\n").as_deref(),
            Some("session-2.scope")
        );
        assert_eq!(session_scope_of("0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-x-1.scope\n"), None);
        assert_eq!(parse_mem_total("MemTotal:       32213144 kB\n"), Some(32213144 * 1024));
    }

    #[test]
    fn the_session_scope_is_protected_and_kept_out_of_swap() {
        let props = session_properties(Some(32 * GIB));
        for expected in [
            ("CPUWeight", Value::U64(1000)),
            ("IOWeight", Value::U64(1000)),
            ("MemorySwapMax", Value::U64(0)),
            ("MemoryMin", Value::U64(256 * MIB)),
            ("MemoryLow", Value::U64(GIB)),
        ] {
            assert!(props.contains(&expected), "{expected:?}");
        }
        assert_eq!(
            set_properties_args(Bus::System, "session-2.scope", &[("CPUWeight", Value::U64(1000)), ("ManagedOOMPreference", Value::Str("avoid"))]),
            [
                "--system", "--timeout=2", "call", "org.freedesktop.systemd1",
                "/org/freedesktop/systemd1", "org.freedesktop.systemd1.Manager",
                "SetUnitProperties", "sba(sv)", "session-2.scope", "true", "2",
                "CPUWeight", "t", "1000", "ManagedOOMPreference", "s", "avoid",
            ]
        );
        assert_eq!(rtkit_args(77, 5)[6..], ["MakeThreadRealtime", "tu", "77", "5"]);
        assert_eq!(rtkit_nice_args(77, -8)[6..], ["MakeThreadHighPriority", "ti", "77", "-8"]);
    }
}
