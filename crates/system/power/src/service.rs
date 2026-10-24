//! The power **service** — the async orchestrator that pairs the pure policy
//! ([`super::policy`], [`super::battery`], [`super::inhibit`]) with the `systemd-logind`
//! / `UPower` integration ([`super::logind`], [`super::upower`]).
//!
//! Runs on a background tokio runtime (spawned by the compositor next to the notification
//! service) and:
//!
//! * on the seat that owns them, takes a logind *block* lock so logind delegates the lid
//!   switch and power key to the compositor's policy;
//! * mirrors the [`InhibitRegistry`] into a logind `idle:sleep` block lock, and the other way
//!   round holds the registry while another program (`systemd-inhibit`, a remote admin
//!   session) holds a logind `idle` or `sleep` block lock, so idle-suspend waits for it;
//! * watches the battery, runs the threshold [`BatteryMonitor`], and pushes low/critical toasts
//!   + a critical auto-suspend;
//! * reacts to lid changes per policy (suspend / lock / ignore);
//! * drains [`PowerCommand::Suspend`] requests and issues `logind.Suspend`;
//! * resets the compositor's idle timer on resume;
//! * samples the battery history at its own resolution.
//!
//! The service sleeps until something happens: a command wakes it through the bridge, an
//! inhibitor change through the registry, the lid and the battery through `UPower`'s
//! `PropertiesChanged`. Only what announces nothing runs on a deadline (`Deadlines`): logind's
//! inhibitor list while an idle timeout is armed, the next history sample, a lease's lapse, and
//! resubscribing a signal stream that ended.
//!
//! The reaction *mappings* (battery events → signals, lid → effect) are pure functions
//! tested below; `run` only wires them to the D-Bus streams.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::Notify;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::battery::{BatteryEvent, BatteryMonitor, BatterySnapshot};
use crate::bridge::{PowerBridge, PowerCommand, PowerNotification, PowerSignal};
use crate::history::{BatteryHistoryPoint, MIN_INTERVAL_MINUTES, PowerHistoryWriter};
use crate::inhibit::{INHIBIT_POWER, INHIBIT_SUSPEND, InhibitRegistry, InhibitToken};
use crate::logind::{LogindInhibitor, LogindManager};
use crate::policy::{PowerAction, PowerPolicy};
use crate::profiles::PowerProfilesMonitor;
use crate::upower::UPowerMonitor;

/// Retry D-Bus signal subscriptions slowly enough that a repeatedly failing
/// bus cannot turn the service loop into a hot spin, while still recovering
/// promptly after logind/power-profiles-daemon restarts.
const SIGNAL_STREAM_RETRY_DELAY: Duration = Duration::from_secs(5);

/// How often the service re-reads logind's inhibitor list while an idle timeout is armed. logind
/// announces no change to it, so this is a poll; an idle suspend reads it once more before acting
/// ([`PowerCommand::IdleSuspend`]).
const FOREIGN_INHIBITOR_POLL: Duration = Duration::from_secs(30);

/// Never sample the battery history more often than this.
const HISTORY_SAMPLE_FLOOR: Duration = Duration::from_secs(60);

/// When no history point is due (no battery, a failed read), look again after the history's own
/// resolution.
const HISTORY_SAMPLE_RETRY: Duration = Duration::from_secs(MIN_INTERVAL_MINUTES * 60);

fn stream_retry_after(now: Instant) -> Instant {
    after(now, SIGNAL_STREAM_RETRY_DELAY)
}

fn after(now: Instant, delay: Duration) -> Instant {
    now.checked_add(delay).unwrap_or(now)
}

/// Every moment the service must wake without being told to. Commands, inhibitor changes, the
/// lid, the battery, sleep and profile changes wake it by themselves.
#[derive(Debug, Default)]
struct Deadlines {
    /// Resubscribe to logind's `PrepareForSleep`.
    sleep_stream: Option<Instant>,
    /// Resubscribe to power-profiles-daemon's `ActiveProfile`.
    profile_stream: Option<Instant>,
    /// Resubscribe to `UPower`'s `LidIsClosed`.
    lid_stream: Option<Instant>,
    /// Resubscribe to the display device's charge.
    battery_stream: Option<Instant>,
    /// Re-read logind's inhibitor list; only while an idle timeout is armed.
    foreign_inhibitors: Option<Instant>,
    /// Sample the battery history.
    history: Option<Instant>,
    /// A registry lease stops counting by itself.
    lease_lapse: Option<Instant>,
    /// Whether `foreign_inhibitors` follows an armed idle timeout; becoming armed reads at once.
    foreign_armed: bool,
}

impl Deadlines {
    fn next(&self) -> Option<Instant> {
        [
            self.sleep_stream,
            self.profile_stream,
            self.lid_stream,
            self.battery_stream,
            self.foreign_inhibitors,
            self.history,
            self.lease_lapse,
        ]
        .into_iter()
        .flatten()
        .min()
    }
}

/// The signal streams the loop waits on; `None` while one waits for its resubscription
/// deadline.
struct Streams {
    sleep: Option<zbus::proxy::SignalStream<'static>>,
    profile: Option<zbus::proxy::PropertyStream<'static, String>>,
    lid: Option<zbus::proxy::PropertyStream<'static, bool>>,
    battery: Option<futures_util::stream::BoxStream<'static, ()>>,
}

/// Whether `slot` has come due at `now`; clears it when it has.
fn take_due(slot: &mut Option<Instant>, now: Instant) -> bool {
    if slot.is_some_and(|at| at <= now) {
        *slot = None;
        true
    } else {
        false
    }
}

