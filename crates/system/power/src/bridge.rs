//! The `Send + Sync` mailbox between the async power service (which owns the
//! system-bus connection) and the synchronous compositor loop (which owns the
//! session-lock state and the notification stack).
//!
//! Modelled on the compositor's `NotificationBridge`: cloneable (an `Arc`
//! bump), lock-guarded vectors, drained each way.
//!
//! * **service → compositor** ([`PowerSignal`]): "lock the session", "post
//!   this battery toast", "we just resumed — reset the idle timer".
//! * **compositor → service** ([`PowerCommand`]): "suspend now" (the sync
//!   loop cannot block on a D-Bus round-trip, so it hands the request to the
//!   async service). Every command also wakes the service
//!   ([`PowerBridge::command_wake`]), so it acts at once instead of on a tick.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;

use crate::policy::PowerPolicy;

/// A battery/power notification the compositor should surface as an OS toast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PowerNotification {
    /// Toast title.
    pub summary: String,
    /// Toast body.
    pub body: String,
    /// `true` for the critical-battery toast (maps to urgency 2), `false`
    /// for the low-battery heads-up (urgency 1).
    pub critical: bool,
}

/// An instruction from the power service to the compositor loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PowerSignal {
    /// Lock the session (lid/power-key policy resolved to `Lock`).
    Lock,
    /// logind is about to suspend. The compositor must enter its existing
    /// lock state and draw that state before acknowledging the sleep delay
    /// inhibitor; this is distinct from a policy-only lock request.
    PreparingForSleep,
    /// A compositor-requested suspend could not be started. The compositor
    /// must cancel any pre-sleep latch it was waiting to prepare; otherwise a
    /// transient logind/polkit failure would strand the session behind its
    /// lock screen even though the machine never slept.
    SuspendFailed,
    /// Post a battery notification.
    Notify(PowerNotification),
    /// The machine just resumed from sleep — reset the idle timer so it does
    /// not immediately re-blank/re-suspend on the stale pre-sleep timestamp.
    ResumedFromSleep,
    /// power-profiles-daemon changed profile; apply the newly resolved idle
    /// timeout to the live session state machine.
    IdleSuspendChanged(Option<Duration>),
}

/// A request from the compositor loop to the power service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PowerCommand {
    /// Suspend the machine via logind: a deliberate request (the session
    /// menu, the palette) that proceeds whatever holds the machine awake.
    Suspend,
    /// The idle timer ran out and the compositor found no inhibitor. The
    /// service re-reads other programs' logind locks first, because it only
    /// polls them every 30 s, and answers [`PowerSignal::SuspendFailed`]
    /// instead of suspending when one appeared in between.
    IdleSuspend,
    /// The compositor has submitted a frame containing the lock screen. The
    /// power service may now release its logind sleep delay inhibitor.
    LockScreenDrawn,
    /// Settings changed on disk; replace the service's threshold/lid policy.
    UpdatePolicy(PowerPolicy),
}

#[derive(Default, Debug)]
struct BridgeInner {
    signals: Vec<PowerSignal>,
    commands: Vec<PowerCommand>,
    /// power-profiles-daemon's active profile as the service last read it.
    active_profile: Option<String>,
}

/// The shared power mailbox. Clone to hand a handle to each side.
#[derive(Clone, Debug)]
pub struct PowerBridge {
    inner: Arc<Mutex<BridgeInner>>,
    /// Woken on every command, so the service sleeps until there is one.
    command_wake: Arc<Notify>,
}

