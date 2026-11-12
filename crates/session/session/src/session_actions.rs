//! The session's own actions, offered to a turn's agent as steps of the broker
//! (`docs/designs/app-language/shell.md` §8.1, "Any action").
//!
//! The agent answering a person's turn arranges the screen as the REPL does: the
//! arrangement's `place` and `place-found`, the undo steps, the windows and the notices.
//! Those are published by the session on its own door, on the loop's thread, and this is
//! the provider its broker answers them under, so one catalogue names them for every caller
//! that has a broker: the agent door, a launched agent, and a task's program alike.
//!
//! Each step is decided where it lands: the broker admits it as the turn's step (the
//! person's grants, the agent's, the issue's), and the session carries it out on the loop's
//! pass, attributed to the agent and the person who asked, as a change the person takes
//! back like any other. The arrangement is `Rc<RefCell<…>>` on the loop's thread and is
//! never shared: its rows are read on the pass and handed here, and an invocation is handed
//! to the session's door ([`SessionSteps`](crate::platform::steps::SessionSteps)) and answered
//! on the pass, as the door's own calls are.
#![cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "the page offers the arrangement and the windows as steps next"
    )
)]

use std::sync::Arc;

use tilo_actions::remote::session_actions as wire;
use tilo_actions::remote::{Pending, pending};
use tilo_actions::{ActionDescriptor, ActionFailure};
use tilo_broker_core::steps::{HostStepOutcome, HostStepRequest, HostStepServices};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};

use crate::platform::steps::SessionSteps;

/// The session's own actions, as the broker's catalogue names them.
pub(crate) struct SessionActions {
    doors: Arc<dyn SessionSteps>,
    /// The rows the session's own door offered on its last pass: what the catalogue lists.
    /// Read on the loop and written here, so a catalogue built on the broker's own thread
    /// never waits on the session, and never reads the arrangement from off its thread.
    rows: std::sync::RwLock<Vec<tilo_actions::door::PlacedAction>>,
    /// Where the door stood when the rows were last read
    /// ([`tilo_session_core::door::Doors::revision`]).
    read_at: std::sync::Mutex<Vec<(usize, u64)>>,
}

impl SessionActions {
    /// The session's actions, whose rows the session hands over on each pass through `doors`.
    pub(crate) fn new(doors: Arc<dyn SessionSteps>) -> Self {
        Self {
            doors,
            rows: std::sync::RwLock::default(),
            read_at: std::sync::Mutex::default(),
        }
    }

    /// Whether the door moved since its rows were last read: equal revisions offer the same rows,
    /// so a pass reads them again only once what a surface offers moved.
    pub(crate) fn door_moved(&self, revision: Vec<(usize, u64)>) -> bool {
        let mut read_at = self.read_at.lock_recover();
        let moved = *read_at != revision;
        *read_at = revision;
        moved
    }

    /// Publish the rows the session's own door offers now, as its pass read them: what the
    /// broker's catalogue lists from here on. Whether they are new: a pass that read the same
    /// rows again changes nothing, and no catalogue is rebuilt for it.
    pub(crate) fn offer(&self, rows: Vec<tilo_actions::door::PlacedAction>) -> bool {
        let mut held = self.rows.write_recover();
        let changed = *held != rows;
        *held = rows;
        changed
    }

    /// The rows offered now, as the last pass read them.
    pub(crate) fn offered(&self) -> Vec<tilo_actions::door::PlacedAction> {
        self.rows.read_recover().clone()
    }
}