/// Whether any idle timeout is armed — the only time another program's logind lock changes what
/// the compositor does.
fn idle_timeout_armed(policy: &PowerPolicy) -> bool {
    policy.idle_blank.is_some() || policy.idle_lock.is_some() || policy.idle_suspend.is_some()
}

/// How long to wait for the next history sample: until the first second a point can land
/// (`due_minute`), but never less than [`HISTORY_SAMPLE_FLOOR`].
fn history_wait(due_minute: Option<u64>, now_secs: u64) -> Duration {
    due_minute
        .map_or(HISTORY_SAMPLE_RETRY, |minute| {
            Duration::from_secs(minute.saturating_mul(60).saturating_sub(now_secs))
        })
        .max(HISTORY_SAMPLE_FLOOR)
}

/// What a lid/power-key/critical decision resolves to for the service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Suspend the machine.
    Suspend,
    /// Lock the session (a [`PowerSignal::Lock`]).
    Lock,
    /// Do nothing.
    None,
}

impl From<Option<PowerAction>> for Effect {
    fn from(a: Option<PowerAction>) -> Self {
        match a {
            Some(PowerAction::Suspend) => Effect::Suspend,
            Some(PowerAction::Lock) => Effect::Lock,
            Some(PowerAction::Ignore) | None => Effect::None,
        }
    }
}

/// Decide the effect of a lid observation given the previous state. Only an
/// open→closed edge acts; the first observation (`prev == None`) or a lid that stays closed
/// records the state. Pure — the async [`PowerService`] calls exactly this.
pub fn lid_edge_effect(prev: Option<bool>, now: bool, policy: &PowerPolicy) -> Effect {
    if now && prev == Some(false) {
        Effect::from(policy.on_lid(true))
    } else {
        Effect::None
    }
}

/// Map battery-threshold events to the compositor signals + whether to auto-suspend. Pure:
/// this is what [`PowerService::run`] applies.
pub fn battery_reaction(events: &[BatteryEvent], policy: &PowerPolicy) -> (Vec<PowerSignal>, bool) {
    let mut signals = Vec::new();
    let mut suspend = false;
    for ev in events {
        match ev {
            BatteryEvent::Low(pct) => signals.push(PowerSignal::Notify(PowerNotification {
                summary: "Battery low".into(),
                body: format!("{pct}% remaining. Consider plugging in."),
                critical: false,
            })),
            BatteryEvent::Critical(pct) => {
                let suspending = policy.critical_action == PowerAction::Suspend;
                let tail = if suspending {
                    "Suspending to protect your work."
                } else {
                    "Please plug in now."
                };
                signals.push(PowerSignal::Notify(PowerNotification {
                    summary: "Battery critically low".into(),
                    body: format!("{pct}% remaining. {tail}"),
                    critical: true,
                }));
                if suspending {
                    suspend = true;
                } else if policy.critical_action == PowerAction::Lock {
                    signals.push(PowerSignal::Lock);
                }
            }
            BatteryEvent::Recovered => { /* the toast expires on its own */ }
        }
    }
    (signals, suspend)
}

/// Map logind's `PrepareForSleep` value to the mandatory compositor action.
/// The session locks on the pre-sleep edge and only resets its idle state after
/// resume. Keeping this pure makes the security decision testable without D-Bus.
#[must_use]
pub fn sleep_reaction(going_to_sleep: bool) -> PowerSignal {
    if going_to_sleep {
        PowerSignal::PreparingForSleep
    } else {
        PowerSignal::ResumedFromSleep
    }
}

/// Who names the logind locks Tilo's own power services take. Another Tilo session on the same
/// machine (a second seat, a dev slot) mirrors its registry under these names too; counting its
/// mirror as an outside hold would let two sessions keep each other awake forever.
const OWN_INHIBITOR_WHO: [&str; 2] = ["Tilo power", "Tilo compositor"];

/// One logind `ListInhibitors` row: `(what, who, why, mode, uid, pid)`.
pub type LogindInhibitorRow = (String, String, String, String, u32, u32);

/// The registry hold that mirrors other programs' logind block locks: their reasons
/// (`who: why`) and the registry flags they map to. Pure — the service applies exactly this.
///
/// A `block` lock on `sleep` holds off idle-suspend only, so the screen still blanks and locks
/// while, say, a remote admin keeps the machine awake; a `block` lock on `idle` is a request to
/// keep the whole session awake and holds both. `delay` locks, Tilo's own locks
/// (`OWN_INHIBITOR_WHO`) and anything this process (`own_pid`) holds are not outside holds.
#[must_use]
pub fn foreign_sleep_hold(rows: &[LogindInhibitorRow], own_pid: u32) -> Option<(Vec<String>, u32)> {
    let mut reasons = Vec::new();
    let mut flags = 0;
    for (what, who, why, mode, _uid, pid) in rows {
        if mode != "block" || *pid == own_pid || OWN_INHIBITOR_WHO.contains(&who.as_str()) {
            continue;
        }
        let mut row_flags = 0;
        for item in what.split(':') {
            match item {
                "idle" => row_flags |= INHIBIT_POWER,
                "sleep" => row_flags |= INHIBIT_SUSPEND,
                _ => {}
            }
        }
        if row_flags != 0 {
            flags |= row_flags;
            reasons.push(format!("{who}: {why}"));
        }
    }
    reasons.sort();
    reasons.dedup();
    (flags != 0).then_some((reasons, flags))
}

