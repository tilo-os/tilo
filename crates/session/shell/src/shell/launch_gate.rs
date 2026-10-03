//! Holding app launches until the session's services exist.
//!
//! An app resolves its collaboration descriptor once, when it starts. A
//! launch that runs while the collaboration broker is still being brought
//! up gets no broker for its whole life. So while the session is starting
//! its network (after first-login setup), launches wait here instead of
//! running, and run in the order they were requested once it is up. If the
//! start fails, the waiting launches are handed back to be reported as
//! failed, never left waiting.

use std::collections::VecDeque;

/// Launches that may wait at once. A person does not open this many apps in
/// the moments a network takes to start; beyond it, a launch fails and says so.
pub const MAX_HELD_LAUNCHES: usize = 32;

/// What [`LaunchGate::admit`] decided for one launch.
#[derive(Debug, PartialEq, Eq)]
pub enum Admission<T> {
    /// The gate is open: run the launch now.
    Run(T),
    /// The launch waits for [`LaunchGate::open`].
    Held,
    /// The same launch is already waiting; it will run once, not twice.
    AlreadyHeld,
    /// Too many launches are waiting; report this one as failed.
    Full(T),
}

/// Launches held while the session's services start. `K` identifies a
/// launch for deduplication; `T` is the launch itself.
#[derive(Debug)]
pub struct LaunchGate<K, T> {
    held: Option<VecDeque<(K, T)>>,
    capacity: usize,
}

impl<K: PartialEq, T> LaunchGate<K, T> {
    /// An open gate: every launch runs at once.
    pub const fn open_with_capacity(capacity: usize) -> Self {
        Self {
            held: None,
            capacity,
        }
    }

    /// Whether launches are being held.
    pub const fn is_holding(&self) -> bool {
        self.held.is_some()
    }

    /// Start holding launches. Launches already held stay held.
    pub fn hold(&mut self) {
        if self.held.is_none() {
            self.held = Some(VecDeque::new());
        }
    }

    /// Decide what happens to a launch requested now.
    pub fn admit(&mut self, key: K, launch: T) -> Admission<T> {
        let Some(held) = self.held.as_mut() else {
            return Admission::Run(launch);
        };
        if held.iter().any(|(existing, _)| *existing == key) {
            return Admission::AlreadyHeld;
        }
        if held.len() >= self.capacity {
            return Admission::Full(launch);
        }
        held.push_back((key, launch));
        Admission::Held
    }

    /// Stop holding. Returns the held launches in the order they were
    /// requested; the caller runs them if the start succeeded, or reports
    /// them failed if it did not. Later launches run at once.
    pub fn open(&mut self) -> Vec<T> {
        self.held
            .take()
            .map(|held| held.into_iter().map(|(_, launch)| launch).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate() -> LaunchGate<&'static str, &'static str> {
        LaunchGate::open_with_capacity(3)
    }

    #[test]
    fn launch_gate_runs_open_launches_and_holds_others_once_in_bounded_order() {
        fn an_open_gate_runs_launches_at_once() {
            let mut gate = gate();
            assert_eq!(gate.admit("files", "files"), Admission::Run("files"));
            assert!(gate.open().is_empty());
        }

        fn launches_wait_while_holding_and_run_in_request_order_once_open() {
            let mut gate = gate();
            gate.hold();
            assert_eq!(gate.admit("files", "files"), Admission::Held);
            assert_eq!(gate.admit("notes", "notes"), Admission::Held);
            assert!(gate.is_holding());
            assert_eq!(gate.open(), vec!["files", "notes"]);
            assert!(!gate.is_holding());
            assert_eq!(gate.admit("music", "music"), Admission::Run("music"));
        }

        fn a_repeated_launch_waits_once() {
            let mut gate = gate();
            gate.hold();
            assert_eq!(gate.admit("files", "first click"), Admission::Held);
            assert_eq!(gate.admit("files", "second click"), Admission::AlreadyHeld);
            assert_eq!(gate.open(), vec!["first click"]);
        }

        fn the_wait_is_bounded() {
            let mut gate = gate();
            gate.hold();
            for app in ["a", "b", "c"] {
                assert_eq!(gate.admit(app, app), Admission::Held);
            }
            assert_eq!(gate.admit("d", "d"), Admission::Full("d"));
            assert_eq!(gate.open(), vec!["a", "b", "c"]);
        }

        fn holding_again_keeps_what_is_already_held() {
            let mut gate = gate();
            gate.hold();
            assert_eq!(gate.admit("files", "files"), Admission::Held);
            gate.hold();
            assert_eq!(gate.open(), vec!["files"]);
        }

        an_open_gate_runs_launches_at_once();
        launches_wait_while_holding_and_run_in_request_order_once_open();
        a_repeated_launch_waits_once();
        the_wait_is_bounded();
        holding_again_keeps_what_is_already_held();
    }
}
