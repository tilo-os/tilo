//! An action on the workspace's machine secrets is the app's, run through the app's own record:
//! when the app refuses it for want of the person's yes, Terminal hands that question on rather
//! than a plain refusal, so the call waits as their asked record. Their approve sends the same
//! call again with the approve it carries into the app's call, and the app runs it; their
//! decline runs nothing.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tilo_action_services::workspace_invocations;
use tilo_actions::remote::asked::{
    self, AskedDispatch, AskedFacts, AskedRecords, Offer, RecordRef, Resolve,
};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, Confirmation,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId};
use tilo_agent_session::shell::{Opened, Opening, Recorder, ShellRan, Watching, WorkspaceShell};

use super::Workspaces;
use super::providers::Reach;
use super::terminal::{self, TerminalService};
use super::tests::{hosting_device, test_host};

/// The record of the workspace's machine secrets, as Terminal's app keeps it.
const DOCUMENT: &str = "derived:terminal.machine-secrets@tab:0000000000000000";
/// The vault item the person picks for their machine.
const ITEM: &str = "vault.item:0123456789abcdef0123456789abcdef";

/// A shell no machine secrets action reaches: only a session's own verbs run here.
struct NoShell;

impl WorkspaceShell for NoShell {
    fn run(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &Caller,
        _: bool,
        _: Option<&tilo_action_services::environment_commands::Person>,
        _: Watching<'_>,
    ) -> Result<ShellRan, String> {
        Err("this host runs no programs in a test".into())
    }

    fn open(&self, _: &Opening, _: &Caller, _: Recorder) -> Result<Opened, String> {
        Err("this host opens no terminals in a test".into())
    }
}

/// The app's own answers, as the Terminal package's broker gives them: a call without the
/// person's approve is refused with the question they are shown, and their approve runs it.
#[derive(Default)]
struct App {
    /// Each call that reached the app, with the confirmation it carried.
    calls: Mutex<Vec<Option<Confirmation>>>,
    /// What each call it ran asked for.
    runs: Mutex<Vec<InvokeArgument>>,
    /// The calls it ran, which only the person's approve lets through.
    ran: AtomicUsize,
}

impl App {
    fn confirmations(&self) -> Vec<Option<Confirmation>> {
        let Ok(calls) = self.calls.lock() else {
            unreachable!("the app's calls are not poisoned");
        };
        calls.clone()
    }

    fn ran(&self) -> usize {
        self.ran.load(Ordering::SeqCst)
    }
}

impl ActionProvider for App {
    fn query(
        &self,
        _: &Caller,
        _: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::default()))
    }

    fn invoke(
        &self,
        _: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        {
            let Ok(mut calls) = self.calls.lock() else {
                unreachable!("the app's calls are not poisoned");
            };
            calls.push(request.confirmation.clone());
        }
        if !matches!(request.confirmation, Some(Confirmation::Asked(_))) {
            let facts = AskedFacts {
                verb: "hand".into(),
                on: Some("the machine's programs".into()),
                destructive: false,
                ..AskedFacts::default()
            }
            .with_fact("Variable", "GITHUB_TOKEN")
            .with_fact("Item", ITEM);
            return Pending::ready(Err(ActionCallError::asks(facts)));
        }
        self.ran.fetch_add(1, Ordering::SeqCst);
        {
            let Ok(mut runs) = self.runs.lock() else {
                unreachable!("the app's runs are not poisoned");
            };
            runs.push(request.argument.clone());
        }
        Pending::ready(Ok(InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed {
                output: ActionOutput::default(),
            },
        }))
    }

    fn observe(&self, _: &Caller, _: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _: &Caller, _: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}

/// What the person's answer is kept as, and where their approve sends the call again.
#[derive(Default)]
struct Kept {
    providers: super::providers::Providers,
    /// The call the person's approve sent again, as the dispatch sent it.
    resent: Mutex<Option<Pending<Result<InvocationReceipt, ActionCallError>>>>,
    /// The calls they declined.
    declined: AtomicUsize,
}

