use super::reapply_power_policy;
use crate::compositor::backend::drm::input_config::SettingsChange;
use crate::compositor::idle::{IdleConfig, SessionManager, SessionTransition};
use std::time::{Duration, Instant};

/// A write through the settings store reaches the running session: the
/// compositor learns of it from the live document, derives the idle
/// policy from that same document and the idle state machine stops
/// locking.
#[test]
#[serial_test::serial]
fn lock_after_never_stops_auto_lock_in_the_running_session() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let session_settings = settings_store::local().unwrap();
    let settings = crate::shell::test_support::install_settings(session_settings);
    let live = settings_store::live::LiveSettings::start(|| {});
    let mut follower = settings_store::live::SectionFollower::new(&live);
    let bridge = tilo_power_mgmt::PowerBridge::new();
    let t0 = Instant::now();
    let fresh = || {
        SessionManager::new(
            IdleConfig::from_env_values(Some("0"), Some("300"), Some("0")),
            t0,
        )
    };

    // Phase `the_default_locks_after_five_minutes`.
    {
        let mut session = fresh();
        reapply_power_policy(&mut session, &bridge, false);
        assert!(
            session
                .tick(t0 + Duration::from_secs(6 * 60))
                .contains(&SessionTransition::Locked)
        );
    }

    // Phase `setting_it_to_never_is_applied_live`.
    {
        let mut running = fresh();
        let before = live.generation();
        settings_store::SectionWrite::new()
            .set(&settings_store::power::LOCK_MINUTES, &0)
            .commit_by(&settings_store::ChangeOrigin::action("settings.power"))
            .expect("the setting is stored");

        let snapshot = live.wait_for_change(before, tilo_test_env::hang_bound());
        assert_eq!(snapshot.section("power")["lock_minutes"], 0);
        assert!(
            SettingsChange::from(follower.changed_sections(&live)).power(),
            "the write names the power section"
        );
        // What the compositor does on that edge.
        reapply_power_policy(&mut running, &bridge, false);
        assert!(
            !running
                .tick(t0 + Duration::from_secs(24 * 60 * 60))
                .contains(&SessionTransition::Locked),
            "never means the session does not lock on its own"
        );
        assert!(!running.is_locked());
        assert_eq!(
            running.request_lock(Instant::now()),
            vec![SessionTransition::Locked],
            "locking by hand still works"
        );
    }
    drop(settings);
}
