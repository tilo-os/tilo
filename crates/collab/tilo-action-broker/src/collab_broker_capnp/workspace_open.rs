//! A workspace's `open` answers with the instance it started.
//!
//! The launcher answers a ticket for each instance it starts
//! ([`Launched::Started`]), and keeps it in its own launch record; the
//! instance's collaboration connection presents it when the broker
//! authenticates that connection. The `open` stays accepted until then and
//! completes once the instance has published what it offers, naming it by
//! peer id: two opens of one app racing each other each get their own
//! instance, and a slow start never answers with another one.
use super::{CapnpBrokerState, InvocationOwner};
use crate::services::Launched;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;

/// How long an `open` waits for the instance it started to connect: as long
/// as a host keeps the launch record that instance connects with.
pub(super) const CONNECT_WAIT: Duration = Duration::from_secs(30);
/// How long a connected instance may take to publish before its `open`
/// completes all the same: an app may offer nothing.
pub(super) const PUBLISH_WAIT: Duration = Duration::from_secs(3);

/// The key an `open`'s answer names its instance by, in
/// [`ActionOutput::value`].
pub(super) const OPENED_PEER_FIELD: &str = "peer_id";

/// An authenticated connection's launch: the ticket its host recorded, and
/// the app and room the connection was admitted for.
pub(super) struct ConnectedLaunch {
    pub(super) ticket: u64,
    pub(super) app_id: String,
    pub(super) room: String,
}

/// One `open` waiting for the instance it started.
struct Opening {
    job: u64,
    actor: String,
    invocation: InvocationId,
    ticket: u64,
    app_id: String,
    room: String,
    /// The instance, once it connected with the ticket.
    peer: Option<u64>,
    deadline: Instant,
}

/// The workspace `open`s waiting for their instances.
#[derive(Default)]
pub(super) struct WorkspaceOpens(Mutex<Vec<Opening>>);

/// An `open`'s terminal update, for the broker to record.
struct Opened {
    job: u64,
    actor: String,
    update: InvocationUpdate,
}

impl Opening {
    fn opened(self, peer: Option<u64>) -> Opened {
        Opened {
            job: self.job,
            actor: self.actor,
            update: InvocationUpdate::Succeeded {
                invocation: self.invocation,
                output: ActionOutput {
                    value: peer.map(|peer| serde_json::json!({ OPENED_PEER_FIELD: peer })),
                    ..ActionOutput::default()
                },
            },
        }
    }

    fn failed(self, code: &str, message: String) -> Opened {
        Opened {
            job: self.job,
            actor: self.actor,
            update: InvocationUpdate::Failed {
                invocation: self.invocation,
                error: ActionFailure {
                    code: code.into(),
                    message,
                    details: None,
                    retryable: false,
                },
            },
        }
    }
}

impl WorkspaceOpens {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Opening>> {
        self.0.lock_recover()
    }

    /// The `open` waiting for `peer` to publish, taken.
    fn take_peer(&self, peer: u64) -> Option<Opening> {
        let mut openings = self.lock();
        let index = openings
            .iter()
            .position(|opening| opening.peer == Some(peer))?;
        Some(openings.swap_remove(index))
    }

    /// The earliest moment an `open` gives up waiting.
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.lock().iter().map(|opening| opening.deadline).min()
    }
}

impl CapnpBrokerState {
    /// Record job `job`'s `open` of `app_id` in `room`: done at once when
    /// the host cannot say what it started, else accepted until the
    /// instance holding the launch's ticket connects and publishes.
    pub(super) fn record_launch(
        &self,
        (job, invocation): (u64, InvocationId),
        actor: &str,
        (app_id, room): (&str, &str),
        launched: Launched,
    ) {
        let owner = || InvocationOwner::Local {
            actor: actor.to_owned(),
        };
        let Launched::Started { ticket } = launched else {
            self.record_completion(
                job,
                owner(),
                InvocationUpdate::Succeeded {
                    invocation,
                    output: ActionOutput::default(),
                },
            );
            return;
        };
        self.record_completion(job, owner(), InvocationUpdate::Accepted { invocation });
        let deadline = Instant::now()
            .checked_add(CONNECT_WAIT)
            .unwrap_or_else(Instant::now);
        self.workspace_opens.lock().push(Opening {
            job,
            actor: actor.to_owned(),
            invocation,
            ticket,
            app_id: app_id.to_owned(),
            room: room.to_owned(),
            peer: None,
            deadline,
        });
        // The accept loop sleeps until its next deadline, which is now
        // this open's.
        self.local_turn.notify_one();
    }

