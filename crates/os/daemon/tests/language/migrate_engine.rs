//! The Migrations app starts, follows and stops a device's migration engine through the one
//! dispatch: its code calls `engine.start(source: …)` by the package the engine publishes, the
//! call goes out as any app's call does, and the device's dispatch finds the provider that
//! registered the address `devices/<device>/migrate`. The app's run waits on that call until the
//! engine ends it, and stopping the app's invocation stops the engine's.
//!
//! The engine here is the provider's contract alone, so a run lasts until it is stopped; the
//! app, its sandboxed process and the dispatch are the ones a person's computer runs.

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, application_action_target,
    application_open_target,
};
use tilo_actions::{
    ActionAddress, ActionOutput, ActionParam, ActionSpec, EncodedAction, ExecutionMode,
    InvocationId, InvocationUpdate, OperationKind, ParamType, ResourceSpec,
};

use crate::language::host;
use crate::language::migrate_window::approve_waiting;

/// The Migrations app.
const MIGRATE: &str = "so.tilo.migrate";
/// The provider a device's migrations are published under, which is the package the engine's
/// resource is published as: its own name, apart from the Migrations app.
const PROVIDER: &str = "so.tilo.migrate.engine";
/// The device that holds the data.
const DEVICE: &str = "5b0e9c41d2a7f3e85b0e9c41d2a7f3e85b0e9c41d2a7f3e85b0e9c41d2a7f3e8";

/// A device's migration engine, by its contract: `start` runs until it is stopped, by
/// cancelling it or by `stop` naming its source.
struct Engine {
    resources: Vec<ResourceSpec>,
    asked: Mutex<Vec<InvokeRequest>>,
    runs: Mutex<HashMap<InvocationId, (String, InvocationState)>>,
    cancelled: Mutex<Vec<InvocationId>>,
}

impl Engine {
    fn new() -> Arc<Self> {
        let source = || ActionParam::required("source", ParamType::Text, "A source");
        let step = |id: &'static str, params: Vec<ActionParam>, cancellable: bool| {
            let type_id = format!("{PROVIDER}/{id}");
            let encoded = type_id.clone();
            let mut action = ActionSpec::typed_operation(
                id,
                id,
                "",
                OperationKind::Effect,
                type_id,
                params,
                move |_| EncodedAction {
                    type_id: encoded.clone(),
                    payload: Vec::new(),
                },
            );
            let contract = &mut action.type_mut().contract;
            contract.kind = tilo_actions::ActionKind::Outside;
            contract.execution = if cancellable {
                ExecutionMode::Background { cancellable: true }
            } else {
                ExecutionMode::Immediate
            };
            action
        };
        let mut spec = ResourceSpec::new(address(), "devices.migrate", "Migrations");
        spec.app_id = Some(PROVIDER.to_owned());
        spec.actions = vec![
            step(
                "start",
                vec![
                    source(),
                    ActionParam::optional("yes", ParamType::Boolean, "Yes"),
                ],
                true,
            ),
            step("stop", vec![source()], false),
        ];
        Arc::new(Self {
            resources: vec![spec],
            asked: Mutex::default(),
            runs: Mutex::default(),
            cancelled: Mutex::default(),
        })
    }

    /// The calls of `action` the engine was asked, by the source each named.
    fn asked(&self, action: &str) -> Vec<(InvokeRequest, String)> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.target.address.action == action)
            .map(|request| (request.clone(), source_of(request)))
            .collect()
    }

    fn stop_run(&self, id: &InvocationId) -> bool {
        let mut runs = self.runs.lock().unwrap();
        let Some((_, state)) = runs.get_mut(id) else {
            return false;
        };
        if state.is_terminal() {
            return false;
        }
        *state = InvocationState::new(
            *id,
            InvocationUpdate::Cancelled {
                invocation: *id,
                message: Some("stopped after the item it was on".into()),
            },
        );
        true
    }
}

fn source_of(request: &InvokeRequest) -> String {
    match &request.argument {
        InvokeArgument::Json(arguments) => arguments
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    }
}

fn address() -> String {
    format!("devices/{DEVICE}/migrate")
}

impl ActionProvider for Engine {
    fn serves(&self, provider: &str) -> bool {
        provider == PROVIDER
    }

