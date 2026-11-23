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
#![cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "the page offers the layout and the windows as steps next"
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
    steps: Arc<dyn SessionSteps>,
    /// The rows the session's own API offered on its last pass: what the catalogue lists.
    /// Read on the loop and written here, so a catalogue built on the broker's own thread
    /// never waits on the session, and never reads the layout from off its thread.
    rows: std::sync::RwLock<Vec<tilo_actions::api::PlacedAction>>,
    /// Where the API stood when the rows were last read
    /// ([`tilo_session_core::surfaces::Surfaces::revision`]).
    read_at: std::sync::Mutex<Vec<(usize, u64)>>,
}

impl SessionActions {
    /// The session's actions, whose rows the session hands over on each pass through `steps`.
    pub(crate) fn new(steps: Arc<dyn SessionSteps>) -> Self {
        Self {
            steps,
            rows: std::sync::RwLock::default(),
            read_at: std::sync::Mutex::default(),
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
        changed
    }

    /// The rows offered now, as the last pass read them.
    pub(crate) fn offered(&self) -> Vec<tilo_actions::api::PlacedAction> {
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

    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
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
        // The place the ask is in: an agent's turn is the conversation the person wrote in, and
        // a place it opens stands beside that conversation
        // (`docs/designs/app-language/one-app-model.md` §3). The person's own step stands beside
        // the place in front.
        let beside = request
            .actor
            .turn()
            .map(|turn| tilo_session_core::layout::turn_key(&turn.conversation, &turn.message))
            .unwrap_or_default();
        self.steps.invoke_step(
            request.actor.member().clone().into(),
            request.address.clone(),
            argument,
            beside,
            request.group.cloned(),
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
        // session's own API holds.
        let published = published(&session);
        for row in &page.rows {
            let action = row.action.as_ref().expect("a row offers its action");
            assert!(
                published.contains(&action.address.resource),
                "the row names a resource the session publishes: {action:?}"
            );
        }
    }

    #[cfg(target_os = "linux")]
    struct FakeSystem {
        power: std::sync::Mutex<tilo_system_service::model::Power>,
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
                refusal: std::sync::Mutex::new(None),
                changes: std::sync::Mutex::new(Vec::new()),
            })
        }

        fn changes(&self) -> Vec<tilo_system_service::device::Change> {
            self.changes.lock().expect("changes lock").clone()
        }
    }

    #[cfg(target_os = "linux")]
    impl tilo_system_service::device::System for FakeSystem {
        fn power(&self) -> tilo_system_service::model::Power {
            self.power.lock().expect("power lock").clone()
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
        let governor = tilo_peer_state::owner::testing::enroll_interactive_governor_in(
            &owner, &device, &cert, workspace,
        )
        .expect("this device governs the workspace it hosts");
        let host = governor.workspace_host().device_public_key;
        let frames: Vec<Vec<u8>> = governor
            .outgoing()
            .map(|(bytes, _)| bytes.to_vec())
            .collect();
        for frame in frames {
            owner
                .receive(
                    workspace_id,
                    tilo_peer_state::TransportOrigin::RetainedServerDelta(host),
                    postcard::from_bytes(&frame).expect("a publication is a message"),
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

    /// With both the power service and the system service running (as on an owned device),
    /// the catalogue an agent is offered holds exactly one action to set the power profile:
    /// `power#set-power-mode`. Invoking it changes the profile through the system service's
    /// test double, and a device refusal surfaces in `problem`.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_catalogue_offers_one_power_profile_action_and_invoking_it_switches_the_profile() {
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

    /// In a headless start (no power service / no `PowerLink`, only the system service),
    /// `power#set-power-mode` is still offered in the agent's catalogue and switches the profile.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_headless_session_still_offers_and_runs_set_power_mode() {
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