    /// Connection `peer` was authenticated as `launch`: the `open` holding
    /// its ticket, for exactly its app and room, now waits for it to publish.
    pub(super) fn launch_connected(&self, peer: u64, launch: &ConnectedLaunch) {
        let mut openings = self.workspace_opens.lock();
        let Some(opening) = openings.iter_mut().find(|opening| {
            opening.peer.is_none()
                && opening.ticket == launch.ticket
                && opening.app_id == launch.app_id
                && opening.room == launch.room
        }) else {
            return;
        };
        opening.peer = Some(peer);
        opening.deadline = Instant::now()
            .checked_add(PUBLISH_WAIT)
            .unwrap_or_else(Instant::now);
        drop(openings);
        self.local_turn.notify_one();
    }

    /// `peer` published what it offers; the `open` that started it is done.
    pub(super) fn launch_published(&self, peer: u64) {
        if let Some(opening) = self.workspace_opens.take_peer(peer) {
            self.complete_open(opening.opened(Some(peer)));
        }
    }

    /// `peer` disconnected before it published: its `open` failed.
    pub(super) fn launch_disconnected(&self, peer: u64) {
        if let Some(opening) = self.workspace_opens.take_peer(peer) {
            let message = format!(
                "{} exited before it offered anything to open",
                opening.app_id
            );
            self.complete_open(opening.failed("application_exited", message));
        }
    }

    /// Give up on every `open` past its deadline at `now`. An instance that
    /// connected but published nothing is still the one it started; one
    /// that never connected is named by none, rather than by another
    /// instance of the app.
    pub(super) fn expire_workspace_opens(&self, now: Instant) {
        let expired = {
            let mut openings = self.workspace_opens.lock();
            let (expired, waiting) = std::mem::take(&mut *openings)
                .into_iter()
                .partition::<Vec<_>, _>(|opening| opening.deadline <= now);
            *openings = waiting;
            expired
        };
        for opening in expired {
            let peer = opening.peer;
            self.complete_open(opening.opened(peer));
        }
    }

    fn complete_open(&self, opened: Opened) {
        self.record_completion(
            opened.job,
            InvocationOwner::Local {
                actor: opened.actor,
            },
            opened.update,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::{BrokerActor, BrokerChannels, CapnpBrokerState, FanoutState, PeerPublisher};
    use super::{CONNECT_WAIT, ConnectedLaunch, PUBLISH_WAIT};
    use crate::services::{LaunchContent, LaunchPlace, Launched};
    use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;
    use tilo_actions::InvocationUpdate;

    const APP: &str = "so.tilo.word-processor";

    /// Starts each launch with the next ticket.
    struct Ticketed(Mutex<u64>);

    impl crate::services::LauncherServices for Ticketed {
        fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
            Vec::new()
        }
        fn launch(
            &self,
            _: &str,
            _: &tilo_desktop_model::Workspace,
            _: Option<&str>,
            _: &LaunchPlace,
        ) -> Result<(), String> {
            Ok(())
        }
        fn launch_content(
            &self,
            _: &str,
            _: &tilo_desktop_model::Workspace,
            _: LaunchContent<'_>,
            _: &LaunchPlace,
        ) -> Result<Launched, String> {
            let mut next = self.0.lock().unwrap();
            *next = next.wrapping_add(1);
            Ok(Launched::Started { ticket: *next })
        }
        fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
            false
        }
    }

    fn broker() -> (Arc<CapnpBrokerState>, String) {
        let owner = hex::encode([41; 32]);
        let mut workspace = Workspace::new(WorkspaceId(9), "Opens");
        workspace.set_owner(owner);
        let room = workspace.room.to_string();
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let (state, _bridge_rx) = CapnpBrokerState::new_configured(
            &Arc::new(FanoutState::new()),
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(tree.shared().authority()),
            None,
            None,
            crate::services::HostServices {
                launcher: Some(Arc::new(Ticketed(Mutex::new(0)))),
                ..Default::default()
            },
        );
        (state, room)
    }

