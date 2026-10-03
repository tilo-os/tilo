//! The power **service** — the async orchestrator that pairs the pure policy
//! ([`super::policy`], [`super::battery`], [`super::inhibit`]) with the `systemd-logind`
//! / `UPower` integration ([`super::logind`], [`super::upower`]).
//!
//! Runs on a background tokio runtime (spawned by the compositor next to the notification
//! service) and:
//!
//! * on the seat that owns them, takes a logind *block* lock so logind delegates the lid
//!   switch and power key to the compositor's policy;
//! * mirrors the [`InhibitRegistry`] into a logind `idle:sleep` block lock;
//! * polls the battery, runs the threshold [`BatteryMonitor`], and pushes low/critical toasts
//!   + a critical auto-suspend;
//! * reacts to lid changes per policy (suspend / lock / ignore);
//! * drains [`PowerCommand::Suspend`] requests and issues `logind.Suspend`;
//! * resets the compositor's idle timer on resume.
//!
//! The reaction *mappings* (battery events → signals, lid → effect) are pure functions
//! tested below; `run` only wires them to the D-Bus streams.

use futures_util::StreamExt;
use tracing::{debug, info, warn};

use crate::battery::{BatteryEvent, BatteryMonitor, BatterySnapshot};
use crate::bridge::{PowerBridge, PowerCommand, PowerNotification, PowerSignal};
use crate::inhibit::InhibitRegistry;
use crate::logind::{LogindInhibitor, LogindManager};
use crate::policy::{PowerAction, PowerPolicy};
use crate::profiles::PowerProfilesMonitor;
use crate::upower::UPowerMonitor;

/// Retry D-Bus signal subscriptions slowly enough that a repeatedly failing
/// bus cannot turn the service loop into a hot spin, while still recovering
/// promptly after logind/power-profiles-daemon restarts.
const SIGNAL_STREAM_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