/// The running power service. Construct with [`Self::connect`], then [`Self::run`] on a
/// tokio runtime.
pub struct PowerService {
    logind: LogindManager,
    upower: UPowerMonitor,
    power_profiles: Option<PowerProfilesMonitor>,
    bridge: PowerBridge,
    registry: InhibitRegistry,
    policy: PowerPolicy,
    battery: BatteryMonitor,
    /// Last observed lid state, for edge detection (act only on the open→closed transition, not
    /// on every poll while it stays closed).
    last_lid_closed: Option<bool>,
    /// The "delegate lid/power-key to us" block lock, held for the service's lifetime.
    _delegate_lock: Option<LogindInhibitor>,
    /// The logind `idle:sleep` lock mirroring the registry (present iff the registry currently
    /// holds any inhibitor).
    idle_sleep_lock: Option<LogindInhibitor>,
    /// The registry hold standing in for other programs' logind block locks, with the
    /// [`foreign_sleep_hold`] it was taken for.
    foreign_hold: Option<((Vec<String>, u32), InhibitToken)>,
    /// Delay inhibitor held while awake. It is released only after the
    /// compositor confirms that locked chrome has been submitted.
    sleep_delay_lock: Option<LogindInhibitor>,
    sleep_lock_drawn: bool,
    preparing_for_sleep: bool,
    suspend_task: Option<tokio::task::JoinHandle<()>>,
    /// Keeps the battery history in memory between samples.
    history: PowerHistoryWriter,
    /// How often the loop has woken; see [`Self::wakeup_counter`].
    wakeups: Arc<AtomicU64>,
}

impl PowerService {
    /// Connect to logind + `UPower` on a shared system-bus connection and take the delegate lock.
    /// Errors if the system bus / daemons are unreachable — the caller (the compositor)
    /// downgrades that to a warning and keeps running with the pure policy only.
    ///
    /// # Errors
    ///
    /// Returns an error if the system bus is unreachable, or if either the `logind` or
    /// `UPower` proxy cannot be built. Failing to take the delegate lock is not an error — it
    /// is logged and the service still starts.
    pub async fn connect(
        bridge: PowerBridge,
        registry: InhibitRegistry,
        policy: PowerPolicy,
    ) -> zbus::Result<Self> {
        let conn = zbus::Connection::system().await?;
        Self::with_connection(&conn, bridge, registry, policy).await
    }

