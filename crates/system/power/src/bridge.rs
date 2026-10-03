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
//!   async service).

use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    /// Suspend the machine via logind. Emitted after the sync loop has
    /// consulted the inhibitor registry and decided suspend should proceed.
    Suspend,
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
}

/// The shared power mailbox. Clone to hand a handle to each side.
#[derive(Clone, Debug)]
pub struct PowerBridge {
    inner: Arc<Mutex<BridgeInner>>,
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

    // ---- compositor → service ---------------------------------------

    /// Ask the power service to suspend the machine.
    pub fn request_suspend(&self) {
        self.lock().commands.push(PowerCommand::Suspend);
    }

    pub fn lock_screen_drawn(&self) {
        self.lock().commands.push(PowerCommand::LockScreenDrawn);
    }

    pub fn update_policy(&self, policy: PowerPolicy) {
        self.lock()
            .commands
            .push(PowerCommand::UpdatePolicy(policy));
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
}
