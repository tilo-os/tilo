//! The `Send + Sync` mailbox between the async power service (which owns the
//! system-bus connection) and the synchronous compositor loop (which owns the
//! session-lock state and the notification stack): signals one way, commands the
//! other, each drained per frame.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;

use crate::policy::PowerPolicy;
use tilo_lock::MutexRecover as _;

/// How the battery is running out, for the session to put in the words it shows
/// the person.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerNotification {
    /// Charge left, percent.
    pub percent: u8,
    /// `true` for the critical warning, `false` for the low-charge heads-up.
    pub critical: bool,
    /// `true` when the policy sleeps the machine for this critical battery.
    pub suspending: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PowerSignal {
    Lock,
    /// logind is about to suspend. The compositor must enter its lock state and
    /// draw it before acknowledging the sleep delay inhibitor; this is distinct
    /// from a policy-only lock request.
    PreparingForSleep,
    /// A compositor-requested suspend could not be started, so the compositor must
    /// cancel any pre-sleep latch it was holding; otherwise a transient logind or
    /// polkit failure would strand the session behind its lock screen.
    SuspendFailed,
    Notify(PowerNotification),
    /// The machine just resumed: reset the idle timer so it does not immediately
    /// blank or suspend on the stale pre-sleep timestamp.
    ResumedFromSleep,
    /// power-profiles-daemon changed profile; apply the newly resolved idle
    /// timeout to the live session state machine.
    IdleSuspendChanged(Option<Duration>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PowerCommand {
    /// Suspend the machine via logind: a deliberate request (the session menu, the
    /// palette) that proceeds whatever holds the machine awake.
    Suspend,
    /// The idle timer ran out and the compositor found no inhibitor. The service
    /// re-reads other programs' logind locks first, because it only polls them
    /// every 30 s, and answers [`PowerSignal::SuspendFailed`] instead of suspending
    /// when one appeared in between.
    IdleSuspend,
    /// The compositor has submitted a frame containing the lock screen, so the
    /// power service may release its logind sleep delay inhibitor.
    LockScreenDrawn,
    /// Settings changed on disk; replace the service's threshold and lid policy.
    UpdatePolicy(PowerPolicy),
}

#[derive(Default, Debug)]
struct BridgeInner {
    signals: Vec<PowerSignal>,
    commands: Vec<PowerCommand>,
    /// power-profiles-daemon's active profile as the service last read it.
    active_profile: Option<String>,
    /// The profiles power-profiles-daemon offers, in its order.
    profiles: Vec<String>,
    /// Whether the machine runs on battery, as the service last saw it; `None`
    /// before it first looked.
    on_battery: Option<bool>,
    /// How often what the session's power readings show moved: the profiles
    /// offered, the active one, the battery. The session reads them again when
    /// it changes.
    moved: u64,
}

/// How the compositor loop is woken when a signal arrives for it.
pub type SignalWake = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
pub struct PowerBridge {
    inner: Arc<Mutex<BridgeInner>>,
    /// Woken on every command, so the service sleeps until there is one.
    command_wake: Arc<Notify>,
    /// Wakes the compositor loop on every signal: a locked, blanked session parks
    /// for up to a minute, so a signal that only queued waited for whatever woke it
    /// next (a resume applied 16 s late, a sleep delay held to logind's timeout).
    signal_wake: Arc<Mutex<Option<SignalWake>>>,
    /// How often the daemon's offered profiles or active profile moved: checked on
    /// every poll with one atomic load before locking [`BridgeInner`].
    profiles_moved: Arc<AtomicU64>,
    /// Woken whenever the daemon's offered profiles or active profile moves.
    profile_wakes: Arc<Mutex<Vec<SignalWake>>>,
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
            profiles_moved: Arc::new(AtomicU64::new(0)),
            profile_wakes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Wake the compositor loop with `wake` whenever a signal is pushed.
    pub fn set_signal_wake(&self, wake: SignalWake) {
        *self.signal_wake.lock_recover() = Some(wake);
    }

    /// Wake `wake` whenever the daemon's offered profiles or active profile moves.
    pub fn on_profiles_change(&self, wake: SignalWake) {
        self.profile_wakes.lock_recover().push(wake);
    }

    /// Push a signal for the compositor loop and wake it to apply it.
    pub fn push_signal(&self, signal: PowerSignal) {
        self.lock().signals.push(signal);
        self.wake();
    }

    fn wake(&self) {
        let wake = self.signal_wake.lock_recover().clone();
        if let Some(wake) = wake {
            wake();
        }
    }

    fn wake_profiles(&self) {
        self.wake();
        let wakes = self.profile_wakes.lock_recover().clone();
        for wake in wakes {
            wake();
        }
    }

    /// Drain every pending signal (called once per compositor frame).
    pub fn drain_signals(&self) -> Vec<PowerSignal> {
        std::mem::take(&mut self.lock().signals)
    }

    /// Record power-profiles-daemon's active profile, read over D-Bus by the
    /// service, so the compositor resolves the profile's idle-suspend timeout on a
    /// settings reload without asking the daemon from its frame thread.
    pub fn note_active_profile(&self, profile: &str) {
        let mut inner = self.lock();
        if inner.active_profile.as_deref() != Some(profile) {
            inner.active_profile = Some(profile.to_owned());
            drop(inner);
            self.reading_moved();
            self.profiles_moved.fetch_add(1, Ordering::Release);
            self.wake_profiles();
        }
    }

    /// The active power profile the service last observed; `None` before it has read
    /// one or where power-profiles-daemon is absent.
    pub fn active_profile(&self) -> Option<String> {
        self.lock().active_profile.clone()
    }

    /// Record the power profiles power-profiles-daemon offers, in its order.
    pub fn note_profiles(&self, profiles: Vec<String>) {
        let mut inner = self.lock();
        if inner.profiles != profiles {
            inner.profiles = profiles;
            drop(inner);
            self.reading_moved();
            self.profiles_moved.fetch_add(1, Ordering::Release);
            self.wake_profiles();
        }
    }

    /// How often the offered power profiles or the active profile moved.
    pub fn profiles_moved(&self) -> u64 {
        self.profiles_moved.load(Ordering::Acquire)
    }

    /// The offered power profiles and the active profile read together.
    pub fn profiles_snapshot(&self) -> (Vec<String>, Option<String>) {
        let inner = self.lock();
        (inner.profiles.clone(), inner.active_profile.clone())
    }

    /// How often what the power readings show moved: a session that saw another
    /// count reads them again.
    pub fn readings_moved(&self) -> u64 {
        self.lock().moved
    }

    /// Count a move of what the readings show (the battery read anew) and wake
    /// the session's loop to read them, as a signal does.
    pub fn reading_moved(&self) {
        let mut inner = self.lock();
        inner.moved = inner.moved.wrapping_add(1);
        drop(inner);
        self.wake();
    }

    pub fn profiles(&self) -> Vec<String> {
        self.lock().profiles.clone()
    }

    /// Record whether the machine runs on battery and answer whether that changed
    /// since the service last looked: the first look only notes it, since the
    /// session started on the timer of the source it found.
    pub fn note_on_battery(&self, on_battery: bool) -> bool {
        let mut inner = self.lock();
        let changed = inner.on_battery.is_some_and(|before| before != on_battery);
        inner.on_battery = Some(on_battery);
        changed
    }

    /// Whether the machine runs on battery, as the service last saw it.
    pub fn on_battery(&self) -> bool {
        self.lock().on_battery.unwrap_or(false)
    }

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
        // `notify_one` keeps a permit when the service is busy elsewhere, so a command
        // pushed between its drain and its next wait is not lost.
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

    tilo_test_env::scenario! {
        fn the_bridge_delivers_once_and_wakes_the_compositor;

        phase the_bridge_delivers_signals_commands_and_policy_once_to_shared_mailboxes;
        {
            {
                let b = PowerBridge::new();
                b.push_signal(PowerSignal::Lock);
                b.push_signal(PowerSignal::Notify(PowerNotification {
                    percent: 12,
                    critical: false,
                    suspending: false,
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

        phase every_signal_wakes_the_compositor_loop;
        {
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
        // A command pushed while the service is not waiting is kept for its next wait
        // rather than lost.
        compositor.request_suspend();
        tokio::time::timeout(Duration::from_millis(1), wake.notified())
            .await
            .expect("the pending command's permit wakes the next wait");
    }

    #[test]
    fn power_profile_changes_move_profiles_counter_and_wake_listeners_without_moving_sleep() {
        let service = PowerBridge::new();
        let compositor = service.clone();
        assert_eq!(compositor.profiles_moved(), 0);
        let profile_woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        compositor.on_profiles_change(Arc::new({
            let profile_woken = Arc::clone(&profile_woken);
            move || {
                profile_woken.fetch_add(1, Ordering::SeqCst);
            }
        }));
        service.note_profiles(vec!["balanced".into(), "power-saver".into()]);
        assert_eq!(compositor.profiles_moved(), 1);
        assert_eq!(profile_woken.load(Ordering::SeqCst), 1);

        service.note_active_profile("power-saver");
        assert_eq!(compositor.profiles_moved(), 2);
        assert_eq!(profile_woken.load(Ordering::SeqCst), 2);
        assert_eq!(
            compositor.profiles_snapshot(),
            (
                vec!["balanced".to_owned(), "power-saver".to_owned()],
                Some("power-saver".to_owned()),
            )
        );

        service.note_active_profile("power-saver");
        assert_eq!(
            compositor.profiles_moved(),
            2,
            "a duplicate profile note does not move the counter"
        );
        service.note_active_profile("performance");
        assert_eq!(compositor.profiles_moved(), 3);
    }
}