    fn published(&self) -> Vec<ResourceSpec> {
        self.resources.clone()
    }

    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::published(
            PROVIDER,
            &self.resources,
            &query,
        )))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.asked.lock().unwrap().push(request.clone());
        let id = request.invocation;
        let source = source_of(&request);
        let state = match request.target.address.action.as_str() {
            "start" => InvocationState::new(id, InvocationUpdate::Accepted { invocation: id }),
            "stop" => {
                let going = self
                    .runs
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(_, (of, state))| *of == source && !state.is_terminal())
                    .map(|(run, _)| *run);
                let Some(run) = going.filter(|run| self.stop_run(run)) else {
                    return Pending::ready(Err(ActionCallError::Refused {
                        reason: format!("nothing of {source} is being brought over"),
                    }));
                };
                InvocationState::new(
                    id,
                    InvocationUpdate::Succeeded {
                        invocation: id,
                        output: ActionOutput {
                            value: Some(json!({"source": source, "run": run.0.to_string()})),
                            resources: Vec::new(),
                        },
                    },
                )
            }
            _ => return Pending::ready(Err(ActionCallError::UnknownAction)),
        };
        let receipt = state.receipt();
        self.runs.lock().unwrap().insert(id, (source, state));
        Pending::ready(Ok(receipt))
    }

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.runs
            .lock()
            .unwrap()
            .get(id)
            .map(|(_, state)| state.clone())
    }

    fn next_change(&self, _caller: &Caller, _id: &InvocationId) -> Option<Pending<()>> {
        // Looked at again a moment later: a run here moves only when it is stopped.
        let (moved, answer) = tilo_actions::remote::pending();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            moved.complete(());
        });
        Some(answer)
    }

    fn cancel(&self, _caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        self.cancelled.lock().unwrap().push(*id);
        Pending::ready(if self.stop_run(id) {
            Ok(())
        } else {
            Err(ActionCallError::UnknownInvocation)
        })
    }
}

/// The person at the device: one run of its dispatch, held for the whole test as a shell holds
/// its own. An invocation, and the calls its app makes for it, continue only while the run that
/// started it does.
struct Person<'a> {
    host: &'a host::Host,
    dispatch: Arc<dyn ActionProvider + Send + Sync>,
}

/// Invoke `action` on `resource` of the Migrations app with `arguments`, as the person.
async fn invoke(
    person: &Person<'_>,
    resource: &str,
    action: &str,
    arguments: &Value,
) -> InvocationReceipt {
    let target = application_action_target(person.host.workspace(), MIGRATE, resource, action);
    person
        .dispatch
        .invoke(
            &person.host.caller(),
            host::call(&target.provider, target.address, arguments),
        )
        .await
        .expect("the dispatch answers")
        .unwrap_or_else(|refused| panic!("{action} is admitted: {refused:?}"))
}