fn stream_retry_after(now: tokio::time::Instant) -> tokio::time::Instant {
    now.checked_add(SIGNAL_STREAM_RETRY_DELAY).unwrap_or(now)
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
    /// Delay inhibitor held while awake. It is released only after the
    /// compositor confirms that locked chrome has been submitted.
    sleep_delay_lock: Option<LogindInhibitor>,
    sleep_lock_drawn: bool,
    preparing_for_sleep: bool,
    suspend_task: Option<tokio::task::JoinHandle<()>>,
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
        let logind = LogindManager::with_connection(&conn).await?;
        let upower = UPowerMonitor::with_connection(&conn).await?;
        let power_profiles = PowerProfilesMonitor::with_connection(&conn).await.ok();

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
            sleep_delay_lock,
            sleep_lock_drawn: false,
            preparing_for_sleep: false,
            suspend_task: None,
        })
    }

    /// Run the service loop until the process ends. Never returns under normal operation.
    pub async fn run(mut self) {
        // Prompt cadence for commands + registry mirroring + lid polling; battery is polled less
        // often (every `BATTERY_POLL_EVERY` ticks).
        const BATTERY_POLL_EVERY: u32 = 80; // 250ms * 80 = 20s

        let policy = self.policy;
        info!(target: "tilo::power", ?policy, "power service started");

        let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
        let mut ticks: u32 = 0;

        // Resume stream. D-Bus signal streams can end when the bus daemon or
        // connection is restarted (common during a desktop/session restart),
        // so keep a bounded retry instead of leaving resume handling disabled.
        let mut sleep_stream = match self.logind.receive_prepare_for_sleep().await {
            Ok(stream) => Some(stream),
            Err(error) => {
                warn!(target: "tilo::power", %error, "PrepareForSleep subscription unavailable; retrying");
                None
            }
        };
        let mut sleep_stream_retry_at = tokio::time::Instant::now();
        let profile_monitor = self.power_profiles.clone();
        let mut profile_stream = match profile_monitor.as_ref() {
            Some(monitor) => Some(monitor.receive_active_profile_changed().await),
            None => None,
        };
        let mut profile_stream_retry_at = tokio::time::Instant::now();

        if let Some(monitor) = profile_monitor.as_ref()
            && let Ok(profile) = monitor.active_profile().await
        {
            self.apply_active_profile(&profile);
        }

        // Prime battery + lid so a machine that boots already-low / lid-closed reacts promptly.
        self.poll_battery().await;
        self.poll_lid().await;

        loop {
            tokio::select! {
                _ = tick.tick() => {
                    let now = tokio::time::Instant::now();
                    if sleep_stream.is_none() && now >= sleep_stream_retry_at {
                        match self.logind.receive_prepare_for_sleep().await {
                            Ok(stream) => {
                                info!(target: "tilo::power", "subscribed to logind PrepareForSleep");
                                sleep_stream = Some(stream);
                            }
                            Err(error) => {
                                debug!(target: "tilo::power", %error, "PrepareForSleep subscription unavailable; retrying");
                                sleep_stream_retry_at = stream_retry_after(now);
                            }
                        }
                    }
                    if profile_stream.is_none()
                        && now >= profile_stream_retry_at
                        && let Some(monitor) = profile_monitor.as_ref()
                    {
                        // power-profiles-daemon's zbus property stream has
                        // no fallible subscription step; if its stream
                        // ended, creating a new one is the retry.
                        profile_stream = Some(monitor.receive_active_profile_changed().await);
                        info!(target: "tilo::power", "subscribed to power-profile changes");
                    }
                    self.reap_suspend_task().await;
                    self.drain_commands().await;
                    self.mirror_inhibit_registry().await;
                    self.poll_lid().await;
                    ticks = ticks.wrapping_add(1);
                    if ticks.is_multiple_of(BATTERY_POLL_EVERY) {
                        self.poll_battery().await;
                    }
                }
                msg = next_or_pending(&mut sleep_stream), if sleep_stream.is_some() => {
                    if let Some(msg) = msg {
                        self.handle_prepare_for_sleep(&msg).await;
                    } else {
                        warn!(target: "tilo::power", "PrepareForSleep stream ended; scheduling resubscription");
                        sleep_stream = None;
                        sleep_stream_retry_at = stream_retry_after(tokio::time::Instant::now());
                    }
                }
                changed = next_or_pending(&mut profile_stream), if profile_stream.is_some() => {
                    if let Some(changed) = changed {
                        match changed.get().await {
                            Ok(profile) => self.apply_active_profile(&profile),
                            Err(error) => debug!(target: "tilo::power", %error, "active profile change could not be read"),
                        }
                    } else {
                        debug!(target: "tilo::power", "power-profile stream ended; scheduling resubscription");
                        profile_stream = None;
                        profile_stream_retry_at = stream_retry_after(tokio::time::Instant::now());
                    }
                }
            }
        }
    }

    /// Poll the battery once and apply the threshold reaction.
    async fn poll_battery(&mut self) {
        match self.upower.history_samples().await {
            Ok(samples) => {
                for sample in samples {
                    if let Err(error) = crate::history::record_power_history(
                        sample.kind,
                        sample.capacity,
                        sample.health,
                        sample.on_ac,
                    ) {
                        debug!(target: "tilo::power", %error, "power history sample could not be stored");
                    }
                }
            }
            Err(error) => debug!(target: "tilo::power", %error, "power history sample failed"),
        }
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
        let policy = crate::config::load_policy_for_profile(Some(profile));
        if policy == self.policy {
            return;
        }
        info!(target: "tilo::power", %profile, ?policy, "active power profile policy applied");
        self.policy = policy;
        self.bridge
            .push_signal(PowerSignal::IdleSuspendChanged(policy.idle_suspend));
    }

    async fn apply_battery_snapshot(&mut self, snap: BatterySnapshot) {
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

    /// Poll the lid and, on the open→closed edge, apply the lid policy. Edge-triggered so we
    /// act once per close, not every tick it stays shut.
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

    async fn handle_prepare_for_sleep(&mut self, msg: &zbus::Message) {
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
            }
        }
    }

    /// Drain compositor-issued suspend requests.
    async fn drain_commands(&mut self) {
        for cmd in self.bridge.drain_commands() {
            match cmd {
                PowerCommand::Suspend => self.start_suspend().await,
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

    async fn reap_suspend_task(&mut self) {
        if self
            .suspend_task
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
            && let Some(task) = self.suspend_task.take()
            && let Err(error) = task.await
        {
            warn!(target: "tilo::power", %error, "logind Suspend task failed");
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
        // Phase `ended_signal_streams_back_off_before_resubscribe`.
        {
            let now = tokio::time::Instant::now();
            let retry = stream_retry_after(now);
            assert!(retry > now);
            assert_eq!(retry.duration_since(now), SIGNAL_STREAM_RETRY_DELAY);
        }
        // Phase `only_the_button_seat_takes_the_key_delegation`.
        {
            assert!(receives_seat_buttons(Some("seat0")));
            assert!(receives_seat_buttons(None));
            assert!(receives_seat_buttons(Some("")));
            assert!(!receives_seat_buttons(Some("seat-pr1307")));
            assert!(!receives_seat_buttons(Some("seat1")));
        }
        // Phase `lid_edge_only_acts_on_close_transition`.
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
            // Policy is honoured on the edge.
            p.lid_close_action = PowerAction::Lock;
            assert_eq!(lid_edge_effect(Some(false), true, &p), Effect::Lock);
            p.lid_close_action = PowerAction::Ignore;
            assert_eq!(lid_edge_effect(Some(false), true, &p), Effect::None);
        }
        // Phase `effect_from_action`.
        {
            assert_eq!(Effect::from(Some(PowerAction::Suspend)), Effect::Suspend);
            assert_eq!(Effect::from(Some(PowerAction::Lock)), Effect::Lock);
            assert_eq!(Effect::from(Some(PowerAction::Ignore)), Effect::None);
            assert_eq!(Effect::from(None), Effect::None);
        }
        // Phase `sleep_always_locks_before_resume_is_reported`.
        {
            assert_eq!(sleep_reaction(true), PowerSignal::PreparingForSleep);
            assert_eq!(sleep_reaction(false), PowerSignal::ResumedFromSleep);
        }
        // Phase `suspend_failure_is_reported_to_cancel_a_pre_sleep_latch`.
        {
            let bridge = crate::bridge::PowerBridge::new();
            bridge.push_signal(PowerSignal::SuspendFailed);
            assert_eq!(bridge.drain_signals(), vec![PowerSignal::SuspendFailed]);
        }
    }

    #[test]
    fn battery_events_toast_and_suspend_according_to_policy() {
        // Phase `low_event_becomes_a_noncritical_toast_no_suspend`.
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
        // Phase `critical_event_toasts_and_suspends_when_policy_says_suspend`.
        {
            let p = PowerPolicy {
                critical_action: PowerAction::Suspend,
                ..Default::default()
            };
            let (signals, suspend) = battery_reaction(&[BatteryEvent::Critical(4)], &p);
            assert!(suspend, "critical + suspend policy ⇒ auto-suspend");
            assert!(matches!(signals[0], PowerSignal::Notify(ref n) if n.critical));
        }
        // Phase `critical_event_does_not_suspend_when_policy_ignores`.
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
        // Phase `recovered_event_is_silent`.
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
