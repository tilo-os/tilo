//! The power **service** — the async orchestrator that pairs the pure policy
//! ([`super::policy`], [`super::battery`], [`super::inhibit`]) with the real `systemd-logind`
//! / `UPower` glue ([`super::logind`], [`super::upower`]).
//!
//! Runs on a background tokio runtime (spawned by the compositor next to the notification
//! service) and:
//!
//! * takes a logind *block* lock so logind delegates the lid switch and power key to the
//!   compositor's policy;
//! * mirrors the [`InhibitRegistry`] into a logind `idle:sleep` block lock;
//! * polls the battery, runs the threshold [`BatteryMonitor`], and pushes low/critical toasts
//!   + a critical auto-suspend;
//! * reacts to lid changes per policy (suspend / lock / ignore);
//! * drains [`PowerCommand::Suspend`] requests and issues `logind.Suspend`;
//! * resets the compositor's idle timer on resume.
//!
//! The reaction *mappings* (battery events → signals, lid → effect) are pure functions
//! tested below; `run` only wires them to real streams.

use futures_util::StreamExt;
use tracing::{debug, info, warn};

use crate::battery::{BatteryEvent, BatteryMonitor, BatterySnapshot};
use crate::bridge::{PowerBridge, PowerCommand, PowerNotification, PowerSignal};
use crate::inhibit::InhibitRegistry;
use crate::logind::{LogindInhibitor, LogindManager};
use crate::policy::{PowerAction, PowerPolicy};
use crate::profiles::PowerProfilesMonitor;
use crate::upower::UPowerMonitor;

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

/// Decide the effect of a lid observation given the previous state. Only a genuine
/// open→closed edge acts; the first observation (`prev == None`) or a lid that stays closed
/// just records. Pure — the async [`PowerService`] calls exactly this.
pub fn lid_edge_effect(prev: Option<bool>, now: bool, policy: &PowerPolicy) -> Effect {
    if now && prev == Some(false) {
        Effect::from(policy.on_lid(true))
    } else {
        Effect::None
    }
}