impl HostStepServices for SessionActions {
    fn provider(&self) -> &'static str {
        wire::PROVIDER
    }

    fn app_id(&self) -> &'static str {
        wire::PROVIDER
    }

    fn descriptors(&self) -> Vec<ActionDescriptor> {
        self.offered()
            .into_iter()
            .map(|placed| placed.action)
            .collect()
    }

    fn rows(&self) -> Vec<tilo_actions::door::PlacedAction> {
        self.offered()
    }

    /// A turn's step: decided by the turn that asked for it, exactly as an app's action the
    /// same agent takes in it is, and answered for an agent working no turn and for one whose
    /// grants refuse it with the same refusal the app action hears.
    fn for_turns(&self) -> bool {
        true
    }

    fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        let argument = request.argument.map(str::to_owned);
        let (done, asked) = pending();
        self.doors.invoke_step(
            request.actor.id().to_owned(),
            request.address.clone(),
            argument,
            request.run.cloned(),
            done,
        );
        Ok(asked.map(|ran| {
            ran.map_err(|reason| {
                Box::new(ActionFailure {
                    code: "session_step_refused".into(),
                    message: reason,
                    details: None,
                    retryable: false,
                })
            })
            .map(Option::unwrap_or_default)
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tilo_actions::remote::{ActionHostError, Caller, CatalogueQuery, PendingPoll};

    /// A headless session serving its own broker on `plane`, so the session's own actions reach
    /// the catalogue every caller of the desktop is offered.
    fn serving(state: &std::path::Path, plane: &std::path::Path) -> crate::Session {
        crate::Session::headless(
            crate::Options {
                presentation: ui::headless::fixture_runner_inputs,
                collab: false,
                state: Some(state.to_path_buf()),
                action_plane: crate::ActionPlane::At(plane.join("collab.sock")),
                ..crate::Options::default()
            },
            &[tilo_compositor::OutputConfig::new("HEADLESS-1", 1280, 800)],
        )
        .expect("a session")
    }

    /// What `ask` answers, run until the session's pass has answered it: a host step's rows and
    /// its invocation are read on the loop, which steps here until the answer is in.
    fn answered<T: Send + 'static>(
        session: &mut crate::Session,
        ask: impl FnOnce() -> tilo_actions::remote::Pending<Result<T, ActionHostError>>,
    ) -> Result<T, ActionHostError> {
        let (done, mut answered) = tilo_actions::remote::pending();
        ask().then(move |answer| done.complete(answer));
        let deadline = tilo_test_env::hang_deadline();
        while std::time::Instant::now() < deadline {
            session.step(Duration::from_millis(1)).expect("a frame");
            if let PendingPoll::Ready(answer) = answered.poll_ready() {
                return answer.expect("the broker answers");
            }
        }
        Err(ActionHostError::Refused {
            reason: "the session did not answer".to_owned(),
        })
    }

    /// The session's own actions are its broker's: the person at this seat is offered the
    /// arrangement's `place` on the session's own provider, read on the loop's pass and listed
    /// by the broker — one catalogue, whichever door the caller came in by. Its invocation, and
    /// the turn's admission of it, are proven against a real tracker in the broker's own
    /// `a_turn_takes_the_hosts_turn_steps_and_nobody_else_does`.
    #[test]
    fn the_sessions_own_actions_are_the_brokers() {
        use std::os::unix::fs::PermissionsExt as _;

        let _settled = ui::motion::testing::settled();
        let state = tempfile::tempdir().expect("a state directory");
        // The broker binds in a directory only this account reaches.
        let plane = tempfile::tempdir().expect("a directory to serve the broker in");
        std::fs::set_permissions(plane.path(), std::fs::Permissions::from_mode(0o700))
            .expect("the broker's directory is this user's");
        let mut session = serving(state.path(), plane.path());
        session.step(Duration::from_millis(1)).expect("a frame");
        let me = session.shared.person.clone();
        let broker = session
            .action_broker()
            .expect("the session serves a broker");
        let workspace = session
            .handle()
            .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
            .expect("a workspace is in front");
        let person = Caller::local_person(me).in_workspace(workspace);

        let page = answered(&mut session, || {
            broker.action_query(
                &person,
                &CatalogueQuery {
                    provider: Some(tilo_actions::remote::SESSION_ACTIONS_PROVIDER.to_owned()),
                    max_results: 128,
                    ..CatalogueQuery::default()
                },
            )
        })
        .expect("the person sees the catalogue");
        let placing = page
            .rows
            .iter()
            .filter_map(|row| row.action.as_ref())
            .find(|action| action.address.action == "place")
            .expect("the arrangement's place is offered on the broker")
            .clone();
        assert_eq!(
            placing.address.resource,
            crate::arrangement::RESOURCE,
            "offered on the arrangement the session publishes"
        );
        assert_eq!(
            page.rows.first().map(|row| row.app_id.as_str()),
            Some(tilo_actions::remote::SESSION_ACTIONS_PROVIDER),
            "the rows stand under the session's own provider"
        );

        // What the broker lists is what the session publishes: every row names a resource the
        // session's own door holds.
        let published = published(&session);
        for row in &page.rows {
            let action = row.action.as_ref().expect("a row offers its action");
            assert!(
                published.contains(&action.address.resource),
                "the row names a resource the session publishes: {action:?}"
            );
        }
    }

    /// Every resource the session's own door publishes, by its id.
    fn published(session: &crate::Session) -> Vec<String> {
        fn walk(resources: &[tilo_actions::ResourceSpec], out: &mut Vec<String>) {
            for resource in resources {
                out.push(resource.id.as_str().to_owned());
                walk(&resource.children, out);
            }
        }
        let mut out = Vec::new();
        walk(&session.resources(), &mut out);
        out
    }
}
