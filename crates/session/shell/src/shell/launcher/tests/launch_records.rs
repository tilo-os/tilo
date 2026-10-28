//! Launch records: a launch is admitted for its process's whole life, two
//! launches of one app in one room keep their own records, and a process
//! that has exited is refused.

use super::*;

#[cfg(target_os = "linux")]
fn launch_identity(app: &str, room: &str) -> ServiceLaunchIdentity {
    ServiceLaunchIdentity {
        app_id: app.into(),
        workspace_id: room.into(),
        allowed_compat_entry_ids: vec!["entry".into()],
        compat_entry_generation: 0,
        application_creation: None,
    }
}

/// A launch record has no clock: what once expired it was time passing
/// while other launches came and went. Those events happen here between the
/// launch and its connections, as they do in the minutes an app runs, and
/// the launch is admitted through all of them until its process exits.
#[test]
#[cfg(target_os = "linux")]
fn a_connection_long_after_launch_is_admitted() {
    let app = format!("so.tilo.notes.{}", uuid::Uuid::new_v4());
    let room = uuid::Uuid::new_v4().to_string();
    let launcher = LauncherStandIn::spawn();
    let pid = launcher.pid();
    register_trusted_window_launch(pid, launch_identity(&app, &room), Some(7), Some(3));
    let admitted = || {
        authenticated_window_launch(pid, &app, &room, None)
            .map(|launch| (launch.continuation_instance, launch.launch_ticket))
    };
    assert_eq!(admitted(), Some((Some(7), Some(3))), "its first connection");
    // Later launches start and end, each recorded and retired.
    for _ in 0..3 {
        let mut other = LauncherStandIn::spawn();
        register_trusted_window_launch(other.pid(), launch_identity(&app, &room), None, None);
        other.stop();
        unregister_launched_process(other.pid());
    }
    // Its compatibility service takes its handoff, once.
    let (_, start_time) = proc_stat(pid).expect("the launch is running");
    assert!(consume_trusted_service_launch(pid, start_time, &app, &room).is_some());
    // Its helpers and reconnects come later, as often as they come.
    for _ in 0..3 {
        assert_eq!(admitted(), Some((Some(7), Some(3))), "a later connection");
        assert!(attest_trusted_service_launch_for_pid(pid, &app, &room).is_some());
    }
    drop(launcher);
    unregister_launched_process(pid);
    assert_eq!(admitted(), None, "its record ended with its process");
}

#[test]
#[cfg(target_os = "linux")]
fn two_launches_of_one_app_in_one_room_keep_their_own_records() {
    let app = format!("so.tilo.notes.{}", uuid::Uuid::new_v4());
    let room = uuid::Uuid::new_v4().to_string();
    let ours = std::process::id();

    // Windows the session connects itself: each by its instance, whichever
    // connection the broker reaches first.
    let (first, second) = (
        uuid::Uuid::new_v4().as_u64_pair().0,
        uuid::Uuid::new_v4().as_u64_pair().0,
    );
    register_in_process_window(&app, &room, first);
    register_in_process_window(&app, &room, second);
    let window = |instance| {
        authenticated_window_launch(ours, &app, &room, Some(instance))
            .map(|launch| launch.continuation_instance)
    };
    assert_eq!(window(second), Some(Some(second)));
    assert_eq!(window(first), Some(Some(first)));
    assert_eq!(window(first), None, "each connection spends its own record");
    assert!(
        authenticated_window_launch(ours, &app, &room, None).is_none(),
        "a window's record is not a windowless connection's"
    );

    // Launched processes: each keeps its own, and one ending leaves the other.
    let mut one = LauncherStandIn::spawn();
    let two = LauncherStandIn::spawn();
    register_trusted_window_launch(one.pid(), launch_identity(&app, &room), Some(1), Some(11));
    register_trusted_window_launch(two.pid(), launch_identity(&app, &room), Some(2), Some(22));
    let launched = |pid| {
        authenticated_window_launch(pid, &app, &room, None)
            .map(|launch| (launch.continuation_instance, launch.launch_ticket))
    };
    assert_eq!(launched(one.pid()), Some((Some(1), Some(11))));
    assert_eq!(launched(two.pid()), Some((Some(2), Some(22))));
    one.stop();
    unregister_launched_process(one.pid());
    assert_eq!(launched(one.pid()), None);
    assert_eq!(launched(two.pid()), Some((Some(2), Some(22))));
    drop(two);
}

