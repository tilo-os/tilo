//! The session's own actions, offered to a turn's agent as steps of the broker
//! (`docs/designs/app-language/shell.md` §8.1, "Any action").
//!
//! The agent answering a person's turn arranges the screen as the REPL does: the
//! layout's `place` and `place-found`, the undo steps, the windows and the notices.
//! Those are published by the session on its own API, on the loop's thread, and this is
//! the provider its broker answers them under, so one catalogue names them for every caller
//! that has a broker: the MCP server, a launched agent, and a task's program alike.
//!
//! Each step is decided where it lands: the broker admits it as the turn's step (the
//! person's grants, the agent's, the issue's), and the session carries it out on the loop's
//! pass, attributed to the agent and the person who asked, as a change the person takes
//! back like any other. The layout is `Rc<RefCell<…>>` on the loop's thread and is
//! never shared: its rows are read on the pass and handed here, and an invocation is handed
//! to the session's API ([`SessionSteps`](crate::platform::steps::SessionSteps)) and answered
//! on the pass, as the API's own calls are.
//!
//! One step is not on the loop: the person's own tilolang line (`session:programs#run-line`,
//! [`wire::RUN_LINE`]). It runs in the session's program host as the person
//! ([`Programs`]), so it makes no frame wait, and the
//! person may stop it while it runs.
#![cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "the page offers the layout and the windows as steps next"
    )
)]

use std::sync::Arc;

use tilo_actions::remote::admitted::{offered, settled};
use tilo_actions::remote::session_actions as wire;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Admitted, Caller, CatalogueQuery, InvocationReceipt,
    InvokeRequest, Outcome, Pending, pending,
};
use tilo_actions::{ActionDescriptor, ActionFailure, InvocationId};
use tilo_broker_core::BrokerActor;
use tilo_lock::{MutexRecover as _, RwLockRecover as _};
use tilo_workspace_service::workspaces::providers::{Providers, Reach};

use crate::platform::steps::SessionSteps;
use tilo_workspace_service::workspaces::task_programs::{ProgramCaller, ProgramRequest, Programs};

/// The session's own actions, as the broker's catalogue names them.
pub(crate) struct SessionActions {
    steps: Arc<dyn SessionSteps>,
    /// The session's program host: where the person's own line runs, off the loop.
    programs: Option<Arc<dyn Programs>>,
    /// The rows the session's own API offered on its last pass: what the catalogue lists.
    /// Read on the loop and written here, so a catalogue built on the broker's own thread
    /// never waits on the session, and never reads the layout from off its thread.
    rows: std::sync::RwLock<Vec<tilo_actions::api::PlacedAction>>,
    /// Where the API stood when the rows were last read
    /// ([`tilo_session_core::surfaces::Surfaces::revision`]).
    read_at: std::sync::Mutex<Vec<(usize, u64)>>,
    /// How many times the rows changed: the revision of the catalogue the daemon is handed.
    generation: std::sync::atomic::AtomicU64,
    /// Told each time the rows changed, for the daemon to be handed them.
    moved: tokio::sync::Notify,
}

impl SessionActions {
    /// The session's actions, whose rows the session hands over on each pass through `steps`,
    /// and whose person's line runs through `programs`.
    pub(crate) fn new(steps: Arc<dyn SessionSteps>, programs: Option<Arc<dyn Programs>>) -> Self {
        Self {
            steps,
            programs,
            rows: std::sync::RwLock::default(),
            read_at: std::sync::Mutex::default(),
            generation: std::sync::atomic::AtomicU64::new(0),
            moved: tokio::sync::Notify::new(),
        }
    }

    /// Whether the API moved since its rows were last read: equal revisions offer the same rows,
    /// so a pass reads them again only once what a surface offers moved.
    pub(crate) fn rows_moved(&self, revision: Vec<(usize, u64)>) -> bool {
        let mut read_at = self.read_at.lock_recover();
        let moved = *read_at != revision;
        *read_at = revision;
        moved
    }

    /// Publish the rows the session's own API offers now, as its pass read them: what the
    /// broker's catalogue lists from here on. Whether they are new: a pass that read the same
    /// rows again changes nothing, and no catalogue is rebuilt for it.
    pub(crate) fn offer(&self, rows: Vec<tilo_actions::api::PlacedAction>) -> bool {
        let mut held = self.rows.write_recover();
        let changed = *held != rows;
        *held = rows;
        drop(held);
        if changed {
            self.generation
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            self.moved.notify_one();
        }
        changed
    }

    /// The rows offered now, as the last pass read them.
    pub(crate) fn offered(&self) -> Vec<tilo_actions::api::PlacedAction> {
        self.rows.read_recover().clone()
    }

    /// Answer the session's own actions from now on, offered to a turn's agent and admitted by
    /// that turn: the layout, its undo, the windows and the notices.
    pub(crate) fn register(self: Arc<Self>, providers: &Providers) {
        providers.provide(wire::PROVIDER, wire::PROVIDER, Reach::Turns, self);
    }

    /// Serve them on the device's dispatch too, over the shell's link `shell`, for as long as it
    /// lasts: offering the rows the session's last pass read, and the rows again each time a pass
    /// reads other ones.
    #[cfg(unix)]
    pub(crate) fn serve_on(self: &Arc<Self>, shell: runtime_collab::ShellClient) {
        let runtime = match tilo_rt::try_app() {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::warn!(%error, "the device's dispatch reaches none of the session's own actions");
                return;
            }
        };
        let actions = Arc::clone(self);
        runtime.spawn(async move {
            let provided = match shell
                .provide(
                    wire::PROVIDER,
                    &actions.page(),
                    Arc::clone(&actions) as Arc<dyn ActionProvider + Send + Sync>,
                )
                .await
            {
                Ok(provided) => provided,
                Err(error) => {
                    tracing::warn!(%error, "the device's dispatch reaches none of the session's own actions");
                    return;
                }
            };
            loop {
                actions.moved.notified().await;
                if let Err(error) = provided.publish(&actions.page()).await {
                    tracing::debug!(%error, "the daemon is handed the session's actions no more");
                    return;
                }
            }
        });
    }

    /// Every step the session offers now, as the daemon lists them.
    #[cfg(unix)]
    fn page(&self) -> CataloguePage {
        let revision = self
            .generation
            .load(std::sync::atomic::Ordering::Acquire)
            .to_string();
        CataloguePage {
            revision,
            rows: self
                .rows()
                .into_iter()
                .map(|placed| {
                    tilo_actions::remote::CatalogueRow::for_placed_action(
                        wire::PROVIDER,
                        wire::PROVIDER,
                        placed,
                    )
                })
                .collect(),
            exhausted: true,
            ..CataloguePage::default()
        }
    }
}

