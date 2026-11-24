//! An action on the workspace's machine secrets is the app's, run through the app's own record:
//! when the app refuses it for want of the person's yes, Terminal hands that question on rather
//! than a plain refusal, so the call waits as their asked record. Their approve sends the same
//! call again with the approve it carries into the app's call, and the app runs it; their
//! decline runs nothing.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tilo_actions::remote::asked::{self, AskedDispatch, AskedFacts, Offer, RecordRef, Resolve};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, Confirmation,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
    RemoteActionTarget, workspace_invocations,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId};
use tilo_agent_session::shell::{Opened, Opening, Recorder, ShellRan, Watching, WorkspaceShell};

use super::providers::Reach;
use super::terminal::{self, TerminalService};
use super::tests::{governor, test_host};

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
        _: Option<&tilo_actions::remote::environment_commands::Person>,
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
        self.calls.lock().unwrap().clone()
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
        self.calls
            .lock()
            .unwrap()
            .push(request.confirmation.clone());
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
        self.runs.lock().unwrap().push(request.argument.clone());
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
        *self.resent.lock().unwrap() = Some(resent);
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
    tilo_test_env::within("a Terminal action's answer", receiver)
        .await
        .expect("the answer comes")
        .expect("answered")
}

/// The call that names Terminal's machine secrets record.
fn call(provider: &str) -> InvokeRequest {
    let mut arguments = serde_json::Map::new();
    arguments.insert("variable".into(), json!("GITHUB_TOKEN"));
    arguments.insert("item".into(), json!(ITEM));
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: provider.to_owned(),
            address: ActionAddress::new("terminal.machine-secrets", "add-secret"),
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
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: workspace_invocations::PROVIDER.to_owned(),
            address: ActionAddress::new(&record.record, asked::RESOLVE),
        },
        argument: InvokeArgument::Json(
            serde_json::to_value(resolving)
                .unwrap()
                .as_object()
                .cloned()
                .unwrap(),
        ),
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
    let (key, owner) = governor();
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
    let answered_ok = answered(workspaces.resolve_asked(&caller, &answer(&at, Offer::Approve)))
        .await
        .expect("their approve is taken");
    assert!(
        matches!(answered_ok.status, ReceiptStatus::Completed { .. }),
        "their answer is done: {answered_ok:?}"
    );
    let resent = kept
        .resent
        .lock()
        .unwrap()
        .take()
        .expect("their approve sent the call again");
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
    let confirmations = app.confirmations();
    assert_eq!(confirmations.len(), 2, "{confirmations:?}");
    assert!(
        matches!(confirmations[1], Some(Confirmation::Asked(_))),
        "the app's own call carried their approve: {confirmations:?}"
    );
    assert_eq!(app.ran(), 1, "the app ran it once");
    let runs = app.runs.lock().unwrap();
    let InvokeArgument::Json(arguments) = &runs[0] else {
        panic!("the app ran the call it was asked: {runs:?}");
    };
    assert_eq!(arguments.get("variable"), Some(&json!("GITHUB_TOKEN")));
    assert_eq!(arguments.get("item"), Some(&json!(ITEM)));
    drop(runs);

    // Answered once: the same record answers no more.
    let again = answered(workspaces.resolve_asked(&caller, &answer(&at, Offer::Approve))).await;
    assert!(again.is_err(), "approved once: {again:?}");

    // Another call they decline runs nothing.
    let reached = app.confirmations().len();
    let declined_call = call(&provider);
    let held = answered(
        workspaces
            .providers()
            .invoke(&caller, declined_call.clone())
            .expect("Terminal serves its records"),
    )
    .await;
    let Err(ActionCallError::ConfirmationRequired(facts)) = held else {
        panic!("the person is asked: {held:?}");
    };
    let other = records
        .write_asked(&workspace, &caller, &declined_call, &facts)
        .expect("their question is kept");
    answered(workspaces.resolve_asked(&caller, &answer(&other, Offer::Decline)))
        .await
        .expect("their decline is taken");
    assert_eq!(kept.declined.load(Ordering::SeqCst), 1, "it was declined");
    assert_eq!(app.ran(), 1, "a decline runs nothing");
    assert_eq!(
        app.confirmations().len(),
        reached,
        "the declined call never reached the app to run"
    );

    server.shutdown().await;
}
