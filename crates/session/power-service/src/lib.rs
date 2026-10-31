//! The session's **power policy**: when the machine blanks, locks and
//! sleeps on its own, what the lid, the power key and a failing battery do,
//! and what keeps it awake.
//!
//! # Shape
//!
//! The decisions are [`tilo_power_mgmt::PowerPolicy`], which the session's
//! settings loader reads from the person's settings and narrows to what this
//! machine owes (an always-on host never sleeps on its own). The
//! [`SessionPower`] service runs `tilo_power_mgmt::PowerService` over D-Bus —
//! logind for sleeping, `UPower` for the lid and the battery — and tells the
//! session's loop what to do through the [`PowerLink`]:
//!
//! * the idle timeouts go to the compositor, which runs its own idle state
//!   machine, and a suspend it decides on comes back to the service, which
//!   re-reads other programs' logind locks before it sleeps the machine;
//! * a lid, a critical battery or the power key resolved to *lock* locks the
//!   session, resolved to *suspend* goes to logind;
//! * logind's `PrepareForSleep` locks the session first and holds the sleep
//!   delay until the lock screen's frame is drawn; a failed suspend lifts
//!   only the lock that this sleep installed; the resume edge resets the
//!   idle timers;
//! * a suspend inhibitor (a download, a remote session, media playing) holds
//!   the idle timer and is mirrored into a logind lock, so other programs'
//!   `systemd-inhibit` and the platform's own idle sleep wait for it too.
//!
//! # Published
//!
//! The service publishes two resources, so a person, an app and a headless
//! test read and hold the same things through the session's action host:
//!
//! * `power:sleep` — the policy in force, what holds the machine awake, and
//!   the actions `keep-awake`, `release`, `power-key` and `lid`;
//! * `power:battery` — the pack's charge, its state and the last low or
//!   critical battery notice.
//!
//! # Wiring
//!
//! The session passes a [`PowerLink`] in and applies what it says; nothing
//! here touches the compositor or draws anything.

pub mod effects;
mod model;
mod service;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tilo_lock::MutexRecover as _;
use tilo_power_mgmt::inhibit::{INHIBIT_IDLE, INHIBIT_SUSPEND};
use tilo_power_mgmt::{
    InhibitRegistry, InhibitToken, PowerAction, PowerBridge, PowerNotification, PowerPolicy,
    PowerSignal,
};

pub use model::{Battery, ChargeCapCapability, Notice, Sleep};
pub use service::{BATTERY, SLEEP, SessionPower};

/// The pack's charge-limit control as this machine offers it: the limit the
/// driver exposes and the write that sets it. The real one is the
/// [`tilo_power_mgmt::sysfs`] reading plus the narrowly scoped
/// `tilo-battery-cap@` service that owns the write ([`OsChargePack`]); a
/// test's is in memory.
pub trait ChargePack: Send + Sync + std::fmt::Debug {
    /// The limit this pack's driver offers, its current value and the ones
    /// it takes; `None` where the hardware offers no limit.
    fn limit(&self) -> Option<ChargeCapCapability>;
    /// Apply `percent` and read the hardware back.
    ///
    /// # Errors
    ///
    /// Why the pack did not take it: out of the driver's range, the helper
    /// refused, or the firmware kept another value.
    fn set_limit(&self, percent: u32) -> Result<ChargeCapCapability, String>;
}

/// This machine's pack: the driver's limit, written through the
/// `tilo-battery-cap@` service that alone may touch it.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsChargePack;

impl ChargePack for OsChargePack {
    fn limit(&self) -> Option<ChargeCapCapability> {
        os_actions::charge_capability()
    }

    fn set_limit(&self, percent: u32) -> Result<ChargeCapCapability, String> {
        os_actions::set_charge_limit_checked(percent)
    }
}

/// The idle, lid, key and battery policy of this session, and the way to
/// every part of the machine that acts on it.
///
/// Clone freely: every clone shares one policy, one registry and one
/// mailbox.
#[derive(Clone, Debug)]
pub struct PowerLink {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    bridge: PowerBridge,
    registry: InhibitRegistry,
    /// The pack's charge limit, when this machine has one.
    pack: std::sync::Arc<dyn ChargePack>,
    /// The policy in force.
    policy: Mutex<PowerPolicy>,
    /// The last battery notice, until a surface shows it.
    notice: Mutex<Option<Notice>>,
    /// The holds taken through this link, by ticket.
    holds: Mutex<BTreeMap<u64, Held>>,
    next_ticket: AtomicU64,
    /// Whether a service over logind runs for this link: without one the
    /// machine's sleep is the session actions' business, not ours.
    attached: AtomicBool,
}