const LINE: tilo_actions::ActionParam = tilo_actions::ActionParam::required(
    "line",
    tilo_actions::ParamType::Text,
    "The tilolang line to run as you.",
);
const LINE_PARAMS: &[tilo_actions::ActionParam] = &[LINE];

/// The person's own line, described for the broker's catalogue: a background step, which the
/// person may stop while it runs. The line itself only reads: every change it makes is its own
/// invocation, decided where it lands, and credited to the person.
fn line_descriptor() -> ActionDescriptor {
    use tilo_session_core::desktop::i18n::t;
    let mut contract = tilo_actions::ActionContract {
        execution: tilo_actions::ExecutionMode::Background { cancellable: true },
        result: tilo_actions::ResultSpec::Json {
            schema: serde_json::json!({
                "type": "object",
                "required": ["text", "person_text", "failed"],
            }),
        },
        ..tilo_actions::ActionContract::view()
    };
    // A line runs as the person who typed it, never as an agent: agents' listings leave it out.
    contract.set_audience(tilo_actions::Audience::People);
    ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new(wire::PROGRAMS_RESOURCE, wire::RUN_LINE),
        resource_kind: wire::PROGRAMS_RESOURCE.into(),
        resource_label: t("programs-resource"),
        action_label: t("programs-run-line"),
        keywords: Vec::new(),
        icon: None,
        description: Some(t("programs-run-line-detail")),
        locus: tilo_actions::ExecutionLocus::default(),
        kind: tilo_actions::OperationKind::View,
        contract,
        input: tilo_actions::InputSpec::None,
        params: LINE_PARAMS.to_vec(),
        view: None,
    }
}

/// The line `slot` carries, as the broker sent it.
fn line_of(slot: Option<&str>) -> Result<String, String> {
    let Some(slot) = slot else {
        return Err("a line takes its text: `line`".into());
    };
    let json = slot
        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
        .ok_or("a line takes its arguments by name")?;
    let arguments: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(json).map_err(|error| error.to_string())?;
    arguments
        .get("line")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "a line takes its text: `line`".into())
}

impl SessionActions {
    /// Every step the session offers, as the row it stands in.
    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        let mut rows = self.offered();
        rows.push(tilo_actions::api::PlacedAction {
            parent: String::new(),
            has_children: false,
            action: line_descriptor(),
        });
        rows
    }
}

impl tilo_actions::remote::Admits for SessionActions {
    const PROVIDER: &'static str = wire::PROVIDER;
    const APP: &'static str = wire::PROVIDER;

    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        self.rows()
    }

    /// Carry out an invocation the device's dispatch admitted for `caller`: a turn's step,
    /// decided by the turn that asked for it, exactly as an app's action the same agent takes in
    /// it is, and answered for an agent working no turn and for one whose grants refuse it with
    /// the same refusal the app action hears.
    fn start(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        if request.target.address.resource == wire::PROGRAMS_RESOURCE
            && request.target.address.action == wire::RUN_LINE
        {
            return self.start_line(caller, request, admitted);
        }
        let actor = BrokerActor::for_caller(caller);
        let argument = request.argument.to_slot();
        let (done, asked) = pending();
        // The place the ask is in: an agent's turn is the conversation the person wrote in, and
        // a place it opens stands beside that conversation
        // (`docs/designs/app-language/one-app-model.md` §3). The person's own step stands beside
        // the place in front.
        let beside = actor
            .turn()
            .map(|turn| tilo_session_core::layout::turn_key(&turn.conversation, &turn.message))
            .unwrap_or_default();
        self.steps.invoke_step(
            actor.member().clone().into(),
            request.target.address.clone(),
            argument,
            beside,
            request
                .context
                .as_ref()
                .and_then(|context| context.group.clone()),
            done,
        );
        Ok(asked.map(|ran| {
            ran.map_err(|error| {
                Box::new(ActionFailure {
                    code: error.code().to_owned(),
                    message: error.to_string(),
                    details: None,
                    retryable: false,
                })
            })
            .map(Option::unwrap_or_default)
        }))
    }
}