    /// [`Self::connect`] on an existing connection to logind and `UPower`.
    ///
    /// # Errors
    ///
    /// Returns an error if either the `logind` or `UPower` proxy cannot be built.
    pub async fn with_connection(
        conn: &zbus::Connection,
        bridge: PowerBridge,
        registry: InhibitRegistry,
        policy: PowerPolicy,
    ) -> zbus::Result<Self> {
        let logind = LogindManager::with_connection(conn).await?;
        let upower = UPowerMonitor::with_connection(conn).await?;
        let power_profiles = PowerProfilesMonitor::with_connection(conn).await.ok();

        // Best-effort: if logind refuses the delegate lock we still run our policy; logind may
        // just also act on the lid. Do not fail startup.
        let seat = std::env::var("XDG_SEAT").ok();
        let delegate = if receives_seat_buttons(seat.as_deref()) {
            match logind.delegate_handled_keys().await {
                Ok(l) => {
                    info!(target: "tilo::power", "logind: delegating lid/power-key to compositor policy");
                    Some(l)
                }
                Err(e) => {
                    warn!(target: "tilo::power", error = %e, "logind: could not take delegate lock");
                    None
                }
            }
        } else {
            info!(
                target: "tilo::power",
                seat = seat.as_deref().unwrap_or_default(),
                "logind: lid/power-key stay with logind; they belong to seat0, not this seat"
            );
            None
        };
        let sleep_delay_lock = take_sleep_delay_lock(&logind).await;

        Ok(Self {
            logind,
            upower,
            power_profiles,
            bridge,
            registry,
            policy,
            battery: BatteryMonitor::new(),
            last_lid_closed: None,
            _delegate_lock: delegate,
            idle_sleep_lock: None,
            foreign_hold: None,
            sleep_delay_lock,
            sleep_lock_drawn: false,
            preparing_for_sleep: false,
            suspend_task: None,
            history: PowerHistoryWriter::new(),
            wakeups: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Counts every time the running loop wakes, for diagnostics and the idle-wakeup tests: an
    /// idle session with no armed timeout should leave it still for minutes.
    pub fn wakeup_counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.wakeups)
    }

    /// Run the service loop until the process ends. Never returns under normal operation.
    pub async fn run(mut self) {
        let policy = self.policy;
        info!(target: "tilo::power", ?policy, "power service started");

        let commands = self.bridge.command_wake();
        let registry_changed = Arc::new(Notify::new());
        self.registry.on_change({
            let changed = Arc::clone(&registry_changed);
            move || changed.notify_one()
        });
        let mut deadlines = Deadlines::default();
        let mut streams = self.start(&mut deadlines).await;

        loop {
            self.schedule_polls(&mut deadlines);
            let next_deadline = deadlines.next();
            tokio::select! {
                () = commands.notified() => self.drain_commands().await,
                () = registry_changed.notified() => self.mirror_inhibit_registry().await,
                joined = join_or_pending(&mut self.suspend_task) => {
                    self.suspend_task = None;
                    if let Err(error) = joined {
                        warn!(target: "tilo::power", %error, "logind Suspend task failed");
                    }
                }
                msg = next_or_pending(&mut streams.sleep), if streams.sleep.is_some() => {
                    if let Some(msg) = msg {
                        self.handle_prepare_for_sleep(&msg, &mut deadlines).await;
                    } else {
                        warn!(target: "tilo::power", "PrepareForSleep stream ended; scheduling resubscription");
                        streams.sleep = None;
                        deadlines.sleep_stream = Some(stream_retry_after(Instant::now()));
                    }
                }
                changed = next_or_pending(&mut streams.profile), if streams.profile.is_some() => {
                    if let Some(changed) = changed {
                        match changed.get().await {
                            Ok(profile) => self.apply_active_profile(&profile),
                            Err(error) => debug!(target: "tilo::power", %error, "active profile change could not be read"),
                        }
                    } else {
                        debug!(target: "tilo::power", "power-profile stream ended; scheduling resubscription");
                        streams.profile = None;
                        deadlines.profile_stream = Some(stream_retry_after(Instant::now()));
                    }
                }
                changed = next_or_pending(&mut streams.lid), if streams.lid.is_some() => {
                    if changed.is_some() {
                        self.poll_lid().await;
                    } else {
                        debug!(target: "tilo::power", "lid stream ended; scheduling resubscription");
                        streams.lid = None;
                        deadlines.lid_stream = Some(stream_retry_after(Instant::now()));
                    }
                }
                changed = next_or_pending(&mut streams.battery), if streams.battery.is_some() => {
                    if changed.is_some() {
                        self.poll_battery().await;
                    } else {
                        debug!(target: "tilo::power", "battery stream ended; scheduling resubscription");
                        streams.battery = None;
                        deadlines.battery_stream = Some(stream_retry_after(Instant::now()));
                    }
                }
                () = sleep_until_or_pending(next_deadline) => {
                    self.resubscribe_due(&mut streams, &mut deadlines).await;
                    self.poll_due(&mut deadlines).await;
                }
            }
            self.wakeups.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Subscribe to every signal the loop waits on and prime the state they report, so a machine
    /// that boots already-low / lid-closed reacts promptly; from then on the daemons announce
    /// their changes.
    async fn start(&mut self, deadlines: &mut Deadlines) -> Streams {
        // Resume stream. D-Bus signal streams can end when the bus daemon or
        // connection is restarted (common during a desktop/session restart),
        // so keep a bounded retry instead of leaving resume handling disabled.
        let sleep = match self.logind.receive_prepare_for_sleep().await {
            Ok(stream) => Some(stream),
            Err(error) => {
                warn!(target: "tilo::power", %error, "PrepareForSleep subscription unavailable; retrying");
                deadlines.sleep_stream = Some(stream_retry_after(Instant::now()));
                None
            }
        };
        let profile_monitor = self.power_profiles.clone();
        let profile = match profile_monitor.as_ref() {
            Some(monitor) => Some(monitor.receive_active_profile_changed().await),
            None => None,
        };
        if let Some(monitor) = profile_monitor.as_ref()
            && let Ok(profile) = monitor.active_profile().await
        {
            self.apply_active_profile(&profile);
        }

        self.sample_history(deadlines).await;
        self.poll_battery().await;
        self.poll_lid().await;
        let streams = Streams {
            sleep,
            profile,
            lid: Some(self.upower.receive_lid_changes().await),
            battery: Some(self.upower.receive_battery_changes().await),
        };
        if idle_timeout_armed(&self.policy) {
            self.poll_foreign_inhibitors().await;
            deadlines.foreign_armed = true;
        }
        self.mirror_inhibit_registry().await;
        self.drain_commands().await;
        streams
    }

    /// Resubscribe every signal stream whose retry has come due.
    async fn resubscribe_due(&mut self, streams: &mut Streams, deadlines: &mut Deadlines) {
        let now = Instant::now();
        if take_due(&mut deadlines.sleep_stream, now) {
            match self.logind.receive_prepare_for_sleep().await {
                Ok(stream) => {
                    info!(target: "tilo::power", "subscribed to logind PrepareForSleep");
                    streams.sleep = Some(stream);
                }
                Err(error) => {
                    debug!(target: "tilo::power", %error, "PrepareForSleep subscription unavailable; retrying");
                    deadlines.sleep_stream = Some(stream_retry_after(now));
                }
            }
        }
        if take_due(&mut deadlines.profile_stream, now)
            && let Some(monitor) = self.power_profiles.as_ref()
        {
            // power-profiles-daemon's zbus property stream has
            // no fallible subscription step; if its stream
            // ended, creating a new one is the retry.
            streams.profile = Some(monitor.receive_active_profile_changed().await);
            info!(target: "tilo::power", "subscribed to power-profile changes");
        }
        if take_due(&mut deadlines.lid_stream, now) {
            streams.lid = Some(self.upower.receive_lid_changes().await);
            self.poll_lid().await;
        }
        if take_due(&mut deadlines.battery_stream, now) {
            streams.battery = Some(self.upower.receive_battery_changes().await);
            self.poll_battery().await;
        }
    }

    /// Run every poll whose deadline has come due.
    async fn poll_due(&mut self, deadlines: &mut Deadlines) {
        let now = Instant::now();
        if take_due(&mut deadlines.foreign_inhibitors, now) {
            self.poll_foreign_inhibitors().await;
        }
        if take_due(&mut deadlines.lease_lapse, now) {
            self.mirror_inhibit_registry().await;
        }
        if take_due(&mut deadlines.history, now) {
            self.sample_history(deadlines).await;
        }
    }

    /// Arm the deadlines that follow from the current state: logind's inhibitor list is read
    /// only while an idle timeout is armed (and another program's hold is dropped when none is),
    /// and a lease that will lapse needs a wake when it does.
    fn schedule_polls(&mut self, deadlines: &mut Deadlines) {
        let now = Instant::now();
        if idle_timeout_armed(&self.policy) {
            // An idle timeout that was just armed reads at once: a lock taken while none was
            // armed must not wait a whole poll to count.
            let first = !std::mem::replace(&mut deadlines.foreign_armed, true);
            deadlines.foreign_inhibitors.get_or_insert_with(|| {
                if first {
                    now
                } else {
                    after(now, FOREIGN_INHIBITOR_POLL)
                }
            });
        } else {
            deadlines.foreign_armed = false;
            deadlines.foreign_inhibitors = None;
            if self.foreign_hold.take().is_some() {
                debug!(target: "tilo::power", "no idle timeout armed: stopped following other programs' locks");
            }
        }
        let wall = std::time::Instant::now();
        deadlines.lease_lapse = self
            .registry
            .next_lapse_for(INHIBIT_POWER, wall)
            .map(|lapse| after(now, lapse.saturating_duration_since(wall)));
    }

    /// Sample the power sources into the history and schedule the next sample for when a point
    /// can land.
    async fn sample_history(&mut self, deadlines: &mut Deadlines) {
        let now_secs = tilo_clock::unix_now();
        let mut due_minute: Option<u64> = None;
        match self.upower.history_samples().await {
            Ok(samples) => {
                for sample in samples {
                    let point = BatteryHistoryPoint::sampled(
                        now_secs / 60,
                        sample.capacity,
                        sample.health,
                        sample.on_ac,
                    );
                    if let Err(error) = self.history.record(sample.kind, point) {
                        debug!(target: "tilo::power", %error, "power history sample could not be stored");
                    }
                    if let Some(minute) = self.history.next_due_minute(sample.kind) {
                        due_minute = Some(due_minute.map_or(minute, |due| due.min(minute)));
                    }
                }
            }
            Err(error) => debug!(target: "tilo::power", %error, "power history sample failed"),
        }
        deadlines.history = Some(after(Instant::now(), history_wait(due_minute, now_secs)));
    }

    /// Read the battery once and apply the threshold reaction.
    async fn poll_battery(&mut self) {
        let snap = match self.upower.battery_snapshot().await {
            Ok(s) => s,
            Err(e) => {
                debug!(target: "tilo::power", error = %e, "battery read failed");
                return;
            }
        };
        self.apply_battery_snapshot(snap).await;
    }

    fn apply_active_profile(&mut self, profile: &str) {
        self.bridge.note_active_profile(profile);
        self.reapply_policy();
    }

    /// Resolve the policy for the profile and power source last seen, and
    /// hand the compositor its idle-suspend timeout when that changed.
    fn reapply_policy(&mut self) {
        let profile = self.bridge.active_profile();
        let on_battery = self.bridge.on_battery();
        let policy = crate::config::load_policy_for_profile(profile.as_deref(), on_battery);
        if policy == self.policy {
            return;
        }
        info!(target: "tilo::power", ?profile, on_battery, ?policy, "power policy applied");
        self.policy = policy;
        self.bridge
            .push_signal(PowerSignal::IdleSuspendChanged(policy.idle_suspend));
    }

    async fn apply_battery_snapshot(&mut self, snap: BatterySnapshot) {
        // Unplugged or plugged in: each source has its own suspend timer.
        if self.bridge.note_on_battery(snap.runs_on_battery()) {
            self.reapply_policy();
        }
        let events = self.battery.evaluate(&snap, &self.policy);
        if events.is_empty() {
            return;
        }
        let (signals, suspend) = battery_reaction(&events, &self.policy);
        for s in signals {
            self.bridge.push_signal(s);
        }
        if suspend {
            info!(target: "tilo::power", percent = snap.percent, "critical battery: suspending");
            self.start_suspend().await;
        }
    }

    /// Read the lid and, on the open→closed edge, apply the lid policy. Edge-triggered so we
    /// act once per close, not on every reading while it stays shut.
    async fn poll_lid(&mut self) {
        let closed = match self.upower.lid_is_closed().await {
            Ok(c) => c,
            Err(e) => {
                debug!(target: "tilo::power", error = %e, "lid read failed");
                return;
            }
        };
        let prev = self.last_lid_closed;
        self.last_lid_closed = Some(closed);
        match lid_edge_effect(prev, closed, &self.policy) {
            Effect::Suspend => {
                info!(target: "tilo::power", "lid closed: suspending");
                self.start_suspend().await;
            }
            Effect::Lock => {
                info!(target: "tilo::power", "lid closed: locking");
                self.bridge.push_signal(PowerSignal::Lock);
            }
            Effect::None => {}
        }
    }

    async fn handle_prepare_for_sleep(&mut self, msg: &zbus::Message, deadlines: &mut Deadlines) {
        // `PrepareForSleep(true)` = about to sleep, `false` = just resumed.
        if let Ok(going_to_sleep) = msg.body().deserialize::<bool>() {
            if going_to_sleep {
                info!(target: "tilo::power", "system preparing for sleep: locking session");
                self.preparing_for_sleep = true;
                self.bridge.push_signal(sleep_reaction(true));
                self.release_sleep_delay_if_ready();
            } else {
                info!(target: "tilo::power", "resumed from sleep");
                self.preparing_for_sleep = false;
                self.sleep_lock_drawn = false;
                self.sleep_delay_lock = take_sleep_delay_lock(&self.logind).await;
                self.bridge.push_signal(sleep_reaction(false));
                // The loop's deadlines run on the monotonic clock, which stood still while the
                // machine slept: what came due during sleep is due now.
                let now = Instant::now();
                deadlines.history = Some(now);
                if deadlines.foreign_inhibitors.is_some() {
                    deadlines.foreign_inhibitors = Some(now);
                }
            }
        }
    }

    /// Drain compositor-issued suspend requests.
    async fn drain_commands(&mut self) {
        for cmd in self.bridge.drain_commands() {
            match cmd {
                PowerCommand::Suspend => self.start_suspend().await,
                PowerCommand::IdleSuspend => self.start_idle_suspend().await,
                PowerCommand::LockScreenDrawn => {
                    self.sleep_lock_drawn = true;
                    self.release_sleep_delay_if_ready();
                }
                PowerCommand::UpdatePolicy(policy) => self.policy = policy,
            }
        }
    }

    /// Take/release the logind `idle:sleep` block lock to match the Tilo inhibitor registry, so
    /// an app holding a Tilo inhibitor also blocks logind's own idle/suspend handling.
    async fn mirror_inhibit_registry(&mut self) {
        let inhibited = self.registry.is_inhibited();
        match (inhibited, self.idle_sleep_lock.is_some()) {
            (true, false) => {
                let why = self.registry.active_reasons().join(", ");
                match self
                    .logind
                    .inhibit("idle:sleep", "Tilo power", &why, "block")
                    .await
                {
                    Ok(l) => {
                        debug!(target: "tilo::power", %why, "took logind idle:sleep lock");
                        self.idle_sleep_lock = Some(l);
                    }
                    Err(e) => warn!(target: "tilo::power", error = %e, "idle:sleep inhibit failed"),
                }
            }
            (false, true) => {
                debug!(target: "tilo::power", "releasing logind idle:sleep lock");
                self.idle_sleep_lock = None; // drop closes the fd
            }
            _ => {}
        }
    }

    /// Hold the registry while another program holds a logind `idle` or `sleep` block lock, so
    /// the compositor's idle-suspend waits for it exactly as it waits for media or a running task.
    async fn poll_foreign_inhibitors(&mut self) {
        let rows = match self.logind.list_inhibitors().await {
            Ok(rows) => rows,
            Err(error) => {
                debug!(target: "tilo::power", %error, "ListInhibitors failed");
                return;
            }
        };
        let wanted = foreign_sleep_hold(&rows, std::process::id());
        if wanted.as_ref() == self.foreign_hold.as_ref().map(|(held, _)| held) {
            return;
        }
        self.foreign_hold = wanted.map(|(reasons, flags)| {
            info!(target: "tilo::power", ?reasons, "kept awake by another program");
            let token = self
                .registry
                .acquire_for(format!("kept awake by {}", reasons.join(", ")), flags);
            ((reasons, flags), token)
        });
        if self.foreign_hold.is_none() {
            info!(target: "tilo::power", "no other program keeps the machine awake any more");
        }
    }

    /// Suspend because the session went idle, unless another program took a logind lock since
    /// the last [`FOREIGN_INHIBITOR_POLL`]: then report [`PowerSignal::SuspendFailed`], which
    /// un-latches the compositor's idle suspend until the registry is clear again.
    async fn start_idle_suspend(&mut self) {
        self.poll_foreign_inhibitors().await;
        if self.registry.is_inhibited() {
            info!(
                target: "tilo::power",
                reasons = ?self.registry.active_reasons(),
                "idle suspend withdrawn: the machine is held awake"
            );
            self.bridge.push_signal(PowerSignal::SuspendFailed);
            return;
        }
        self.start_suspend().await;
    }

    /// Issue the suspend request, if logind allows it. The inhibitor registry is the compositor's
    /// gate for *idle* suspend; lid/critical suspends are deliberate and proceed regardless.
    async fn start_suspend(&mut self) {
        if self.suspend_task.is_some() {
            return;
        }
        // A delay inhibitor is the safety barrier that gives the compositor
        // time to draw its lock scene. Never initiate a compositor-requested
        // suspend without it: otherwise a slow frame can leave app content
        // visible during the sleep transition. Retry acquisition here as the
        // bus may have recovered since startup/resume.
        if self.sleep_delay_lock.is_none() {
            self.sleep_delay_lock = take_sleep_delay_lock(&self.logind).await;
            if self.sleep_delay_lock.is_none() {
                warn!(target: "tilo::power", "cannot suspend without a sleep-delay inhibitor");
                self.bridge.push_signal(PowerSignal::SuspendFailed);
                return;
            }
        }
        match self.logind.can_suspend().await {
            Ok(v) if v == "yes" => {
                // Ask the compositor to lock before invoking logind. The delay
                // inhibitor remains held until the drawn-frame acknowledgement,
                // including for externally initiated sleep.
                self.sleep_lock_drawn = false;
                self.bridge.push_signal(PowerSignal::PreparingForSleep);
                let logind = self.logind.clone();
                let bridge = self.bridge.clone();
                self.suspend_task = Some(tokio::spawn(async move {
                    if let Err(e) = logind.suspend(false).await {
                        warn!(target: "tilo::power", error = %e, "logind Suspend failed");
                        bridge.push_signal(PowerSignal::SuspendFailed);
                    }
                }));
            }
            Ok(v) => {
                warn!(target: "tilo::power", verdict = %v, "logind says cannot suspend");
                self.bridge.push_signal(PowerSignal::SuspendFailed);
            }
            Err(e) => {
                warn!(target: "tilo::power", error = %e, "CanSuspend failed");
                self.bridge.push_signal(PowerSignal::SuspendFailed);
            }
        }
    }

    fn release_sleep_delay_if_ready(&mut self) {
        if self.preparing_for_sleep && self.sleep_lock_drawn {
            debug!(target: "tilo::power", "lock frame drawn; releasing sleep delay inhibitor");
            self.sleep_delay_lock = None;
        }
    }
}

/// Whether a session on `seat` gets the lid switch and power key. logind tags those
/// buttons `seat0` and keeps them there; a session on any other seat (a second
/// head, a dev-VM slot) never sees them, so delegating them to it would leave the
/// power button, and an ACPI shutdown request, doing nothing at all.
fn receives_seat_buttons(seat: Option<&str>) -> bool {
    seat.is_none_or(|seat| seat.is_empty() || seat == "seat0")
}

async fn take_sleep_delay_lock(logind: &LogindManager) -> Option<LogindInhibitor> {
    match logind
        .inhibit(
            "sleep",
            "Tilo compositor",
            "Draw lock screen before sleep",
            "delay",
        )
        .await
    {
        Ok(lock) => Some(lock),
        Err(error) => {
            warn!(target: "tilo::power", %error, "sleep delay inhibitor unavailable");
            None
        }
    }
}

/// `select!` helper: wait for the running suspend task, or park forever when there is none.
async fn join_or_pending(
    task: &mut Option<tokio::task::JoinHandle<()>>,
) -> Result<(), tokio::task::JoinError> {
    match task {
        Some(task) => task.await,
        None => std::future::pending().await,
    }
}

/// `select!` helper: sleep until `deadline`, or park forever when there is none.
async fn sleep_until_or_pending(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// `select!` helper: poll an `Option<Stream>`, or park forever when absent so the branch is
/// simply inert rather than a busy `None`.
async fn next_or_pending<S: futures_util::Stream + Unpin>(
    stream: &mut Option<S>,
) -> Option<S::Item> {
    match stream {
        Some(s) => s.next().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_reacts_to_lid_sleep_and_signal_edges() {
        {
            let now = Instant::now();
            let retry = stream_retry_after(now);
            assert!(retry > now);
            assert_eq!(retry.duration_since(now), SIGNAL_STREAM_RETRY_DELAY);
        }
        {
            assert!(receives_seat_buttons(Some("seat0")));
            assert!(receives_seat_buttons(None));
            assert!(receives_seat_buttons(Some("")));
            assert!(!receives_seat_buttons(Some("seat-pr1307")));
            assert!(!receives_seat_buttons(Some("seat1")));
        }
        {
            let row = |what: &str, who: &str, mode: &str, pid: u32| -> LogindInhibitorRow {
                (what.into(), who.into(), "why".into(), mode.into(), 0, pid)
            };
            let own = 100;
            // Nothing held, Tilo's own locks, delay locks and this process's locks are no hold.
            assert_eq!(foreign_sleep_hold(&[], own), None);
            assert_eq!(
                foreign_sleep_hold(
                    &[
                        row("idle:sleep", "Tilo power", "block", 7),
                        row("sleep", "Tilo compositor", "delay", own),
                        row("sleep", "NetworkManager", "delay", 3),
                        row("handle-lid-switch", "Tilo compositor", "block", own),
                        row("sleep", "anything", "block", own),
                        row("shutdown", "packagekit", "block", 9),
                    ],
                    own,
                ),
                None
            );
            // A remote admin's `sleep` lock holds suspend only: the screen still blanks and locks.
            assert_eq!(
                foreign_sleep_hold(&[row("sleep", "tilo-vm", "block", 5)], own),
                Some((vec!["tilo-vm: why".to_string()], INHIBIT_SUSPEND))
            );
            // An `idle` lock keeps the whole session awake.
            assert_eq!(
                foreign_sleep_hold(
                    &[
                        row("idle", "mpv", "block", 6),
                        row("sleep", "tilo-vm", "block", 5),
                    ],
                    own,
                ),
                Some((
                    vec!["mpv: why".to_string(), "tilo-vm: why".to_string()],
                    INHIBIT_POWER
                ))
            );
        }
        {
            let mut p = PowerPolicy {
                lid_close_action: PowerAction::Suspend,
                ..Default::default()
            };
            // First observation never acts (records only).
            assert_eq!(lid_edge_effect(None, true, &p), Effect::None);
            assert_eq!(lid_edge_effect(None, false, &p), Effect::None);
            // open→closed acts.
            assert_eq!(lid_edge_effect(Some(false), true, &p), Effect::Suspend);
            // staying closed does not re-act.
            assert_eq!(lid_edge_effect(Some(true), true, &p), Effect::None);
            // closing→opening does not act.
            assert_eq!(lid_edge_effect(Some(true), false, &p), Effect::None);
            p.lid_close_action = PowerAction::Lock;
            assert_eq!(lid_edge_effect(Some(false), true, &p), Effect::Lock);
            p.lid_close_action = PowerAction::Ignore;
            assert_eq!(lid_edge_effect(Some(false), true, &p), Effect::None);
        }
        {
            assert_eq!(Effect::from(Some(PowerAction::Suspend)), Effect::Suspend);
            assert_eq!(Effect::from(Some(PowerAction::Lock)), Effect::Lock);
            assert_eq!(Effect::from(Some(PowerAction::Ignore)), Effect::None);
            assert_eq!(Effect::from(None), Effect::None);
        }
        {
            assert_eq!(sleep_reaction(true), PowerSignal::PreparingForSleep);
            assert_eq!(sleep_reaction(false), PowerSignal::ResumedFromSleep);
        }
        {
            let bridge = crate::bridge::PowerBridge::new();
            bridge.push_signal(PowerSignal::SuspendFailed);
            assert_eq!(bridge.drain_signals(), vec![PowerSignal::SuspendFailed]);
        }
    }

    #[test]
    fn deadlines_follow_the_policy_and_the_history() {
        {
            let now = Instant::now();
            let mut deadlines = Deadlines::default();
            assert_eq!(deadlines.next(), None, "nothing armed: the loop only waits");
            deadlines.history = Some(now + HISTORY_SAMPLE_RETRY);
            deadlines.foreign_inhibitors = Some(now + FOREIGN_INHIBITOR_POLL);
            assert_eq!(deadlines.next(), Some(now + FOREIGN_INHIBITOR_POLL));
            assert!(!take_due(&mut deadlines.foreign_inhibitors, now));
            assert!(take_due(
                &mut deadlines.foreign_inhibitors,
                now + FOREIGN_INHIBITOR_POLL
            ));
            assert_eq!(
                deadlines.foreign_inhibitors, None,
                "a due deadline is spent"
            );
            assert_eq!(deadlines.next(), Some(now + HISTORY_SAMPLE_RETRY));
        }
        {
            let never = PowerPolicy {
                idle_blank: None,
                idle_lock: None,
                idle_suspend: None,
                ..PowerPolicy::default()
            };
            assert!(!idle_timeout_armed(&never));
            for armed in [
                PowerPolicy {
                    idle_blank: Some(Duration::from_secs(60)),
                    ..never
                },
                PowerPolicy {
                    idle_lock: Some(Duration::from_secs(60)),
                    ..never
                },
                PowerPolicy {
                    idle_suspend: Some(Duration::from_secs(60)),
                    ..never
                },
            ] {
                assert!(idle_timeout_armed(&armed));
            }
        }
        {
            let now_secs = 1_000 * 60 + 30;
            assert_eq!(
                history_wait(Some(1_015), now_secs),
                Duration::from_secs(14 * 60 + 30),
                "wakes at the first second the next point lands"
            );
            assert_eq!(
                history_wait(Some(1_000), now_secs),
                HISTORY_SAMPLE_FLOOR,
                "an overdue point still waits the floor"
            );
            assert_eq!(history_wait(None, now_secs), HISTORY_SAMPLE_RETRY);
            assert!(HISTORY_SAMPLE_RETRY >= HISTORY_SAMPLE_FLOOR);
        }
    }

    #[test]
    fn battery_events_toast_and_suspend_according_to_policy() {
        {
            let p = PowerPolicy::default();
            let (signals, suspend) = battery_reaction(&[BatteryEvent::Low(12)], &p);
            assert!(!suspend);
            assert_eq!(signals.len(), 1);
            match &signals[0] {
                PowerSignal::Notify(n) => {
                    assert!(!n.critical);
                    assert!(n.body.contains("12%"));
                }
                other => panic!("expected a notify, got {other:?}"),
            }
        }
        {
            let p = PowerPolicy {
                critical_action: PowerAction::Suspend,
                ..Default::default()
            };
            let (signals, suspend) = battery_reaction(&[BatteryEvent::Critical(4)], &p);
            assert!(suspend, "critical + suspend policy ⇒ auto-suspend");
            assert!(matches!(signals[0], PowerSignal::Notify(ref n) if n.critical));
        }
        {
            let p = PowerPolicy {
                critical_action: PowerAction::Ignore,
                ..Default::default()
            };
            let (signals, suspend) = battery_reaction(&[BatteryEvent::Critical(3)], &p);
            assert!(!suspend, "ignore policy ⇒ warn only, no suspend");
            // Still warns (critical toast), just doesn't act.
            assert_eq!(signals.len(), 1);
            assert!(matches!(signals[0], PowerSignal::Notify(ref n) if n.critical));
        }
        {
            let p = PowerPolicy::default();
            let (signals, suspend) = battery_reaction(&[BatteryEvent::Recovered], &p);
            assert!(signals.is_empty());
            assert!(!suspend);
        }
        // Phase `monitor_plus_reaction_full_discharge`.
        // End-to-end pure path: a discharging battery crossing the low then the critical threshold
        // produces exactly the toasts + suspend the service would apply — the same `BatteryMonitor`
        // + `battery_reaction` the async loop uses, minus the bus.
        {
            use crate::battery::BatteryState;
            let p = PowerPolicy {
                low_battery_percent: 15,
                critical_battery_percent: 5,
                ..Default::default()
            };
            let mut mon = BatteryMonitor::new();
            let disch = |pct| BatterySnapshot {
                present: true,
                percent: pct,
                state: BatteryState::Discharging,
            };

            // Healthy: nothing.
            let (s, _) = battery_reaction(&mon.evaluate(&disch(40), &p), &p);
            assert!(s.is_empty());
            // Low: one non-critical toast.
            let (s, susp) = battery_reaction(&mon.evaluate(&disch(14), &p), &p);
            assert_eq!(s.len(), 1);
            assert!(!susp);
            // Critical: critical toast + suspend.
            let (s, susp) = battery_reaction(&mon.evaluate(&disch(4), &p), &p);
            assert!(susp);
            assert!(matches!(s[0], PowerSignal::Notify(ref n) if n.critical));
            // Deeper: silent (latched).
            let (s, susp) = battery_reaction(&mon.evaluate(&disch(2), &p), &p);
            assert!(s.is_empty() && !susp);
        }
    }
}

#[cfg(test)]
mod fake_bus_tests;