#[derive(Debug)]
struct Held {
    reason: String,
    _token: InhibitToken,
}

impl Default for PowerLink {
    fn default() -> Self {
        Self::new(PowerPolicy::default())
    }
}

impl PowerLink {
    /// A link running `policy`, as the session's settings loader resolved
    /// it, over this machine's own pack.
    #[must_use]
    pub fn new(policy: PowerPolicy) -> Self {
        Self::over(policy, std::sync::Arc::new(OsChargePack))
    }

    /// A link running `policy`, with `pack` behind the charge limit: a test
    /// drives a pack in memory, a capture shows one.
    #[must_use]
    pub fn over(policy: PowerPolicy, pack: std::sync::Arc<dyn ChargePack>) -> Self {
        Self {
            inner: Arc::new(Inner {
                bridge: PowerBridge::new(),
                registry: InhibitRegistry::new(),
                pack,
                policy: Mutex::new(policy),
                notice: Mutex::new(None),
                holds: Mutex::new(BTreeMap::new()),
                next_ticket: AtomicU64::new(1),
                attached: AtomicBool::new(false),
            }),
        }
    }

    /// The mailbox between the service and the session's loop.
    #[must_use]
    pub fn bridge(&self) -> &PowerBridge {
        &self.inner.bridge
    }

    /// The holds in force, this link's and everyone else's (the portal's, a
    /// task's).
    #[must_use]
    pub fn registry(&self) -> &InhibitRegistry {
        &self.inner.registry
    }

    /// The policy in force.
    #[must_use]
    pub fn policy(&self) -> PowerPolicy {
        *self.inner.policy.lock_recover()
    }

    /// Run `policy` from now on, and tell the service.
    pub fn set_policy(&self, policy: PowerPolicy) {
        *self.inner.policy.lock_recover() = policy;
        self.inner.bridge.update_policy(policy);
    }

    /// Mark a service over logind as running for this link.
    pub fn attached(&self) {
        self.inner.attached.store(true, Ordering::Release);
    }

    /// Whether a service over logind runs for this link.
    #[must_use]
    pub fn is_attached(&self) -> bool {
        self.inner.attached.load(Ordering::Acquire)
    }

    /// Whether something keeps the machine from suspending on its own: a
    /// portal hold, media playing, a task that must not be interrupted.
    #[must_use]
    pub fn suspend_held(&self) -> bool {
        self.inner.registry.is_inhibited_for(INHIBIT_SUSPEND)
    }

    /// Whether something says the person is there without touching anything
    /// (an app's `Inhibit IDLE`, a presentation): nothing dims, blanks, locks
    /// or suspends while it holds.
    #[must_use]
    pub fn idle_held(&self) -> bool {
        self.inner.registry.is_inhibited_for(INHIBIT_IDLE)
    }

    /// Why, in the holders' words.
    #[must_use]
    pub fn hold_reasons(&self) -> Vec<String> {
        self.inner.registry.active_reasons()
    }

    /// Hold the machine awake for `reason` until the ticket is released.
    #[must_use]
    pub fn keep_awake(&self, reason: &str) -> u64 {
        let ticket = self.inner.next_ticket.fetch_add(1, Ordering::Relaxed);
        let token = self.inner.registry.acquire_for(reason, INHIBIT_SUSPEND);
        self.inner.holds.lock_recover().insert(
            ticket,
            Held {
                reason: reason.to_owned(),
                _token: token,
            },
        );
        ticket
    }

    /// Hold the machine awake for `reason` until `until`, a lease that stops
    /// counting then unless it is taken again.
    #[must_use]
    pub fn keep_awake_until(&self, reason: &str, until: Instant) -> u64 {
        let ticket = self.inner.next_ticket.fetch_add(1, Ordering::Relaxed);
        let token = self
            .inner
            .registry
            .acquire_until(reason, INHIBIT_SUSPEND, until);
        self.inner.holds.lock_recover().insert(
            ticket,
            Held {
                reason: reason.to_owned(),
                _token: token,
            },
        );
        ticket
    }