impl SessionActions {
    /// Run the person's own line: off the loop, in the session's program host, and as them.
    fn start_line(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        let actor = BrokerActor::for_caller(caller);
        if actor.is_agent() {
            return Err("a line runs as the person at this device, never as an agent".into());
        }
        let Some(person) = actor.member().person_id() else {
            return Err("a line runs as the person at this device".into());
        };
        let argument = request.argument.to_slot();
        let program = line_of(argument.as_deref())?;
        let Some(programs) = &self.programs else {
            return Err("this desktop runs no tilolang programs".into());
        };
        let workspace = caller
            .workspace()
            .ok_or("a line runs in no workspace of this device")?;
        programs.run(ProgramRequest {
            workspace,
            provider: None,
            caller: ProgramCaller::Person {
                person,
                session: None,
            },
            group: request
                .context
                .as_ref()
                .and_then(|context| context.group.clone())
                .unwrap_or_else(|| tilo_actions::WorkGroup {
                    id: wire::PROGRAMS_RESOURCE.to_owned(),
                    label: None,
                    // The person's own REPL, as their API's program sessions are: what the line
                    // does is grouped there, and nothing an agent did is.
                    kind: "api".into(),
                    declared_id: None,
                    session: None,
                }),
            program,
            confirm: false,
            this: None,
            stop: Some(Arc::clone(&admitted.stop)),
            asked_in: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tilo_actions::remote::{ActionCallError, Caller, CatalogueQuery, PendingPoll};

    /// A headless session serving its own broker on `app_wire`, so the session's own actions reach
    /// the catalogue every caller of the desktop is offered.
    fn serving(state: &std::path::Path, app_wire: &std::path::Path) -> crate::Session {
        crate::Session::headless(
            crate::Options {
                presentation: ui::headless::fixture_runner_inputs,
                collab: false,
                state: Some(state.to_path_buf()),
                app_socket: crate::AppSocket::At(app_wire.join("collab.sock")),
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
        ask: impl FnOnce() -> tilo_actions::remote::Pending<Result<T, ActionCallError>>,
    ) -> Result<T, ActionCallError> {
        let (done, mut answered) = tilo_actions::remote::pending();
        ask().then(move |answer| done.complete(answer));
        let deadline = tilo_test_env::hang_deadline();
        while std::time::Instant::now() < deadline {
            session.step(Duration::from_millis(1)).expect("a frame");
            if let PendingPoll::Ready(answer) = answered.poll_ready() {
                return answer.expect("the broker answers");
            }
        }
        Err(ActionCallError::Refused {
            reason: "the session did not answer".to_owned(),
        })
    }

    /// The line `line`, as a composer asks it: the session's own provider, running it as the
    /// person who typed it.
    #[cfg(target_os = "linux")]
    fn run_line(line: &str) -> tilo_actions::remote::InvokeRequest {
        tilo_actions::remote::InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            on_behalf_of: None,
            target: tilo_actions::remote::session_actions::run_line_target(),
            argument: tilo_actions::remote::InvokeArgument::Json(serde_json::Map::from_iter([(
                "line".to_owned(),
                serde_json::json!(line),
            )])),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    /// What the line `request` answered as `person`, running the session until the invocation
    /// ends: its output, or why it ended otherwise.
    #[cfg(target_os = "linux")]
    fn ended_line(
        session: &mut crate::Session,
        person: &Caller,
        request: &tilo_actions::remote::InvokeRequest,
    ) -> Result<serde_json::Value, ActionCallError> {
        let broker = session
            .action_broker()
            .expect("the session serves its actions");
        answered(session, || {
            broker.action_invoke_local_person(person, request)
        })
        .expect("the broker admits the person's line");
        let deadline = tilo_test_env::hang_deadline();
        while std::time::Instant::now() < deadline {
            session.step(Duration::from_millis(1)).expect("a frame");
            if let Some(state) = broker.action_observe(person, &request.invocation)
                && state.update.is_terminal()
            {
                return match state.update {
                    tilo_actions::InvocationUpdate::Succeeded { output, .. } => {
                        Ok(output.value.unwrap_or_default())
                    }
                    tilo_actions::InvocationUpdate::Failed { error, .. } => {
                        Err(ActionCallError::Refused {
                            reason: error.message,
                        })
                    }
                    tilo_actions::InvocationUpdate::Cancelled { message, .. } => {
                        Err(ActionCallError::Refused {
                            reason: message.unwrap_or_else(|| "stopped".to_owned()),
                        })
                    }
                    _ => Err(ActionCallError::Unanswered),
                };
            }
        }
        Err(ActionCallError::Refused {
            reason: "the line never ended".to_owned(),
        })
    }

    /// A headless session whose workspace the person at this device governs, with its own broker:
    /// the shell's own actions are enough for a line to change something.
    #[cfg(target_os = "linux")]
    fn line_session(
        state: &std::path::Path,
        app_wire: &std::path::Path,
        person: &str,
        workspace: &str,
    ) -> crate::Session {
        let mut session = power_session(state, app_wire, person, workspace, Vec::new());
        for _ in 0..4 {
            session.step(Duration::from_millis(1)).expect("a frame");
        }
        session
    }

    /// The person's own line runs as them: what it changes lands, and History credits it to the
    /// person, never an agent.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_line_runs_as_the_person_and_what_it_changes_is_theirs() {
        const WORKSPACE: &str = "3b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5f";
        let line =
            "session.place_found(text: \"Displays\", view: .none, x: .some(40), y: .some(40))";
        let _settled = ui::motion::testing::settled();
        let state = tempfile::tempdir().expect("a state directory");
        let (_env, _identity_root, person_id) = governed_workspace(state.path(), WORKSPACE);
        let app_wire = tempfile::tempdir().expect("a directory to serve the broker in");
        let mut session = line_session(state.path(), app_wire.path(), &person_id, WORKSPACE);
        let person = Caller::local_person(person_id.clone()).in_workspace(WORKSPACE);

        // A shell whose layout was never stored opens its record with the first change to it:
        // the layout as it was opened is no step of anyone's, as a layout opened from the store's
        // is none (`crates/desktop/session-core/src/layout/document.rs`, `acted`).
        let opening = ended_line(&mut session, &person, &run_line(line))
            .expect("the line runs as the person");
        assert_eq!(opening["failed"], serde_json::json!(false), "{opening}");
        assert!(
            !session.layout().borrow_mut().undo(&person_id),
            "the layout as it was opened is no step of the person's: {opening}"
        );

        // The line's next change is a step of the layout's, and the step is the person's: their
        // own invocation, through their own api, so History credits them and they take it back.
        let answer = ended_line(&mut session, &person, &run_line(line))
            .expect("the line runs as the person");
        assert_eq!(answer["failed"], serde_json::json!(false), "{answer}");
        assert_eq!(
            answer["answer"]["changed"],
            serde_json::json!(1),
            "the line changed the layout: {answer}"
        );
        assert_eq!(
            answer["answer"]["history"][0]["resource"],
            serde_json::json!("shell:layout"),
            "the change it recorded is the layout's: {answer}"
        );
        assert!(
            session.layout().borrow_mut().undo(&person_id),
            "the person takes back what the line did: {answer}"
        );
    }

    /// A line that does not parse is answered with where it went wrong, never run.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_line_that_does_not_parse_is_answered_with_its_place() {
        const WORKSPACE: &str = "4b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b50";
        let _settled = ui::motion::testing::settled();
        let state = tempfile::tempdir().expect("a state directory");
        let (_env, _identity_root, person_id) = governed_workspace(state.path(), WORKSPACE);
        let app_wire = tempfile::tempdir().expect("a directory to serve the broker in");
        let mut session = line_session(state.path(), app_wire.path(), &person_id, WORKSPACE);
        let person = Caller::local_person(person_id).in_workspace(WORKSPACE);

        let answer = ended_line(&mut session, &person, &run_line("session.new_strip("))
            .expect("a line that does not parse is still an answer");
        assert_eq!(answer["failed"], serde_json::json!(true), "{answer}");
        let said = answer["person_text"]
            .as_str()
            .filter(|said| !said.is_empty())
            .or_else(|| answer["text"].as_str())
            .unwrap_or_default();
        assert!(
            said.contains("1:"),
            "the answer names the line and column it went wrong at: {answer}"
        );
    }

    /// A person who is no member of the workspace cannot have a line run there, and hears the
    /// system's word for it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_line_in_a_workspace_the_person_is_no_member_of_is_refused() {
        const WORKSPACE: &str = "5b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b51";
        let _settled = ui::motion::testing::settled();
        let state = tempfile::tempdir().expect("a state directory");
        let (_env, _identity_root, person_id) = governed_workspace(state.path(), WORKSPACE);
        let app_wire = tempfile::tempdir().expect("a directory to serve the broker in");
        let mut session = line_session(state.path(), app_wire.path(), &person_id, WORKSPACE);
        let stranger =
            Caller::local_person("a-person-this-workspace-does-not-know").in_workspace(WORKSPACE);

        let broker = session
            .action_broker()
            .expect("the session serves its actions");
        let refused = answered(&mut session, || {
            broker.action_invoke(&stranger, None, None, &run_line("session.new_strip()"))
        });
        assert!(
            matches!(
                refused,
                Err(ActionCallError::NotPermitted | ActionCallError::Refused { .. })
            ),
            "the line is refused in the system's words: {refused:?}"
        );
    }

    /// A line the person stops while it runs makes no change and reads cancelled.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_line_the_person_stops_ends_cancelled_and_makes_no_change() {
        const WORKSPACE: &str = "6b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b52";
        let _settled = ui::motion::testing::settled();
        let state = tempfile::tempdir().expect("a state directory");
        let (_env, _identity_root, person_id) = governed_workspace(state.path(), WORKSPACE);
        let app_wire = tempfile::tempdir().expect("a directory to serve the broker in");
        let mut session = line_session(state.path(), app_wire.path(), &person_id, WORKSPACE);
        let person = Caller::local_person(person_id).in_workspace(WORKSPACE);
        let strips = session.layout().borrow().reading().strips.len();
        let broker = session
            .action_broker()
            .expect("the session serves its actions");
        let request = run_line("session.new_strip(name: \"Never\")");

        // The line's calls are the session's own, which run on its pass: it has made none yet.
        answered(&mut session, || {
            broker.action_invoke_local_person(&person, &request)
        })
        .expect("the broker admits the person's line");
        answered(&mut session, || {
            broker.action_cancel(&person, &request.invocation)
        })
        .expect("the person stops the line");

        let deadline = tilo_test_env::hang_deadline();
        let mut ended = None;
        while std::time::Instant::now() < deadline {
            session.step(Duration::from_millis(1)).expect("a frame");
            if let Some(state) = broker.action_observe(&person, &request.invocation)
                && state.update.is_terminal()
            {
                ended = Some(state.update);
                break;
            }
        }
        assert!(
            matches!(
                ended,
                Some(tilo_actions::InvocationUpdate::Cancelled { .. })
            ),
            "a stopped line reads cancelled: {ended:?}"
        );
        assert_eq!(
            session.layout().borrow().reading().strips.len(),
            strips,
            "the stopped line made no change"
        );
    }

    /// The session's own actions are its broker's: the person at this device is offered the
    /// layout's `place` on the session's own provider, read on the loop's pass and listed
    /// by the broker — one catalogue, whichever API the caller came in by. Its invocation, and
    /// the turn's admission of it, are proven against a real tracker in the broker's own
    /// `a_turn_takes_the_hosts_turn_steps_and_nobody_else_does`.
    #[test]
    fn the_sessions_own_actions_are_the_brokers() {
        use std::os::unix::fs::PermissionsExt as _;

        let _settled = ui::motion::testing::settled();
        let state = tempfile::tempdir().expect("a state directory");
        // The broker binds in a directory only this account reaches.
        let app_wire = tempfile::tempdir().expect("a directory to serve the broker in");
        std::fs::set_permissions(app_wire.path(), std::fs::Permissions::from_mode(0o700))
            .expect("the broker's directory is this user's");
        let mut session = serving(state.path(), app_wire.path());
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
            .expect("the layout's place is offered on the broker")
            .clone();
        assert_eq!(
            placing.address.resource,
            tilo_session_core::layout::RESOURCE,
            "offered on the layout the session publishes"
        );
        assert_eq!(
            page.rows.first().map(|row| row.app_id.as_str()),
            Some(tilo_actions::remote::SESSION_ACTIONS_PROVIDER),
            "the rows stand under the session's own provider"
        );

        // What the broker lists is what the session publishes: every row names a resource the
        // session's own API holds, but the person's line, which is a host step of the session's
        // own and not a resource its services read.
        let published = published(&session);
        for row in &page.rows {
            let action = row.action.as_ref().expect("a row offers its action");
            assert!(
                published.contains(&action.address.resource)
                    || action.address.resource
                        == tilo_actions::remote::session_actions::PROGRAMS_RESOURCE,
                "the row names a resource the session publishes: {action:?}"
            );
        }
    }

    #[cfg(target_os = "linux")]
    struct FakeSystem {
        power: std::sync::Mutex<tilo_system_service::model::Power>,
        power_reads: std::sync::atomic::AtomicUsize,
        refusal: std::sync::Mutex<Option<String>>,
        changes: std::sync::Mutex<Vec<tilo_system_service::device::Change>>,
    }

    #[cfg(target_os = "linux")]
    impl FakeSystem {
        fn new() -> std::sync::Arc<Self> {
            use tilo_system_service::model::{Power, PowerMode};
            std::sync::Arc::new(Self {
                power: std::sync::Mutex::new(Power {
                    modes: vec![PowerMode::Saver, PowerMode::Balanced],
                    mode: Some(PowerMode::Balanced),
                    ..Power::default()
                }),
                power_reads: std::sync::atomic::AtomicUsize::new(0),
                refusal: std::sync::Mutex::new(None),
                changes: std::sync::Mutex::new(Vec::new()),
            })
        }

        fn changes(&self) -> Vec<tilo_system_service::device::Change> {
            self.changes.lock().expect("changes lock").clone()
        }

        fn power_reads(&self) -> usize {
            self.power_reads.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[cfg(target_os = "linux")]
    impl tilo_system_service::device::System for FakeSystem {
        fn power(&self) -> tilo_system_service::model::Power {
            self.power_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.power.lock().expect("power lock").clone()
        }

        fn power_mode(&self) -> Option<tilo_system_service::model::PowerMode> {
            self.power.lock().expect("power lock").mode
        }

        fn user_switching(&self) -> bool {
            self.power.lock().expect("power lock").user_switching
        }

        fn update(&self) -> tilo_system_service::update::Update {
            tilo_system_service::update::Update::default()
        }

        fn time(&self) -> tilo_system_service::model::Time {
            tilo_system_service::model::Time::default()
        }

        fn keyboard_light(&self) -> tilo_system_service::model::KeyboardLight {
            tilo_system_service::model::KeyboardLight::default()
        }

        fn change(&self, change: &tilo_system_service::device::Change) -> Result<(), String> {
            self.changes
                .lock()
                .expect("changes lock")
                .push(change.clone());
            if let tilo_system_service::device::Change::PowerMode(mode) = change {
                let refusal = self.refusal.lock().expect("refusal lock").clone();
                if let Some(reason) = refusal {
                    return Err(reason);
                }
                self.power.lock().expect("power lock").mode = Some(*mode);
            }
            Ok(())
        }

        fn locate(&self) -> Result<settings_store::day_schedule::Location, String> {
            Err("no fix".into())
        }

        fn nearest_zone(
            &self,
            _location: settings_store::day_schedule::Location,
        ) -> Option<String> {
            None
        }
    }

    /// Build a session-action invoke request for `(resource, action)` with `arguments`.
    #[cfg(target_os = "linux")]
    fn session_step(
        resource: &str,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> tilo_actions::remote::InvokeRequest {
        tilo_actions::remote::InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            on_behalf_of: None,
            target: tilo_actions::remote::RemoteActionTarget {
                provider: tilo_actions::remote::SESSION_ACTIONS_PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(resource, action),
            },
            argument: tilo_actions::remote::InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    /// Give the session a real device identity and govern `workspace` under it so the broker
    /// admits the person's and their agent's calls on this device.
    #[cfg(target_os = "linux")]
    fn governed_workspace(
        state: &std::path::Path,
        workspace: &str,
    ) -> (tilo_test_env::ScopedEnv, tempfile::TempDir, String) {
        let root = tempfile::tempdir().expect("an identity directory");
        let xdg = root.path().join("config");
        let system = root.path().join("system");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", &xdg)
            .set("XDG_DATA_HOME", root.path().join("data"))
            .set("TILO_IDENTITY_ROOT", &system)
            .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
        let paths = tilo_device_identity::IdentityPaths::under(&system);
        tilo_identity::provisioning::ensure_collaboration_identity(
            &xdg.join("tilo/identity"),
            &paths,
            tilo_identity::provisioning::IdentityStorage::Files,
        )
        .expect("this computer's identity");
        let device =
            tilo_device_identity::persist::load_device_key(&paths).expect("this device's key");
        let cert = tilo_device_identity::persist::read_device_cert(&paths.device_cert)
            .expect("this device's certificate");
        let owner = tilo_peer_state::owner::open_file(
            &tilo_session_client::engine::owner_path(state).expect("this device's state directory"),
        )
        .expect("an owner for this device's workspace");
        let workspace_id =
            tilo_desktop_model::WorkspaceId::parse(workspace).expect("a workspace id");
        let governor = tilo_peer_state::owner::testing::enroll_interactive_in(
            &owner, &device, &cert, workspace,
        )
        .expect("this device governs the workspace it hosts");
        let host = governor.device().device_public_key;
        let frames: Vec<Vec<u8>> = governor.outgoing().map(|(bytes, _)| bytes).collect();
        for frame in frames {
            owner
                .receive(
                    workspace_id,
                    tilo_peer_state::TransportOrigin::RetainedServerDelta(host),
                    postcard::from_bytes(frame.as_slice()).expect("a publication is a message"),
                    std::time::SystemTime::now(),
                )
                .expect("the host admits the workspace's people");
        }
        (env, root, hex::encode(cert.user_public_key))
    }

    /// Step `session` until `power` has no pending action left and return its reading.
    #[cfg(target_os = "linux")]
    fn settled_power(session: &mut crate::Session) -> serde_json::Value {
        let deadline = tilo_test_env::hang_deadline();
        while std::time::Instant::now() < deadline {
            session.step(Duration::from_millis(1)).expect("a frame");
            let reading = session
                .read(tilo_system_service::POWER)
                .expect("power reading");
            if reading["pending"] == serde_json::json!([]) {
                return reading;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("power action never settled")
    }

    /// Start a headless session mounted with `services` and a governed workspace where
    /// `person_id` owns the workspace and agent `scribe` is allowed to read and act.
    #[cfg(target_os = "linux")]
    fn power_session(
        state: &std::path::Path,
        app_wire: &std::path::Path,
        person_id: &str,
        workspace_id: &str,
        services: Vec<Box<dyn tilo_service::Service>>,
    ) -> crate::Session {
        use std::os::unix::fs::PermissionsExt as _;

        std::fs::set_permissions(app_wire, std::fs::Permissions::from_mode(0o700))
            .expect("the actions directory is this user's");
        let mut session = crate::Session::headless(
            crate::Options {
                presentation: ui::headless::fixture_runner_inputs,
                services,
                person: person_id.to_owned(),
                collab: false,
                state: Some(state.to_path_buf()),
                app_socket: crate::AppSocket::At(app_wire.join("collab.sock")),
                ..crate::Options::default()
            },
            &[tilo_compositor::OutputConfig::new("HEADLESS-1", 1280, 800)],
        )
        .expect("a session");
        let id = tilo_desktop_model::WorkspaceId::parse(workspace_id).expect("workspace id");
        let mut workspace = tilo_desktop_model::Workspace::new(id, "Shell");
        workspace.set_owner(person_id.to_owned());
        workspace.policy.grants.push(tilo_action_policy::Grant::new(
            "agent-member:scribe:session",
            tilo_action_policy::Grantee::agent("scribe"),
            workspace.node(),
            tilo_action_policy::KindSet::ALL,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person(person_id.to_owned()),
        ));
        session.handle().replace_tree(tilo_desktop_model::Layout {
            active: workspace.id(),
            workspaces: vec![workspace],
            ..tilo_desktop_model::Layout::default()
        });
        for _ in 0..4 {
            session.step(Duration::from_millis(1)).expect("a frame");
        }
        session
    }

    /// Every power-profile or power-mode action offered to agent `scribe` in `session`'s catalogue.
    #[cfg(target_os = "linux")]
    fn agent_profile_actions(
        session: &mut crate::Session,
        person_id: &str,
        workspace_id: &str,
    ) -> Vec<(String, String, String)> {
        let actions = session
            .action_broker()
            .expect("the session serves its actions");
        let agent = Caller::local_person(person_id.to_owned())
            .in_workspace(workspace_id)
            .delegated_to("scribe")
            .in_new_execution();
        let page = answered(session, || {
            actions.action_query(
                &agent,
                &CatalogueQuery {
                    provider: Some(tilo_actions::remote::SESSION_ACTIONS_PROVIDER.to_owned()),
                    max_results: 512,
                    ..CatalogueQuery::default()
                },
            )
        })
        .expect("the agent sees the session's catalogue");
        page.rows
            .into_iter()
            .filter_map(|row| row.action)
            .filter(|action| {
                (action.address.resource == tilo_system_service::POWER
                    || action.address.resource.starts_with("power:"))
                    && (action.address.action.contains("mode")
                        || action.address.action.contains("profile"))
            })
            .map(|action| {
                (
                    action.address.resource,
                    action.address.action,
                    action.resource_kind,
                )
            })
            .collect()
    }

    /// Build a one-field JSON argument map for a session action call.
    #[cfg(target_os = "linux")]
    fn single_arg(key: &str, value: &str) -> serde_json::Map<String, serde_json::Value> {
        let mut map = serde_json::Map::new();
        map.insert(key.to_owned(), serde_json::json!(value));
        map
    }

    /// Build a settled `SystemService` backed by a fresh `FakeSystem` test double.
    #[cfg(target_os = "linux")]
    fn fake_system_service() -> (std::sync::Arc<FakeSystem>, Box<dyn tilo_service::Service>) {
        let fake = FakeSystem::new();
        let mut service = tilo_system_service::SystemService::new(
            std::sync::Arc::clone(&fake) as std::sync::Arc<dyn tilo_system_service::device::System>,
            None,
        )
        .expect("system");
        tilo_service::Service::settle(&mut service);
        (fake, Box::new(service))
    }

    /// Invoke a session action on the session's catalogue provider as `person`.
    #[cfg(target_os = "linux")]
    fn invoke_session_step(
        session: &mut crate::Session,
        person: &Caller,
        resource: &str,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_actions::remote::InvocationReceipt, ActionCallError> {
        let actions = session
            .action_broker()
            .expect("the session serves its actions");
        answered(session, || {
            actions.action_invoke_local_person(person, &session_step(resource, action, arguments))
        })
    }

    /// Build a `PowerLink` populated with the three standard power-profiles-daemon profiles.
    #[cfg(target_os = "linux")]
    fn populated_power_link() -> tilo_session_power::PowerLink {
        let link = tilo_session_power::PowerLink::default();
        link.bridge().note_profiles(vec![
            "performance".into(),
            "balanced".into(),
            "power-saver".into(),
        ]);
        link.bridge().note_active_profile("balanced");
        link
    }

    /// With both the power service and the system service mounted, the session's own
    /// provider publishes exactly one power-profile action on `power` / `power:*`:
    /// `power#set-power-mode`. Invoking it changes the profile through the system service's
    /// test double, and a device refusal surfaces in `problem`.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_session_publishes_one_power_profile_action_and_invoking_it_switches_the_profile() {
        use tilo_system_service::device::Change;
        use tilo_system_service::model::PowerMode;

        const WORKSPACE: &str = "1b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5d";
        let _settled = ui::motion::testing::settled();
        let state = tempfile::tempdir().expect("a state directory");
        let (_env, _identity_root, person_id) = governed_workspace(state.path(), WORKSPACE);
        let app_wire = tempfile::tempdir().expect("a directory to serve actions in");
        let power_link = populated_power_link();
        let power_service =
            tilo_session_power::SessionPower::without_daemons(power_link.clone()).expect("power");
        let (fake_system, system_service) = fake_system_service();
        let mut session = power_session(
            state.path(),
            app_wire.path(),
            &person_id,
            WORKSPACE,
            vec![Box::new(power_service), system_service],
        );
        assert_eq!(
            agent_profile_actions(&mut session, &person_id, WORKSPACE),
            vec![(
                "power".to_owned(),
                "set-power-mode".to_owned(),
                "so.tilo.system.power".to_owned(),
            )],
            "the agent's catalogue offers exactly one power-profile action"
        );
        let person = Caller::local_person(person_id).in_workspace(WORKSPACE);

        // The retired `power:sleep#set-profile` name is gone: no alias, no forwarding.
        let retired = single_arg("profile", "power-saver");
        assert_eq!(
            session.invoke(tilo_session_power::SLEEP, "set-profile", &retired),
            Err(tilo_actions::ApiError::Unknown(
                tilo_actions::ActionAddress::new(tilo_session_power::SLEEP, "set-profile"),
            )),
            "power:sleep#set-profile is unknown on Session::invoke"
        );
        assert_eq!(
            invoke_session_step(
                &mut session,
                &person,
                tilo_session_power::SLEEP,
                "set-profile",
                retired,
            ),
            Err(ActionCallError::UnknownAction),
            "power:sleep#set-profile is unknown on the catalogue provider"
        );
        assert!(power_link.bridge().drain_commands().is_empty());

        // Invoking the one published action switches the profile through the system service.
        invoke_session_step(
            &mut session,
            &person,
            tilo_system_service::POWER,
            "set-power-mode",
            single_arg("mode", "saver"),
        )
        .expect("invoking power#set-power-mode succeeds");
        let power = settled_power(&mut session);
        assert_eq!(power["mode"], serde_json::json!("saver"));
        assert_eq!(power["problem"], serde_json::Value::Null);
        assert_eq!(
            fake_system.changes(),
            vec![Change::PowerMode(PowerMode::Saver)]
        );
        assert!(power_link.bridge().drain_commands().is_empty());

        // A refusal from the device surfaces in `problem` and leaves the mode unchanged.
        *fake_system.refusal.lock().expect("refusal lock") =
            Some("power-profiles-daemon denied the switch".into());
        invoke_session_step(
            &mut session,
            &person,
            tilo_system_service::POWER,
            "set-power-mode",
            single_arg("mode", "balanced"),
        )
        .expect("the refusal lands asynchronously on the resource");
        let power = settled_power(&mut session);
        assert_eq!(power["mode"], serde_json::json!("saver"));
        assert_eq!(
            power["problem"],
            serde_json::json!("power-profiles-daemon denied the switch")
        );

        // An unoffered mode is rejected synchronously with its exact message.
        assert_eq!(
            session.invoke(
                tilo_system_service::POWER,
                "set-power-mode",
                &single_arg("mode", "performance"),
            ),
            Err(tilo_actions::ApiError::InvalidArguments(
                "this device offers no power mode performance".to_owned()
            ))
        );
    }

    /// With only the system service mounted (no power service / no `PowerLink`),
    /// `power#set-power-mode` is still offered under the session's provider and switches
    /// the profile through the system service's test double.
    #[cfg(target_os = "linux")]
    #[test]
    fn without_the_power_service_the_session_still_offers_and_runs_set_power_mode() {
        use tilo_system_service::device::Change;
        use tilo_system_service::model::PowerMode;

        const WORKSPACE: &str = "2b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5e";

        let _settled = ui::motion::testing::settled();
        let state = tempfile::tempdir().expect("a state directory");
        let (_env, _identity_root, person_id) = governed_workspace(state.path(), WORKSPACE);
        let app_wire = tempfile::tempdir().expect("a directory to serve headless actions in");
        let (fake_system, system_service) = fake_system_service();

        let mut session = power_session(
            state.path(),
            app_wire.path(),
            &person_id,
            WORKSPACE,
            vec![system_service],
        );
        assert_eq!(
            agent_profile_actions(&mut session, &person_id, WORKSPACE),
            vec![(
                "power".to_owned(),
                "set-power-mode".to_owned(),
                "so.tilo.system.power".to_owned(),
            )],
            "headless start still offers power#set-power-mode"
        );

        let person = Caller::local_person(person_id).in_workspace(WORKSPACE);
        invoke_session_step(
            &mut session,
            &person,
            tilo_system_service::POWER,
            "set-power-mode",
            single_arg("mode", "saver"),
        )
        .expect("invoking power#set-power-mode in headless session succeeds");
        let power = settled_power(&mut session);
        assert_eq!(power["mode"], serde_json::json!("saver"));
        assert_eq!(
            fake_system.changes(),
            vec![Change::PowerMode(PowerMode::Saver)]
        );
    }

    /// Mount both `SessionPower` and `SystemService` wired through a shared `PowerLink`.
    #[cfg(target_os = "linux")]
    fn live_power_session(
        state: &std::path::Path,
        app_wire: &std::path::Path,
        person_id: &str,
        workspace_id: &str,
        fake_system: &std::sync::Arc<FakeSystem>,
        profiles: Vec<String>,
        active: &str,
    ) -> (tilo_session_power::PowerLink, crate::Session) {
        let power_link = tilo_session_power::PowerLink::default();
        power_link.attached();
        power_link.bridge().note_profiles(profiles);
        power_link.bridge().note_active_profile(active);
        let power_service =
            tilo_session_power::SessionPower::without_daemons(power_link.clone()).expect("power");
        let mut system_service = crate::power::system_service(
            std::sync::Arc::clone(fake_system)
                as std::sync::Arc<dyn tilo_system_service::device::System>,
            None,
            Some(&power_link),
        )
        .expect("system");
        tilo_service::Service::settle(&mut system_service);
        let session = power_session(
            state,
            app_wire,
            person_id,
            workspace_id,
            vec![Box::new(power_service), Box::new(system_service)],
        );
        (power_link, session)
    }

    /// With a `PowerLink`, a profile change arriving from the daemon reaches the `power`
    /// reading live without waiting for a poll of `System::power`.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_daemon_profile_change_reaches_power_without_polling() {
        const WORKSPACE: &str = "3b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5f";

        let _settled = ui::motion::testing::settled();
        let state = tempfile::tempdir().expect("a state directory");
        let (_env, _identity_root, person_id) = governed_workspace(state.path(), WORKSPACE);
        let app_wire = tempfile::tempdir().expect("a directory to serve live power actions in");

        let fake_system = FakeSystem::new();
        // Give the test double a different mode so any poll of `System::power` would disagree.
        *fake_system.power.lock().expect("power lock") = tilo_system_service::model::Power {
            modes: vec![tilo_system_service::model::PowerMode::Performance],
            mode: Some(tilo_system_service::model::PowerMode::Performance),
            user_switching: true,
            ..tilo_system_service::model::Power::default()
        };
        let (power_link, mut session) = live_power_session(
            state.path(),
            app_wire.path(),
            &person_id,
            WORKSPACE,
            &fake_system,
            vec!["balanced".into(), "power-saver".into()],
            "balanced",
        );

        let reading = session
            .read(tilo_system_service::POWER)
            .expect("power reading");
        assert_eq!(reading["modes"], serde_json::json!(["balanced", "saver"]));
        assert_eq!(reading["mode"], serde_json::json!("balanced"));
        assert_eq!(reading["user-switching"], serde_json::json!(true));

        // The daemon changes profile externally; one session step surfaces it without polling.
        power_link.bridge().note_active_profile("power-saver");
        session.step(Duration::from_millis(1)).expect("a frame");

        let updated = session
            .read(tilo_system_service::POWER)
            .expect("updated power reading");
        assert_eq!(updated["mode"], serde_json::json!("saver"));
        assert_eq!(updated["modes"], serde_json::json!(["balanced", "saver"]));
        assert_eq!(
            fake_system.power_reads(),
            0,
            "the system service never polled System::power()"
        );

        let sleep = session
            .read(tilo_session_power::SLEEP)
            .expect("sleep reading");
        assert_eq!(sleep.get("profile"), None, "{sleep}");
        assert_eq!(sleep.get("profiles"), None, "{sleep}");
    }

    /// What `power` lists as `modes` is exactly what `power#set-power-mode` accepts,
    /// including after the daemon changes the advertised profile list.
    #[cfg(target_os = "linux")]
    #[test]
    fn power_modes_and_set_power_mode_stay_in_step_when_the_daemon_list_changes() {
        use tilo_system_service::device::Change;
        use tilo_system_service::model::PowerMode;

        const WORKSPACE: &str = "4b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b60";

        let _settled = ui::motion::testing::settled();
        let state = tempfile::tempdir().expect("a state directory");
        let (_env, _identity_root, person_id) = governed_workspace(state.path(), WORKSPACE);
        let app_wire = tempfile::tempdir().expect("a directory to serve live mode-list actions in");
        let fake_system = FakeSystem::new();
        let (power_link, mut session) = live_power_session(
            state.path(),
            app_wire.path(),
            &person_id,
            WORKSPACE,
            &fake_system,
            vec!["balanced".into(), "power-saver".into()],
            "balanced",
        );

        assert_eq!(
            session.read(tilo_system_service::POWER).expect("power")["modes"],
            serde_json::json!(["balanced", "saver"])
        );
        assert_eq!(
            session.invoke(
                tilo_system_service::POWER,
                "set-power-mode",
                &single_arg("mode", "performance"),
            ),
            Err(tilo_actions::ApiError::InvalidArguments(
                "this device offers no power mode performance".to_owned()
            ))
        );

        // The daemon now advertises performance and balanced, without power-saver.
        power_link
            .bridge()
            .note_profiles(vec!["performance".into(), "balanced".into()]);
        session.step(Duration::from_millis(1)).expect("a frame");
        assert_eq!(
            session.read(tilo_system_service::POWER).expect("power")["modes"],
            serde_json::json!(["performance", "balanced"])
        );
        assert!(
            session
                .invoke(
                    tilo_system_service::POWER,
                    "set-power-mode",
                    &single_arg("mode", "performance"),
                )
                .is_ok(),
            "performance is accepted once the daemon advertises it"
        );
        let _ = settled_power(&mut session);
        assert_eq!(
            fake_system.changes(),
            vec![Change::PowerMode(PowerMode::Performance)]
        );
        assert_eq!(
            session.invoke(
                tilo_system_service::POWER,
                "set-power-mode",
                &single_arg("mode", "saver"),
            ),
            Err(tilo_actions::ApiError::InvalidArguments(
                "this device offers no power mode saver".to_owned()
            )),
            "saver is refused once the daemon no longer advertises it"
        );

        // When the daemon advertises no profiles, the action becomes unavailable.
        power_link.bridge().note_profiles(Vec::new());
        session.step(Duration::from_millis(1)).expect("a frame");
        assert_eq!(
            session.read(tilo_system_service::POWER).expect("power")["modes"],
            serde_json::json!([])
        );
        assert_eq!(
            session.invoke(
                tilo_system_service::POWER,
                "set-power-mode",
                &single_arg("mode", "balanced"),
            ),
            Err(tilo_actions::ApiError::Unavailable {
                code: "no_power_daemon".to_owned(),
                reason: "This device offers no power modes".to_owned(),
            })
        );
    }

    /// When the session has a `PowerLink` whose power service never attached or fed a
    /// daemon reading, `power` still polls `System::power()` and `set-power-mode` works.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_unattached_power_service_falls_back_to_polling_and_runs_set_power_mode() {
        use tilo_system_service::device::Change;
        use tilo_system_service::model::PowerMode;

        const WORKSPACE: &str = "5b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b61";

        let _settled = ui::motion::testing::settled();
        let state = tempfile::tempdir().expect("a state directory");
        let (_env, _identity_root, person_id) = governed_workspace(state.path(), WORKSPACE);
        let app_wire = tempfile::tempdir().expect("a directory to serve unattached power actions");

        let power_link = tilo_session_power::PowerLink::default();
        let power_service =
            tilo_session_power::SessionPower::without_daemons(power_link.clone()).expect("power");
        let fake_system = FakeSystem::new();
        let mut system_service = crate::power::system_service(
            std::sync::Arc::clone(&fake_system)
                as std::sync::Arc<dyn tilo_system_service::device::System>,
            None,
            Some(&power_link),
        )
        .expect("system");
        tilo_service::Service::settle(&mut system_service);
        let mut session = power_session(
            state.path(),
            app_wire.path(),
            &person_id,
            WORKSPACE,
            vec![Box::new(power_service), Box::new(system_service)],
        );

        let reading = session
            .read(tilo_system_service::POWER)
            .expect("power reading");
        assert_eq!(reading["modes"], serde_json::json!(["saver", "balanced"]));
        assert_eq!(reading["mode"], serde_json::json!("balanced"));
        assert!(
            fake_system.power_reads() >= 1,
            "an unattached power service falls back to polling System::power()"
        );

        let person = Caller::local_person(person_id).in_workspace(WORKSPACE);
        invoke_session_step(
            &mut session,
            &person,
            tilo_system_service::POWER,
            "set-power-mode",
            single_arg("mode", "saver"),
        )
        .expect("set-power-mode works while falling back to polling");
        let power = settled_power(&mut session);
        assert_eq!(power["mode"], serde_json::json!("saver"));
        assert_eq!(
            fake_system.changes(),
            vec![Change::PowerMode(PowerMode::Saver)]
        );
    }

    /// On the live path, the first `power` reading in which `pending` clears after
    /// `set-power-mode` lands already carries the new `mode` without waiting for a D-Bus signal.
    #[cfg(target_os = "linux")]
    #[test]
    fn on_the_live_path_the_first_reading_after_set_power_mode_lands_carries_the_new_mode() {
        const WORKSPACE: &str = "6b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b62";

        let _settled = ui::motion::testing::settled();
        let state = tempfile::tempdir().expect("a state directory");
        let (_env, _identity_root, person_id) = governed_workspace(state.path(), WORKSPACE);
        let app_wire = tempfile::tempdir().expect("a directory to serve live mode actions");
        let fake_system = FakeSystem::new();
        let (_power_link, mut session) = live_power_session(
            state.path(),
            app_wire.path(),
            &person_id,
            WORKSPACE,
            &fake_system,
            vec!["balanced".into(), "power-saver".into()],
            "balanced",
        );

        assert!(
            session
                .invoke(
                    tilo_system_service::POWER,
                    "set-power-mode",
                    &single_arg("mode", "saver"),
                )
                .is_ok()
        );
        // Do NOT push a D-Bus signal onto `power_link.bridge()`; the very first reading
        // where `pending` clears must already carry `"saver"`.
        let reading = settled_power(&mut session);
        assert_eq!(reading["pending"], serde_json::json!([]));
        assert_eq!(reading["mode"], serde_json::json!("saver"));
        assert_eq!(fake_system.power_reads(), 0);
    }

    /// Every resource the session's own API publishes, by its id.
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