#[test]
#[cfg(target_os = "linux")]
fn a_process_that_has_exited_is_refused() {
    let app = format!("so.tilo.notes.{}", uuid::Uuid::new_v4());
    let room = uuid::Uuid::new_v4().to_string();
    let mut launcher = LauncherStandIn::spawn();
    let pid = launcher.pid();
    register_trusted_window_launch(pid, launch_identity(&app, &room), None, Some(5));
    assert!(authenticated_window_launch(pid, &app, &room, None).is_some());

    // It exits. Until the shell reaps it, its pid and start time are still
    // there to read; the record is bound to the process, and refuses.
    launcher.child.kill().expect("end the launch");
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: waits for the exit of our own child without reaping it, into a
    // zeroed siginfo the call owns for its duration.
    let waited = unsafe {
        libc::waitid(
            libc::P_PID,
            pid,
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOWAIT,
        )
    };
    assert_eq!(waited, 0, "the launch exits");
    assert!(
        proc_stat(pid).is_some(),
        "the exited launch is not yet reaped"
    );
    assert!(authenticated_window_launch(pid, &app, &room, None).is_none());
    assert!(attest_trusted_service_launch_for_pid(pid, &app, &room).is_none());
    assert!(consume_trusted_service_launch_for_pid(pid, &app, &room).is_none());

    // Its exit is reported: the record ends.
    launcher.stop();
    unregister_launched_process(pid);
    assert!(!lock(&TRUSTED_SERVICE_LAUNCHES).contains_key(&pid));
    assert!(authenticated_window_launch(pid, &app, &room, None).is_none());
}

#[test]
#[cfg(target_os = "linux")]
fn private_window_launch_binding_is_exact_live_and_host_retained() {
    let launcher = LauncherStandIn::spawn();
    let pid = launcher.pid();
    let identity = ServiceLaunchIdentity {
        app_id: "so.tilo.code".into(),
        workspace_id: "private-window-room".into(),
        allowed_compat_entry_ids: Vec::new(),
        compat_entry_generation: 0,
        application_creation: None,
    };
    register_trusted_window_launch(pid, identity, Some(42), Some(9));
    assert!(
        authenticated_window_launch(pid, "so.tilo.notes", "private-window-room", None).is_none()
    );
    assert!(authenticated_window_launch(pid, "so.tilo.code", "other-room", None).is_none());
    assert!(
        authenticated_window_launch(
            launcher.descendant(),
            "so.tilo.code",
            "private-window-room",
            None
        )
        .is_none()
    );
    let bound = authenticated_window_launch(pid, "so.tilo.code", "private-window-room", None)
        .expect("original exact launcher record");
    assert_eq!(bound.continuation_instance, Some(42));
    assert_eq!(bound.launch_ticket, Some(9), "the launch's own ticket");
    // Kernel start time is part of the retained record, not app-reported identity.
    lock(&TRUSTED_SERVICE_LAUNCHES)
        .get_mut(&pid)
        .unwrap()
        .start_time = u64::MAX;
    assert!(
        authenticated_window_launch(pid, "so.tilo.code", "private-window-room", None).is_none()
    );
    lock(&TRUSTED_SERVICE_LAUNCHES).remove(&pid);
    assert!(
        authenticated_window_launch(pid, "so.tilo.code", "private-window-room", None).is_none()
    );
}

#[test]
#[cfg(target_os = "linux")]
fn in_process_private_window_binding_is_single_use_and_exact() {
    let app = format!("so.tilo.code.{}", uuid::Uuid::new_v4());
    let room = uuid::Uuid::new_v4().to_string();
    register_in_process_window(&app, &room, 42);
    assert!(
        authenticated_window_launch(std::process::id(), &app, "other-room", Some(42)).is_none()
    );
    assert!(authenticated_window_launch(u32::MAX, &app, &room, Some(42)).is_none());
    let bound = authenticated_window_launch(std::process::id(), &app, &room, Some(42)).unwrap();
    assert_eq!(bound.continuation_instance, Some(42));
    assert!(authenticated_window_launch(std::process::id(), &app, &room, Some(42)).is_none());
    register_in_process_launch(&app, &room);
    assert_eq!(
        authenticated_window_launch(std::process::id(), &app, &room, None)
            .unwrap()
            .continuation_instance,
        None
    );
}