    /// Let the machine sleep on its own again. Whether `ticket` was held.
    pub fn release(&self, ticket: u64) -> bool {
        self.inner.holds.lock_recover().remove(&ticket).is_some()
    }

    /// What this link holds, by ticket.
    #[must_use]
    pub fn holds(&self) -> Vec<(u64, String)> {
        self.inner
            .holds
            .lock_recover()
            .iter()
            .map(|(ticket, held)| (*ticket, held.reason.clone()))
            .collect()
    }

    /// Carry out what the policy resolved to: a lock for the session's
    /// loop, a suspend through logind, nothing at all. What was done, for
    /// the caller's reading.
    pub fn act(&self, action: PowerAction) -> &'static str {
        match action {
            PowerAction::Suspend => {
                self.inner.bridge.request_suspend();
            }
            PowerAction::Lock => self.inner.bridge.push_signal(PowerSignal::Lock),
            PowerAction::Ignore => {}
        }
        action.label()
    }

    /// What the power key does, if anything.
    #[must_use]
    pub fn power_key(&self) -> Option<PowerAction> {
        self.policy().on_power_key()
    }

    /// What the lid does, if anything: only closing it does.
    #[must_use]
    pub fn lid(&self, closed: bool) -> Option<PowerAction> {
        self.policy().on_lid(closed)
    }

    /// Keep the battery notice `notice` until a surface shows it.
    pub fn note_notice(&self, notice: &PowerNotification) {
        *self.inner.notice.lock_recover() = Some(Notice {
            summary: notice.summary.clone(),
            body: notice.body.clone(),
            critical: notice.critical,
        });
    }

    /// The last battery notice, taking it.
    pub fn take_notice(&self) -> Option<Notice> {
        self.inner.notice.lock_recover().take()
    }

    /// The battery as it reads now, with the notice a surface has not shown
    /// yet.
    #[must_use]
    pub fn battery(&self) -> Battery {
        let snapshot = tilo_power_mgmt::sysfs_battery_snapshot();
        let minutes_left = match tilo_power_mgmt::sysfs_battery_time() {
            Some(
                tilo_power_mgmt::BatteryTime::ToEmpty(minutes)
                | tilo_power_mgmt::BatteryTime::ToFull(minutes),
            ) => Some(minutes),
            None => None,
        };
        Battery {
            present: snapshot.present,
            percentage: snapshot.percent,
            state: state_label(snapshot.state),
            on_battery: snapshot.runs_on_battery(),
            minutes_left,
            charge_limit: self.inner.pack.limit(),
            notice: self.inner.notice.lock_recover().clone(),
        }
    }

    /// Stop charging at `percent`, where the pack's driver offers a limit,
    /// and read the hardware back.
    ///
    /// # Errors
    ///
    /// Why the pack did not take it.
    pub fn set_charge_limit(&self, percent: u32) -> Result<ChargeCapCapability, String> {
        self.inner.pack.set_limit(percent)
    }

    /// The policy in force and what holds the machine awake.
    #[must_use]
    pub fn sleep(&self) -> Sleep {
        let policy = self.policy();
        Sleep {
            blank_minutes: minutes(policy.idle_blank),
            lock_minutes: minutes(policy.idle_lock),
            suspend_minutes: minutes(policy.idle_suspend),
            lock_on_sleep: policy.lock_on_sleep,
            lid_close: policy.lid_close_action.label(),
            power_key: policy.power_key_action.label(),
            held_awake: self.suspend_held(),
            holds: self.hold_reasons(),
        }
    }
}

/// `timeout` in whole minutes, rounding up: what a person reads.
fn minutes(timeout: Option<std::time::Duration>) -> Option<u64> {
    timeout.map(|timeout| timeout.as_secs().div_ceil(60))
}

/// The battery state as a person reads it.
fn state_label(state: tilo_power_mgmt::BatteryState) -> &'static str {
    use tilo_power_mgmt::BatteryState;
    match state {
        BatteryState::Charging => "charging",
        BatteryState::Discharging => "discharging",
        BatteryState::Empty => "empty",
        BatteryState::Full => "full",
        BatteryState::PendingCharge => "pending-charge",
        BatteryState::PendingDischarge => "pending-discharge",
        BatteryState::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests;