/// Map battery-threshold events to the compositor signals + whether to auto-suspend. Pure:
/// this is what [`PowerService::run`] applies for real.
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
        PowerSignal::Lock
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
        let delegate = match logind.delegate_handled_keys().await {
            Ok(l) => {
                info!(target: "qos::power", "logind: delegating lid/power-key to compositor policy");
                Some(l)
            }
            Err(e) => {
                warn!(target: "qos::power", error = %e, "logind: could not take delegate lock");
                None
            }
        };

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
        })
    }

    /// Run the service loop until the process ends. Never returns under normal operation.
    pub async fn run(mut self) {
        // Prompt cadence for commands + registry mirroring + lid polling; battery is polled less
        // often (every `BATTERY_POLL_EVERY` ticks).
        const BATTERY_POLL_EVERY: u32 = 80; // 250ms * 80 = 20s

        let policy = self.policy;
        info!(target: "qos::power", ?policy, "power service started");

        let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
        let mut ticks: u32 = 0;

        // Resume stream (best-effort; a failed subscribe just disables the resume timer reset).
        let mut sleep_stream = self.logind.receive_prepare_for_sleep().await.ok();
        let profile_monitor = self.power_profiles.clone();
        let mut profile_stream = match profile_monitor.as_ref() {
            Some(monitor) => Some(monitor.receive_active_profile_changed().await),
            None => None,
        };

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
                    self.drain_commands().await;
                    self.mirror_inhibit_registry().await;
                    self.poll_lid().await;
                    ticks = ticks.wrapping_add(1);
                    if ticks.is_multiple_of(BATTERY_POLL_EVERY) {
                        self.poll_battery().await;
                    }
                }
                Some(msg) = next_or_pending(&mut sleep_stream) => {
                    self.handle_prepare_for_sleep(&msg);
                }
                Some(changed) = next_or_pending(&mut profile_stream) => {
                    match changed.get().await {
                        Ok(profile) => self.apply_active_profile(&profile),
                        Err(error) => debug!(target: "qos::power", %error, "active profile change could not be read"),
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
                        debug!(target: "qos::power", %error, "power history sample could not be stored");
                    }
                }
            }
            Err(error) => debug!(target: "qos::power", %error, "power history sample failed"),
        }
        let snap = match self.upower.battery_snapshot().await {
            Ok(s) => s,
            Err(e) => {
                debug!(target: "qos::power", error = %e, "battery read failed");
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
        info!(target: "qos::power", %profile, ?policy, "active power profile policy applied");
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
            info!(target: "qos::power", percent = snap.percent, "critical battery: suspending");
            self.do_suspend().await;
        }
    }

    /// Poll the lid and, on the open→closed edge, apply the lid policy. Edge-triggered so we
    /// act once per close, not every tick it stays shut.
    async fn poll_lid(&mut self) {
        let closed = match self.upower.lid_is_closed().await {
            Ok(c) => c,
            Err(e) => {
                debug!(target: "qos::power", error = %e, "lid read failed");
                return;
            }
        };
        let prev = self.last_lid_closed;
        self.last_lid_closed = Some(closed);
        match lid_edge_effect(prev, closed, &self.policy) {
            Effect::Suspend => {
                info!(target: "qos::power", "lid closed: suspending");
                self.do_suspend().await;
            }
            Effect::Lock => {
                info!(target: "qos::power", "lid closed: locking");
                self.bridge.push_signal(PowerSignal::Lock);
            }
            Effect::None => {}
        }
    }

    fn handle_prepare_for_sleep(&self, msg: &zbus::Message) {
        // `PrepareForSleep(true)` = about to sleep, `false` = just resumed.
        if let Ok(going_to_sleep) = msg.body().deserialize::<bool>() {
            if going_to_sleep {
                info!(target: "qos::power", "system preparing for sleep: locking session");
            } else {
                info!(target: "qos::power", "resumed from sleep");
            }
            self.bridge.push_signal(sleep_reaction(going_to_sleep));
        }
    }

    /// Drain compositor-issued suspend requests.
    async fn drain_commands(&mut self) {
        for cmd in self.bridge.drain_commands() {
            match cmd {
                PowerCommand::Suspend => self.do_suspend().await,
                PowerCommand::UpdatePolicy(policy) => self.policy = policy,
            }
        }
    }

    /// Take/release the logind `idle:sleep` block lock to match the qOS inhibitor registry, so
    /// an app holding a qOS inhibitor also blocks logind's own idle/suspend handling.
    async fn mirror_inhibit_registry(&mut self) {
        let inhibited = self.registry.is_inhibited();
        match (inhibited, self.idle_sleep_lock.is_some()) {
            (true, false) => {
                let why = self.registry.active_reasons().join(", ");
                match self
                    .logind
                    .inhibit("idle:sleep", "qOS power", &why, "block")
                    .await
                {
                    Ok(l) => {
                        debug!(target: "qos::power", %why, "took logind idle:sleep lock");
                        self.idle_sleep_lock = Some(l);
                    }
                    Err(e) => warn!(target: "qos::power", error = %e, "idle:sleep inhibit failed"),
                }
            }
            (false, true) => {
                debug!(target: "qos::power", "releasing logind idle:sleep lock");
                self.idle_sleep_lock = None; // drop closes the fd
            }
            _ => {}
        }
    }

    /// Issue the real suspend, if logind says it can. The inhibitor registry is the compositor's
    /// gate for *idle* suspend; lid/critical suspends are deliberate and proceed regardless.
    async fn do_suspend(&self) {
        match self.logind.can_suspend().await {
            Ok(v) if v == "yes" => {
                if let Err(e) = self.logind.suspend(false).await {
                    warn!(target: "qos::power", error = %e, "logind Suspend failed");
                }
            }
            Ok(v) => warn!(target: "qos::power", verdict = %v, "logind says cannot suspend"),
            Err(e) => warn!(target: "qos::power", error = %e, "CanSuspend failed"),
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
    fn lid_edge_only_acts_on_close_transition() {
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

    #[test]
    fn effect_from_action() {
        assert_eq!(Effect::from(Some(PowerAction::Suspend)), Effect::Suspend);
        assert_eq!(Effect::from(Some(PowerAction::Lock)), Effect::Lock);
        assert_eq!(Effect::from(Some(PowerAction::Ignore)), Effect::None);
        assert_eq!(Effect::from(None), Effect::None);
    }

    #[test]
    fn sleep_always_locks_before_resume_is_reported() {
        assert_eq!(sleep_reaction(true), PowerSignal::Lock);
        assert_eq!(sleep_reaction(false), PowerSignal::ResumedFromSleep);
    }

    #[test]
    fn low_event_becomes_a_noncritical_toast_no_suspend() {
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

    #[test]
    fn critical_event_toasts_and_suspends_when_policy_says_suspend() {
        let p = PowerPolicy {
            critical_action: PowerAction::Suspend,
            ..Default::default()
        };
        let (signals, suspend) = battery_reaction(&[BatteryEvent::Critical(4)], &p);
        assert!(suspend, "critical + suspend policy ⇒ auto-suspend");
        assert!(matches!(signals[0], PowerSignal::Notify(ref n) if n.critical));
    }

    #[test]
    fn critical_event_does_not_suspend_when_policy_ignores() {
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

    #[test]
    fn recovered_event_is_silent() {
        let p = PowerPolicy::default();
        let (signals, suspend) = battery_reaction(&[BatteryEvent::Recovered], &p);
        assert!(signals.is_empty());
        assert!(!suspend);
    }

    /// End-to-end pure path: a discharging battery crossing the low then the critical threshold
    /// produces exactly the toasts + suspend the service would apply — the same `BatteryMonitor`
    /// + `battery_reaction` the async loop uses, minus the bus.
    #[test]
    fn monitor_plus_reaction_full_discharge() {
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