/// How invocation `id` stands once it ended, asked of the dispatch until it has.
async fn ended(person: &Person<'_>, id: InvocationId) -> InvocationUpdate {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let state = person.dispatch.observe(&person.host.caller(), &id);
        if let Some(state) = state.filter(InvocationState::is_terminal) {
            return state.update;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "invocation {id:?} never ends"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// How invocation `id` ended, approving each step the device asks its person about meanwhile.
async fn ended_approved(host: &host::Host, id: tilo_actions::InvocationId) -> InvocationUpdate {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let state = host.dispatch().observe(&host.caller(), &id);
        if let Some(state) = state.filter(InvocationState::is_terminal) {
            return state.update;
        }
        approve_waiting(host).await;
        assert!(
            std::time::Instant::now() < deadline,
            "invocation {id:?} never ends"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Wait until `seen` holds, asked again until it does, approving each step the device asks its
/// person about on the way: the app's call of the engine is one they allow, as their press of
/// the control in the window is.
async fn until(host: &host::Host, what: &str, seen: impl Fn() -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    while !seen() {
        approve_waiting(host).await;
        assert!(std::time::Instant::now() < deadline, "never: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The resource the Migrations app's `reviewed` is published on: the app's root, as the
/// catalogue lists it.
async fn root(person: &Person<'_>) -> String {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let page = person
            .dispatch
            .query(
                &person.host.caller(),
                CatalogueQuery {
                    app_id: Some(MIGRATE.to_owned()),
                    needle: "reviewed".to_owned(),
                    ..CatalogueQuery::default()
                },
            )
            .await
            .expect("the catalogue answers")
            .expect("the catalogue lists the app");
        let found = page.rows.iter().find_map(|row| {
            let action = row.action.as_ref()?;
            (action.address.action == "reviewed").then(|| action.address.resource.clone())
        });
        if let Some(resource) = found {
            return resource;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the app never lists `reviewed`: {:?}",
            page.rows
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The app's start reaches the engine by its address, the app's invocation lasts as long as the
/// engine's run, and the app's stop ends both; then a start stopped by cancelling the app's own
/// invocation stops the engine's run through the dispatch's cancel.
#[test]
fn the_migrations_app_starts_follows_and_stops_the_devices_engine() {
    host::run_with(&["migrate"], |host| async move {
        // Installed in the workspace as a member installs it: an app keeps its own records
        // there, and the Migrations app writes its root as it opens.
        tilo_workspace_service::workspaces::fixtures::install_app(
            host.daemon().workspaces(),
            host.workspace(),
            MIGRATE,
        )
        .await
        .expect("the member installs Migrations");
        let engine = Engine::new();
        // The engine is the Migrations app's own code, as the device registers it.
        host.daemon()
            .provide_own(PROVIDER, MIGRATE, Arc::clone(&engine) as _);
        let person = Person {
            host: host.as_ref(),
            dispatch: host.dispatch(),
        };
        let open = application_open_target(host.workspace(), MIGRATE);
        let opened = person
            .dispatch
            .invoke(
                &person.host.caller(),
                host::call(&open.provider, open.address, &json!({})),
            )
            .await
            .expect("the dispatch answers")
            .expect("the app opens");
        ended(&person, opened.invocation).await;

        let root = root(&person).await;
        let reviewed = invoke(
            &person,
            &root,
            "reviewed",
            &json!({"source": "home", "title": "Home folders", "device": DEVICE,
                    "handle": "/home/ada"}),
        )
        .await;
        let update = ended(&person, reviewed.invocation).await;
        let InvocationUpdate::Succeeded { output, .. } = update else {
            panic!("the migration is made: {update:?}");
        };
        let migration = output.resources.first().expect("the migration").clone();

        // Start: the app's call reaches the engine at the device's address, as the provider it
        // registered, and the app's invocation waits on it.
        let started = invoke(&person, &migration, "start", &json!({})).await;
        until(person.host, "the engine is asked to start", || {
            !engine.asked("start").is_empty()
        })
        .await;
        let (call, source) = engine.asked("start").remove(0);
        assert_eq!(source, "home");
        assert_eq!(call.target.provider, PROVIDER);
        assert_eq!(call.target.address, ActionAddress::new(address(), "start"));
        assert_eq!(
            call.on_behalf_of,
            Some(started.invocation),
            "the engine's run continues the app's start"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
        let following = person
            .dispatch
            .observe(&person.host.caller(), &started.invocation)
            .expect("the app's start is known");
        assert!(
            !following.is_terminal(),
            "the app's start lasts as long as the engine's run: {following:?}"
        );

        // Stop: the app's call stops the engine's run, which ends the app's start with it.
        let stopped = invoke(&person, &migration, "stop", &json!({})).await;
        assert!(
            matches!(
                ended_approved(person.host, stopped.invocation).await,
                InvocationUpdate::Succeeded { .. }
            ),
            "the stop runs"
        );
        assert_eq!(engine.asked("stop").len(), 1);
        let start_ended = ended_approved(person.host, started.invocation).await;
        assert!(
            !matches!(start_ended, InvocationUpdate::Succeeded { .. }),
            "a stopped run did not bring everything in: {start_ended:?}"
        );

        // Cancel: cancelling the app's start cancels the engine's run it waits on.
        let again = invoke(&person, &migration, "start", &json!({})).await;
        until(person.host, "the engine is asked to start again", || {
            engine.asked("start").len() == 2
        })
        .await;
        let run = engine.asked("start").remove(1).0.invocation;
        person
            .dispatch
            .cancel(&person.host.caller(), &again.invocation)
            .await
            .expect("the dispatch answers")
            .expect("the app's start is cancelled");
        until(person.host, "the engine's run is cancelled", || {
            engine.cancelled.lock().unwrap().contains(&run)
        })
        .await;
        let again_ended = ended(&person, again.invocation).await;
        assert!(
            !matches!(again_ended, InvocationUpdate::Succeeded { .. }),
            "{again_ended:?}"
        );
    });
}
