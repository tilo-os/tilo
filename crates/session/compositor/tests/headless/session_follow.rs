//! The lock the session has, and whose it is: the person's own, which their
//! other devices follow, or the idle timer's, which stays on this device; and
//! how the person using a device beside this one counts here.

use super::*;

/// Whose lock the session has: the person's own, which their other devices
/// follow, or the idle timer's, which stays on this device.
#[test]
fn the_person_s_lock_and_the_idle_timer_s_are_told_apart() {
    let mut compositor = headless(Box::new(Columns { placed: 0 }));
    assert!(
        !compositor.lock_by_idle(),
        "an unlocked session has no lock"
    );
    compositor.lock_now().expect("lock");
    assert!(compositor.locked());
    assert!(
        !compositor.lock_by_idle(),
        "a lock asked for is the person's own"
    );
    compositor.unlock().expect("unlock");
    assert!(!compositor.lock_by_idle());

    // The idle timer locks it: that lock is the timer's, not the person's.
    compositor.set_idle_config(
        tilo_compositor::IdleConfig::never()
            .with_lock(Some(Duration::from_millis(20)))
            .with_suspend(None),
    );
    pump(&mut compositor, &mut [], |compositor, _| {
        compositor.locked()
    });
    assert!(
        compositor.lock_by_idle(),
        "the timer's lock is not the person's"
    );

    // The person unlocks it, and the next lock is theirs again.
    compositor.unlock().expect("unlock");
    assert!(!compositor.lock_by_idle());
    compositor.set_idle_config(tilo_compositor::IdleConfig::never());
    compositor.lock_now().expect("lock");
    assert!(!compositor.lock_by_idle());
}

/// The person touching another of their devices nearby counts here: their
/// idle time begins there, so this device does not lock while they are on
/// the other.
#[test]
fn a_device_used_nearby_keeps_this_one_awake() {
    let mut compositor = headless(Box::new(Columns { placed: 0 }));
    assert!(
        compositor.in_use(),
        "the person just started using this device"
    );
    // A minute without touching anything here, then the person uses the
    // device beside it: this one is in use again.
    compositor.set_idle_config(
        tilo_compositor::IdleConfig::never().with_lock(Some(Duration::from_secs(1))),
    );
    assert!(
        compositor.input_idle() < Duration::from_secs(1),
        "last input was a moment ago"
    );
    compositor.note_use_elsewhere(std::time::Instant::now());
    assert!(compositor.in_use(), "used nearby is in use here");
}
