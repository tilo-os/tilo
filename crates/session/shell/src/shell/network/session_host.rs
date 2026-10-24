//! A compositor-free session hosting its own room (`runtime_host::Session`):
//! the same placement the desktop runs, asked where it stands so the session
//! can wait for its room to be served before it serves apps.

use super::{RoomId, ShellNetwork};

/// Where the placement of a room on this device's host stands.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Placement {
    /// The native owner holds the room: its host serves it.
    Held,
    /// The room waits to be placed on this device's host.
    Placing,
    /// The last pass did not place the room, and none is due until
    /// something placement depends on changes: why.
    NotPlaced(String),
    /// The host made the room and this device's enrollment into it runs.
    Enrolling,
    /// The enrollment ended but the native owner does not hold the room's
    /// replica, or the owner is not there to hold it: why, if known.
    NotInstalled(String),
}

impl Placement {
    /// The step a placement that stopped here was waiting on.
    pub(crate) fn step(&self) -> &'static str {
        match self {
            Self::Held => "nothing",
            Self::Placing | Self::NotPlaced(_) => "the room being placed on this device's host",
            Self::Enrolling => "this device's enrollment into the room",
            Self::NotInstalled(_) => "the room's replica being installed",
        }
    }
}

impl ShellNetwork {
    /// Where the placement of `room`, a workspace made on this device, stands.
    pub(crate) fn placement(&self, room: RoomId) -> Placement {
        let owner = match self.native_owner() {
            Ok(owner) => owner,
            Err(error) => return Placement::NotInstalled(error),
        };
        if owner.rooms().contains(&room) {
            Placement::Held
        } else if self.enrolling_into(room).is_some() {
            Placement::Enrolling
        } else if let Some(error) = self.placement_failure(room) {
            Placement::NotPlaced(error)
        } else if self.waits_for_placement(room) {
            Placement::Placing
        } else {
            Placement::NotInstalled("the enrollment ended without the room".into())
        }
    }

    /// Whether `room` still waits to be placed: a placement pass that failed
    /// leaves it so.
    pub(crate) fn waits_for_placement(&self, room: RoomId) -> bool {
        self.tree.read().pending_placements.contains(&room)
    }

    /// Whether this device's own host is open and serves `room`.
    #[cfg(test)]
    pub(crate) fn serves_here(&self, room: RoomId) -> bool {
        self.room_host().is_some() && self.hosted_here(room)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::time::Instant;

    use super::super::idle_quiet_tests::{
        LONE_CPU_BUDGET, LONE_IDLE, LONE_SETTLE, LONE_WAKEUP_BUDGET, in_a_process_of_its_own,
        session_threads,
    };
    use super::RoomId;

    /// A runtime session hosting its workspace on this device, with no peer
    /// and no app connected, runs nothing periodic: measured as the lone idle
    /// desktop is, over every thread of its process.
    #[test]
    #[serial_test::serial]
    fn an_idle_session_hosting_its_workspace_wakes_nothing() {
        if in_a_process_of_its_own(concat!(
            module_path!(),
            "::an_idle_session_hosting_its_workspace_wakes_nothing"
        )) {
            return;
        }
        let mut home = tilo_test_env::TestHome::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        home.env()
            .set("TILO_IDENTITY_ROOT", dir.path().join("identity"));
        let profile = dir.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let session = crate::runtime_host::Session::start(&profile).unwrap();
        let room = RoomId::parse(&session.room).unwrap();
        assert!(
            session.network().serves_here(room),
            "its own host serves its workspace"
        );

        std::thread::sleep(LONE_SETTLE);
        let before = session_threads(None);
        let started = Instant::now();
        std::thread::sleep(LONE_IDLE);
        let wakeups =
            tilo_test_env::idle::Wakeups::between(before, session_threads(None), started.elapsed());
        eprintln!("an idle hosting session's threads, {wakeups}");
        assert!(
            wakeups.blocking_per_second() <= LONE_WAKEUP_BUDGET,
            "an idle session hosting its workspace woke {:.2} times a second, over \
             {LONE_WAKEUP_BUDGET}/s:\n{wakeups}",
            wakeups.blocking_per_second(),
        );
        assert!(
            wakeups.total_cpu_percent() <= LONE_CPU_BUDGET,
            "an idle session hosting its workspace used {:.2} % of a core, over \
             {LONE_CPU_BUDGET} %:\n{wakeups}",
            wakeups.total_cpu_percent(),
        );
        assert!(
            session.network().serves_here(room),
            "its host stayed open throughout"
        );
        drop(session);
    }
}