    fn open(state: &CapnpBrokerState, room: &str) -> u64 {
        let caller = tilo_actions::remote::Caller::authenticated([41; 32], [42; 32]).in_room(room);
        let (workspace, policy, now) = state.workspaces_for(&caller).pop().unwrap();
        state
            .launch_application(
                (&workspace, &policy, now),
                &BrokerActor::person(hex::encode([41; 32])),
                APP,
                LaunchContent {
                    hash: "",
                    caller: None,
                    author: None,
                },
                &LaunchPlace::Focus,
            )
            .unwrap()
    }

    fn connected(ticket: u64, room: &str) -> ConnectedLaunch {
        ConnectedLaunch {
            ticket,
            app_id: APP.into(),
            room: room.into(),
        }
    }

    /// What a succeeded open answered: the value of its output, null when
    /// it named nothing; `None` while it has not succeeded.
    fn answered(state: &CapnpBrokerState, job: u64) -> Option<serde_json::Value> {
        match state.invocation_status(job)? {
            InvocationUpdate::Succeeded { output, .. } => {
                Some(output.value.unwrap_or(serde_json::Value::Null))
            }
            _ => None,
        }
    }

    fn naming(peer: u64) -> serde_json::Value {
        serde_json::json!({ super::OPENED_PEER_FIELD: peer })
    }

    fn accepted(state: &CapnpBrokerState, job: u64) -> bool {
        matches!(
            state.invocation_status(job),
            Some(InvocationUpdate::Accepted { .. })
        )
    }

    #[test]
    fn an_open_completes_with_the_instance_holding_its_ticket() {
        let (state, room) = broker();
        let first = open(&state, &room);
        let second = open(&state, &room);
        assert!(accepted(&state, first) && accepted(&state, second));

        // The second launch's instance comes up first, as peer 5; another
        // app of the room, not started by either, publishes too.
        state.launch_connected(5, &connected(2, &room));
        state.launch_published(4);
        assert!(accepted(&state, first) && accepted(&state, second));
        state.launch_published(5);
        assert_eq!(answered(&state, second), Some(naming(5)));
        assert!(accepted(&state, first), "peer 5 is not the first's");

        // A connection naming the first's ticket for another app or room
        // is not its instance.
        state.launch_connected(
            6,
            &ConnectedLaunch {
                app_id: "so.tilo.sheets".into(),
                ..connected(1, &room)
            },
        );
        state.launch_connected(6, &connected(1, "another-room"));
        state.launch_published(6);
        assert!(accepted(&state, first));
        state.launch_connected(7, &connected(1, &room));
        state.launch_published(7);
        assert_eq!(answered(&state, first), Some(naming(7)));
    }

    #[test]
    fn an_open_never_names_an_instance_it_did_not_start() {
        let (state, room) = broker();
        let silent = open(&state, &room);
        let quiet = open(&state, &room);
        let exits = open(&state, &room);
        state.launch_connected(8, &connected(2, &room));
        state.launch_connected(9, &connected(3, &room));
        state.launch_disconnected(9);
        assert!(matches!(
            state.invocation_status(exits),
            Some(InvocationUpdate::Failed { error, .. }) if error.code == "application_exited"
        ));
        // Peer 10 of the same app connects with no ticket: not the open's.
        state.launch_published(10);

        let now = Instant::now();
        state.expire_workspace_opens(now);
        assert!(accepted(&state, silent) && accepted(&state, quiet));
        state.expire_workspace_opens(now.checked_add(PUBLISH_WAIT).unwrap());
        assert_eq!(
            answered(&state, quiet),
            Some(naming(8)),
            "an instance that publishes nothing is still the one started"
        );
        assert!(accepted(&state, silent));
        state.expire_workspace_opens(now.checked_add(CONNECT_WAIT).unwrap());
        assert_eq!(
            answered(&state, silent),
            Some(serde_json::Value::Null),
            "a launch whose instance never connected names none"
        );
        assert!(state.workspace_opens.next_deadline().is_none());
    }
}