impl Default for PowerBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl PowerBridge {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(BridgeInner::default())),
            command_wake: Arc::new(Notify::new()),
        }
    }

    // ---- service → compositor ---------------------------------------

    /// Push a signal for the compositor loop to apply on its next frame.
    pub fn push_signal(&self, signal: PowerSignal) {
        self.lock().signals.push(signal);
    }

    /// Drain every pending signal (called once per compositor frame).
    pub fn drain_signals(&self) -> Vec<PowerSignal> {
        std::mem::take(&mut self.lock().signals)
    }

    /// Record power-profiles-daemon's active profile, read over D-Bus by the
    /// service, so the compositor resolves the profile's idle-suspend timeout
    /// on a settings reload without asking the daemon from its frame thread.
    pub fn note_active_profile(&self, profile: &str) {
        let mut inner = self.lock();
        if inner.active_profile.as_deref() != Some(profile) {
            inner.active_profile = Some(profile.to_owned());
        }
    }

    /// The active power profile the service last observed; `None` before it
    /// has read one or where power-profiles-daemon is absent.
    pub fn active_profile(&self) -> Option<String> {
        self.lock().active_profile.clone()
    }

    // ---- compositor → service ---------------------------------------

    /// Ask the power service to suspend the machine.
    pub fn request_suspend(&self) {
        self.push_command(PowerCommand::Suspend);
    }

    /// Ask the power service to suspend because the session went idle; see
    /// [`PowerCommand::IdleSuspend`].
    pub fn request_idle_suspend(&self) {
        self.push_command(PowerCommand::IdleSuspend);
    }

    pub fn lock_screen_drawn(&self) {
        self.push_command(PowerCommand::LockScreenDrawn);
    }

    pub fn update_policy(&self, policy: PowerPolicy) {
        self.push_command(PowerCommand::UpdatePolicy(policy));
    }

    fn push_command(&self, command: PowerCommand) {
        self.lock().commands.push(command);
        // `notify_one` keeps a permit when the service is busy elsewhere, so a
        // command pushed between its drain and its next wait is not lost.
        self.command_wake.notify_one();
    }

    /// The handle the service waits on for [`Self::drain_commands`] to have
    /// something to return.
    pub fn command_wake(&self) -> Arc<Notify> {
        Arc::clone(&self.command_wake)
    }

    /// Drain every pending command (called by the async service loop).
    pub fn drain_commands(&self) -> Vec<PowerCommand> {
        std::mem::take(&mut self.lock().commands)
    }

    /// Number of pending signals (diagnostics/tests).
    pub fn pending_signals(&self) -> usize {
        self.lock().signals.len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BridgeInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bridge_delivers_signals_commands_and_policy_once_to_shared_mailboxes() {
        // Phase `signals_round_trip_once`.
        {
            let b = PowerBridge::new();
            b.push_signal(PowerSignal::Lock);
            b.push_signal(PowerSignal::Notify(PowerNotification {
                summary: "Battery low".into(),
                body: "12% remaining".into(),
                critical: false,
            }));
            assert_eq!(b.pending_signals(), 2);
            let drained = b.drain_signals();
            assert_eq!(drained.len(), 2);
            assert_eq!(drained[0], PowerSignal::Lock);
            // Draining empties the queue.
            assert!(b.drain_signals().is_empty());
        }
        // Phase `commands_round_trip_once`.
        {
            let b = PowerBridge::new();
            b.request_suspend();
            assert_eq!(b.drain_commands(), vec![PowerCommand::Suspend]);
            assert!(b.drain_commands().is_empty());
        }
        // Phase `clones_share_the_mailbox`.
        {
            let a = PowerBridge::new();
            let b = a.clone();
            a.request_suspend();
            // Command pushed on `a` is visible to `b`.
            assert_eq!(b.drain_commands(), vec![PowerCommand::Suspend]);
        }
        // Phase `the_active_profile_the_service_read_is_shared`.
        {
            let service = PowerBridge::new();
            let compositor = service.clone();
            assert_eq!(compositor.active_profile(), None);
            service.note_active_profile("power-saver");
            assert_eq!(compositor.active_profile().as_deref(), Some("power-saver"));
            service.note_active_profile("performance");
            assert_eq!(compositor.active_profile().as_deref(), Some("performance"));
        }
        // Phase `policy_updates_cross_the_bridge_without_stringly_state`.
        {
            let bridge = PowerBridge::new();
            let policy = PowerPolicy::default();
            bridge.update_policy(policy);
            assert_eq!(
                bridge.drain_commands(),
                vec![PowerCommand::UpdatePolicy(policy)]
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn every_command_wakes_the_waiting_service_at_once() {
        let bridge = PowerBridge::new();
        let wake = bridge.command_wake();
        let compositor = bridge.clone();
        for push in [
            PowerBridge::request_suspend as fn(&PowerBridge),
            PowerBridge::request_idle_suspend,
            PowerBridge::lock_screen_drawn,
            |bridge: &PowerBridge| bridge.update_policy(PowerPolicy::default()),
        ] {
            let started = tokio::time::Instant::now();
            let waiting = tokio::spawn({
                let wake = std::sync::Arc::clone(&wake);
                async move { wake.notified().await }
            });
            tokio::task::yield_now().await;
            push(&compositor);
            tokio::time::timeout(Duration::from_millis(1), waiting)
                .await
                .expect("the command wakes the service without a tick")
                .expect("waiter");
            assert_eq!(tokio::time::Instant::now(), started, "no time passed");
            assert_eq!(bridge.drain_commands().len(), 1);
        }
        // A command pushed while the service is not waiting is kept for its
        // next wait rather than lost.
        compositor.request_suspend();
        tokio::time::timeout(Duration::from_millis(1), wake.notified())
            .await
            .expect("the pending command's permit wakes the next wait");
    }
}
