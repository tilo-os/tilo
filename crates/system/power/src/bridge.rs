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
use tilo_lock::MutexRecover as _;

/// A battery/power notification the compositor should surface as an OS toast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PowerNotification {
    /// Toast title.
    pub summary: String,
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
    /// Whether the machine runs on battery, as the service last saw it;
    /// `None` before it first looked.
    on_battery: Option<bool>,
}

/// How the compositor loop is woken when a signal arrives for it.
pub type SignalWake = Arc<dyn Fn() + Send + Sync>;

/// The shared power mailbox. Clone to hand a handle to each side.
#[derive(Clone)]
pub struct PowerBridge {
    inner: Arc<Mutex<BridgeInner>>,
    /// Woken on every command, so the service sleeps until there is one.
    command_wake: Arc<Notify>,
    /// Wakes the compositor loop on every signal. The loop parks until its
    /// next deadline or event: a locked, blanked session parks for up to a
    /// minute, and a signal only queued waited for whatever woke it next (a
    /// resume applied 16 s late, a sleep delay held to logind's timeout).
    signal_wake: Arc<Mutex<Option<SignalWake>>>,
}

impl std::fmt::Debug for PowerBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PowerBridge")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
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
            signal_wake: Arc::new(Mutex::new(None)),
        }
    }

    /// Wake the compositor loop with `wake` whenever a signal is pushed.
    pub fn set_signal_wake(&self, wake: SignalWake) {
        *self.signal_wake.lock_recover() = Some(wake);
    }

    // ---- service → compositor ---------------------------------------

    /// Push a signal for the compositor loop and wake it to apply it.
    pub fn push_signal(&self, signal: PowerSignal) {
        self.lock().signals.push(signal);
        let wake = self.signal_wake.lock_recover().clone();
        if let Some(wake) = wake {
            wake();
        }
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

    /// Record whether the machine runs on battery, as the service saw it, so
    /// the compositor resolves the source's idle-suspend timer on a settings
    /// reload. Returns whether it changed since the service last looked: the
    /// first look only notes it (the session started on the timer of the
    /// source it found).
    pub fn note_on_battery(&self, on_battery: bool) -> bool {
        let mut inner = self.lock();
        let changed = inner.on_battery.is_some_and(|before| before != on_battery);
        inner.on_battery = Some(on_battery);
        changed
    }

    /// Whether the machine runs on battery, as the service last saw it;
    /// `false` before it has looked.
    pub fn on_battery(&self) -> bool {
        self.lock().on_battery.unwrap_or(false)
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
        self.inner.lock_recover()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bridge_delivers_signals_commands_and_policy_once_to_shared_mailboxes() {
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
            assert!(b.drain_signals().is_empty());
        }
        {
            let b = PowerBridge::new();
            b.request_suspend();
            assert_eq!(b.drain_commands(), vec![PowerCommand::Suspend]);
            assert!(b.drain_commands().is_empty());
        }
        {
            let a = PowerBridge::new();
            let b = a.clone();
            a.request_suspend();
            assert_eq!(b.drain_commands(), vec![PowerCommand::Suspend]);
        }
        {
            let service = PowerBridge::new();
            let compositor = service.clone();
            assert_eq!(compositor.active_profile(), None);
            service.note_active_profile("power-saver");
            assert_eq!(compositor.active_profile().as_deref(), Some("power-saver"));
            service.note_active_profile("performance");
            assert_eq!(compositor.active_profile().as_deref(), Some("performance"));
        }
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

    #[test]
    fn every_signal_wakes_the_compositor_loop() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let bridge = PowerBridge::new();
        // Before a wake is set a signal still queues.
        bridge.push_signal(PowerSignal::Lock);
        let woken = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&woken);
        bridge.set_signal_wake(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        let service = bridge.clone();
        service.push_signal(PowerSignal::ResumedFromSleep);
        service.push_signal(PowerSignal::PreparingForSleep);
        assert_eq!(woken.load(Ordering::SeqCst), 2);
        assert_eq!(
            bridge.drain_signals(),
            vec![
                PowerSignal::Lock,
                PowerSignal::ResumedFromSleep,
                PowerSignal::PreparingForSleep
            ]
        );
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