impl AskedDispatch for Kept {
    fn context(&self, _: InvocationId) -> Value {
        Value::Null
    }

    fn readmit(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        _: &Value,
    ) -> Result<(), ActionCallError> {
        let Some(resent) = self.providers.invoke(caller, request) else {
            return Err(ActionCallError::UnknownAction);
        };
        {
            let Ok(mut kept) = self.resent.lock() else {
                unreachable!("the calls kept are not poisoned");
            };
            *kept = Some(resent);
        }
        Ok(())
    }

    fn declined(&self, _: &Caller, _: InvocationId) -> Result<(), ActionCallError> {
        self.declined.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// What `pending` answers, once it does.
async fn answered<T: Send + 'static>(pending: Pending<T>) -> T {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    pending.then(move |value| {
        let _ = sender.send(value);
    });
    let Ok(answer) = tilo_test_env::within("a Terminal action's answer", receiver).await else {
        unreachable!("the answer comes");
    };
    let Some(answer) = answer else {
        unreachable!("answered");
    };
    answer
}

/// The call that names Terminal's machine secrets record.
fn call(provider: &str) -> InvokeRequest {
    let mut arguments = serde_json::Map::new();
    arguments.insert("variable".into(), json!("GITHUB_TOKEN"));
    arguments.insert("item".into(), json!(ITEM));
    InvokeRequest {
        asks: false,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: provider.to_owned(),
            address: ActionAddress::new("tl.so.tilo.terminal.MachineSecrets", "add-secret"),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The person's answer `answer` to their asked record `record`.
fn answer(record: &RecordRef, answer: Offer) -> InvokeRequest {
    let resolving = Resolve {
        resolution: answer,
        ..Resolve::default()
    };
    let Ok(resolution) = serde_json::to_value(resolving) else {
        unreachable!("a resolution is JSON");
    };
    let Some(resolution) = resolution.as_object().cloned() else {
        unreachable!("a resolution is a JSON object");
    };
    InvokeRequest {
        asks: false,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: workspace_invocations::PROVIDER.to_owned(),
            address: ActionAddress::new(&record.record, asked::RESOLVE),
        },
        argument: InvokeArgument::Json(resolution),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The app asks the person before it hands their item to the machine's programs. Their approve
/// of the asked record sends the call again and the app runs it with their yes carried into its
/// own call; a second approve finds nothing, and their decline of another call runs nothing.
#[tokio::test]
async fn an_approved_machine_secret_action_keeps_its_yes_when_the_app_runs_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces().clone();
    let summary = workspaces.create("Home").await.unwrap();
    let workspace = summary.workspace.clone();
    let caller = Caller::authenticated(owner.user_public_key, owner.device_public_key)
        .in_workspace(&workspace);

    let app = Arc::new(App::default());
    workspaces.providers().provide_app(
        &workspace,
        terminal::TERMINAL_APP,
        Reach::ThisDevice,
        app.clone(),
    );
    let terminal = TerminalService::new(
        workspaces.clone(),
        Arc::new(NoShell),
        tokio::runtime::Handle::current(),
    );
    workspaces.provide(terminal::PROVIDER, Arc::new(terminal));

    let kept = Arc::new(Kept {
        providers: workspaces.providers(),
        ..Kept::default()
    });
    // The store this host keeps the calls that wait in, and this test's dispatch in it: the
    // host's own dispatch answers `tilo.invocations#resolve` for the calls it keeps, and this
    // one for the call the test writes here.
    let store = workspaces
        .asked_store()
        .expect("this host keeps asked records");
    let dispatch: Arc<dyn AskedDispatch> = kept.clone();
    let records = store.for_dispatch("terminal test's dispatch", Arc::downgrade(&dispatch));

    let provider = format!("record:{}:{DOCUMENT}", terminal::TERMINAL_APP);
    let asked_for = call(&provider);
    let first = answered(
        workspaces
            .providers()
            .invoke(&caller, asked_for.clone())
            .expect("Terminal serves its records"),
    )
    .await;
    // The app's question for the person travels as it is, so its dispatch keeps it for them
    // instead of telling the caller their answer is a refusal.
    let Err(ActionCallError::ConfirmationRequired(facts)) = first else {
        panic!("the person is asked, not refused: {first:?}");
    };
    assert_eq!(app.ran(), 0, "nothing ran before their answer");
    let at = records
        .write_asked(&workspace, &caller, &asked_for, &facts)
        .expect("their question is kept");

    // Their approve sends the same call again, and the app runs it carrying their yes.
    approve_runs_the_call(&workspaces, &caller, &at, &kept).await;
    let confirmations = app.confirmations();
    assert_eq!(confirmations.len(), 2, "{confirmations:?}");
    assert!(
        matches!(confirmations[1], Some(Confirmation::Asked(_))),
        "the app's own call carried their approve: {confirmations:?}"
    );
    assert_eq!(app.ran(), 1, "the app ran it once");
    {
        let runs = app.runs.lock().unwrap();
        let InvokeArgument::Json(arguments) = &runs[0] else {
            panic!("the app ran the call it was asked: {runs:?}");
        };
        assert_eq!(arguments.get("variable"), Some(&json!("GITHUB_TOKEN")));
        assert_eq!(arguments.get("item"), Some(&json!(ITEM)));
        drop(runs);
    }

    // Answered once: the same record answers no more.
    let again = answered(workspaces.resolve_asked(&caller, &answer(&at, Offer::Approve))).await;
    assert!(again.is_err(), "approved once: {again:?}");

    // Another call they decline runs nothing.
    a_declined_call_runs_nothing(
        &workspaces,
        &caller,
        &provider,
        &workspace,
        &records,
        &app,
        &kept,
    )
    .await;

    server.shutdown().await;
}

/// Their approve of the asked record sends the same call again, and the app runs it carrying
/// their yes.
async fn approve_runs_the_call(
    workspaces: &Workspaces,
    caller: &Caller,
    at: &RecordRef,
    kept: &Kept,
) {
    let Ok(answered_ok) =
        answered(workspaces.resolve_asked(caller, &answer(at, Offer::Approve))).await
    else {
        unreachable!("their approve is taken");
    };
    assert!(
        matches!(answered_ok.status, ReceiptStatus::Completed { .. }),
        "their answer is done: {answered_ok:?}"
    );
    let resent = {
        let Ok(mut resent) = kept.resent.lock() else {
            unreachable!("the calls kept are not poisoned");
        };
        resent.take()
    };
    let Some(resent) = resent else {
        unreachable!("their approve sent the call again");
    };
    let ran = answered(resent).await;
    assert!(
        matches!(
            ran,
            Ok(InvocationReceipt {
                status: ReceiptStatus::Completed { .. },
                ..
            })
        ),
        "the app ran the call they approved: {ran:?}"
    );
}

/// Another call they decline runs nothing: the app is asked once and never runs it.
async fn a_declined_call_runs_nothing(
    workspaces: &Workspaces,
    caller: &Caller,
    provider: &str,
    workspace: &str,
    records: &Arc<dyn AskedRecords>,
    app: &App,
    kept: &Kept,
) {
    let declined_call = call(provider);
    let Some(held) = workspaces.providers().invoke(caller, declined_call.clone()) else {
        unreachable!("Terminal serves its records");
    };
    let held = answered(held).await;
    let Err(ActionCallError::ConfirmationRequired(facts)) = held else {
        unreachable!("the person is asked: {held:?}");
    };
    // Asking reaches the app's own ask once; after the decline nothing more reaches it.
    let reached = app.confirmations().len();
    let Ok(other) = records.write_asked(workspace, caller, &declined_call, &facts) else {
        unreachable!("their question is kept");
    };
    let Ok(_declined) =
        answered(workspaces.resolve_asked(caller, &answer(&other, Offer::Decline))).await
    else {
        unreachable!("their decline is taken");
    };
    assert_eq!(kept.declined.load(Ordering::SeqCst), 1, "it was declined");
    assert_eq!(app.ran(), 1, "a decline runs nothing");
    assert_eq!(
        app.confirmations().len(),
        reached,
        "the declined call never reached the app to run"
    );
}

/// A host shell that reads the workspace as it opens, as a device's does: the read is carried on
/// the reactor that answers the workspace, and the open waits for it on its own thread.
struct ReadingShell {
    copies: Workspaces,
    reactor: tokio::runtime::Handle,
}

impl WorkspaceShell for ReadingShell {
    fn run(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &Caller,
        _: bool,
        _: Option<&tilo_action_services::environment_commands::Person>,
        _: Watching<'_>,
    ) -> Result<ShellRan, String> {
        Err("this host runs no programs in a test".into())
    }

    fn open(&self, opening: &Opening, _: &Caller, _: Recorder) -> Result<Opened, String> {
        let (read, reading) = std::sync::mpsc::sync_channel(1);
        let (copies, workspace) = (self.copies.clone(), opening.workspace.clone());
        self.reactor.spawn(async move {
            let read_name = copies.summary(&workspace).await;
            let _ = read.send(
                read_name
                    .map(|summary| summary.name)
                    .map_err(|error| error.to_string()),
            );
        });
        let name = reading
            .recv_timeout(std::time::Duration::from_secs(20))
            .map_err(|_| "the workspace's read never answered the open".to_owned())??;
        Ok(Opened {
            stream: format!("stream of {name}"),
            session: "0".repeat(32),
        })
    }
}

/// A Terminal window on a fresh tab opens its shell right after a write to the workspace: the
/// shell reads the workspace as it starts, and the reactor that answers that read is the one the
/// open was asked on, so the open answers with the shell instead of waiting on itself.
#[tokio::test]
async fn a_fresh_tab_opens_its_shell_after_a_write() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces().clone();
    let summary = workspaces.create("Home").await.unwrap();
    let workspace = summary.workspace.clone();
    let caller = Caller::authenticated(owner.user_public_key, owner.device_public_key)
        .in_workspace(&workspace);
    let terminal = TerminalService::new(
        workspaces.clone(),
        Arc::new(ReadingShell {
            copies: workspaces.clone(),
            reactor: tokio::runtime::Handle::current(),
        }),
        tokio::runtime::Handle::current(),
    );
    workspaces.provide(terminal::PROVIDER, Arc::new(terminal));

    // Terminal's session records are declared as its package declares them, and a record of
    // the workspace is written just before the open.
    let session = collab_types::surface::canonical(&json!({
        "format": 2,
        "resources": [{
            "kind": "terminal.session",
            "label": "Session",
            "version": 1,
            "fields": [{ "path": "title", "type": "any" }],
            "verbs": [{ "id": "open-terminal", "kind": "undoable" }],
        }],
    }));
    workspaces.declare_package_surface(terminal::TERMINAL_APP, session.as_bytes());
    super::fixtures::write_record(&workspaces, &workspace, ("so.tilo.notes", "note"), |doc| {
        automerge::transaction::Transactable::put(doc, automerge::ROOT, "title", "Groceries")
            .map_err(|error| error.to_string())
    })
    .await
    .expect("the note is written");

    let fresh = collab_types::document::DocumentId::window_default(8);
    let mut opening = call(&format!(
        "record:{}:{}",
        terminal::TERMINAL_APP,
        fresh.route()
    ));
    opening.target.address = ActionAddress::new("terminal.session", "open-terminal");
    opening.argument = InvokeArgument::Json(serde_json::Map::new());
    let opened = answered(
        workspaces
            .providers()
            .invoke(&caller, opening)
            .expect("Terminal serves its records"),
    )
    .await
    .expect("the fresh tab's shell opens");
    let ReceiptStatus::Completed { output } = opened.status else {
        panic!("the open answers at once: {opened:?}");
    };
    assert_eq!(
        output.value.and_then(|value| value.get("stream").cloned()),
        Some(json!("stream of Home")),
        "the shell opened on the workspace it read"
    );

    server.shutdown().await;
}
