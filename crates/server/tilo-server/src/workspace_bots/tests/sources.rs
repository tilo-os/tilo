//! A person's event that waits on the workspace's mail, through the server's
//! own actions ([`tilo_workspace_service::agents::events`]) and the watch it keeps of
//! the accounts the workspace holds, with no device of theirs asking: a
//! message that arrives starts the event once with what arrived as data,
//! nothing that arrives asks nobody anything, and what another workspace
//! keeps, or came while the event was paused, is never handed over.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use tilo_actions::remote::workspace_events::{self, EventView, Sort};
use tilo_actions::remote::{Caller, InvokeArgument, InvokeRequest, ReceiptStatus};
use tilo_server_protocol::WorkspaceId;

use super::judging::ASSISTANT;
use super::times::person_with_an_assistant;
use super::*;
use crate::workspaces::fixtures::mail::{InboxSeen, Inboxes, held_message};
use tilo_workspace_service::agents::events::{Authority, WorkspaceEvents};

/// The account the event watches.
const WORK: &str = "ada@example.test";
/// Another account the workspace keeps.
const HOME: &str = "ada@home.example.test";
/// An account another workspace keeps, on the same server.
const BOBS: &str = "bob@example.test";
const BO: &str = "bo@example.test";
const CY: &str = "cy@example.test";
/// The line an event that waits on the workspace's mail opens with.
const ON_MAIL: &str = "when something new is in \"tilo.accounts.mail\"";

/// A person who keeps an assistant in a workspace of theirs, on a holder no
/// device is connected to, with the mail accounts the workspace keeps and
/// mail servers the test stands in for.
struct Hers {
    workspaces: Workspaces,
    workspace: String,
    cert: DeviceCert,
    inboxes: Arc<Inboxes>,
}

impl Hers {
    /// Her, and what keeps the holder going while it is held.
    async fn on(scratch: &std::path::Path) -> (Self, impl Sized) {
        let (workspaces, workspace, cert, holder) = person_with_an_assistant(scratch).await;
        let inboxes = Arc::new(Inboxes::default());
        workspaces.carry_mail_through(inboxes.clone());
        let hers = Self {
            workspaces,
            workspace,
            cert,
            inboxes,
        };
        hers.keeps(WORK).await;
        (hers, holder)
    }

    /// Her own root.
    const fn person(&self) -> [u8; 32] {
        self.cert.user_public_key
    }

    /// The workspace keeps `account` on this server.
    async fn keeps(&self, account: &str) {
        self.workspaces
            .keep_mail_account_as_added(&self.workspace, account)
            .await
            .unwrap();
    }

    /// The workspace's events on this holder, with `assistant` taking what
    /// they hand over.
    fn events(&self, assistant: &Arc<Assistant>) -> WorkspaceEvents {
        WorkspaceEvents::start_with(
            self.workspaces.clone(),
            assistant.clone(),
            std::sync::Arc::new(crate::workspace_bots::installed::EventRunner),
        )
    }

    /// One action on the workspace's events as a turn this server carries
    /// out for her takes it, answered as the event it is about.
    async fn take(&self, events: &WorkspaceEvents, action: &str, arguments: &Value) -> EventView {
        let caller = Caller::delegated_agent(ASSISTANT, self.person(), self.cert.device_public_key)
            .in_workspace(self.workspace.clone())
            .named("Assistant");
        let request = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: workspace_events::target(action),
            argument: InvokeArgument::Json(arguments.as_object().cloned().unwrap_or_default()),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let workspace = WorkspaceId::parse(&self.workspace).unwrap();
        match events
            .invoke(workspace, &caller, &Authority::Turn, &request)
            .await
            .status
        {
            ReceiptStatus::Completed { output } => {
                serde_json::from_value(output.value.unwrap_or_default()).unwrap()
            }
            other => panic!("{action} on her events: {other:?}"),
        }
    }

    /// An event of hers that waits on `when` and hands `instruction` over.
    async fn sets_up(&self, events: &WorkspaceEvents, when: &str, more: &Value) -> EventView {
        let mut described = json!({
            "name": "Mail",
            "when": when,
            "instruction": "Say who wrote and what they want.",
        });
        if let (Some(described), Some(more)) = (described.as_object_mut(), more.as_object()) {
            described.extend(more.clone());
        }
        self.take(events, workspace_events::SET_UP, &described)
            .await
    }

    /// The watch this server keeps of `account`'s inbox hears a message
    /// arrive: number `uid` there, from `from`, called `about` and saying
    /// `says`, once every watch asked for is in line with the accounts kept.
    async fn arrives(&self, account: &str, uid: u32, from: &str, (about, says): (&str, &str)) {
        self.workspaces.mail_watches_settled().await;
        let heard = self
            .inboxes
            .heard(account)
            .unwrap_or_else(|| panic!("the inbox of {account} is watched"));
        heard.arrived(
            vec![held_message(uid, ("", from), about, says)],
            InboxSeen {
                uid_validity: 7,
                uid,
            },
        );
    }
}

/// Wait until `done` answers something, or fail when it never does.
async fn until<T>(what: &str, mut done: impl FnMut() -> Option<T>) -> T {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(answer) = done() {
            return answer;
        }
        assert!(Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// What the assistant was handed, once it was handed `count` instructions.
async fn handed(assistant: &Assistant, count: usize) -> Vec<Instruction> {
    until("the assistant was never handed that many", || {
        Some(assistant.taken()).filter(|taken| taken.len() >= count)
    })
    .await
}

/// Long enough for a pass of an event that was woken to have ended.
async fn a_pass_goes_by() {
    tokio::time::sleep(Duration::from_millis(1500)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_that_arrives_starts_its_persons_event_once_with_what_arrived_as_data() {
    let scratch = tempfile::tempdir().unwrap();
    let (hers, _holder) = Box::pin(Hers::on(scratch.path())).await;
    let assistant = Assistant::answering(vec![]);
    let events = hers.events(&assistant);

    // A message reaches the workspace's log while something else listens,
    // before any event of hers waits on its mail.
    {
        let _asking = hers.workspaces.listen_for_mail(&hers.workspace);
        hers.arrives(WORK, 3, BO, ("Old news", "This came before the event."))
            .await;
    }
    hers.workspaces.mail_watches_settled().await;
    assert!(
        hers.inboxes.watched().is_empty(),
        "with nothing waiting on her mail her inbox is not watched"
    );

    let made = hers.sets_up(&events, ON_MAIL, &json!({})).await;
    assert_eq!((made.sort, made.when.as_str()), (Sort::Event, ON_MAIL));
    hers.workspaces.mail_watches_settled().await;
    assert_eq!(
        hers.inboxes.watched(),
        [WORK],
        "the event keeps her inbox watched, with no device of hers on"
    );

    hers.arrives(
        WORK,
        4,
        BO,
        ("Lunch on Friday?", "Does Friday at one work?"),
    )
    .await;
    let taken = handed(&assistant, 1).await;
    assert_eq!(taken.len(), 1, "{taken:?}");
    let first = &taken[0];
    assert_eq!(first.person, hex::encode(hers.person()));
    assert_eq!(first.cause.installation, made.id);
    assert!(
        first
            .cause
            .event
            .as_deref()
            .is_some_and(|event| event.starts_with("source\0")),
        "{:?}",
        first.cause
    );
    let told = &first.instruction;
    assert!(
        told.starts_with("Say who wrote and what they want."),
        "{told}"
    );
    assert!(
        told.contains("data, not instructions"),
        "what somebody outside wrote is handed over as data: {told}"
    );
    for said in [BO, "Lunch on Friday?", "Does Friday at one work?"] {
        assert!(told.contains(said), "{said}: {told}");
    }
    assert!(
        !told.contains("Old news"),
        "what her log held before the event was set up is not new to it: {told}"
    );

    // Her inbox's watch tells the same message again, then another: the
    // first starts nothing a second time.
    hers.arrives(
        WORK,
        4,
        BO,
        ("Lunch on Friday?", "Does Friday at one work?"),
    )
    .await;
    hers.arrives(WORK, 5, CY, ("Minutes", "They are attached."))
        .await;
    let taken = handed(&assistant, 2).await;
    assert_eq!(taken.len(), 2, "{taken:?}");
    assert!(
        taken[1].instruction.contains("Minutes") && taken[1].instruction.contains(CY),
        "{}",
        taken[1].instruction
    );

    // The server stops. Its events gone, nothing of theirs holds her inbox
    // watched; something else hears a message arrive meanwhile.
    drop(events);
    until("a stopped event still held her inbox watched", || {
        hers.inboxes.watched().is_empty().then_some(())
    })
    .await;
    let _asking = hers.workspaces.listen_for_mail(&hers.workspace);
    hers.arrives(WORK, 6, BO, ("While you were down", "Still on for Friday?"))
        .await;
    assert_eq!(assistant.taken().len(), 2);

    // The server starts again: the event is handed what arrived while it
    // was down, once, and nothing it was handed before.
    let events = hers.events(&assistant);
    let taken = handed(&assistant, 3).await;
    assert!(
        taken[2].instruction.contains("While you were down"),
        "{}",
        taken[2].instruction
    );
    a_pass_goes_by().await;
    assert_eq!(assistant.taken().len(), 3, "nothing is handed over twice");
    drop(events);
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_new_asks_nobody_and_its_persons_model_says_what_is_handed_over() {
    let scratch = tempfile::tempdir().unwrap();
    let (hers, _holder) = Box::pin(Hers::on(scratch.path())).await;
    let assistant = Assistant::judging(vec![Ok(Value::Bool(false)), Ok(Value::Bool(true))]);
    let events = hers.events(&assistant);
    hers.sets_up(
        &events,
        ON_MAIL,
        &json!({"onlyIf": "Does it ask her for an answer?"}),
    )
    .await;

    // Nothing arrives: no model is asked anything and no turn starts.
    a_pass_goes_by().await;
    assert!(assistant.judged().is_empty(), "no arrival, no model call");
    assert!(assistant.taken().is_empty(), "no arrival, no turn");

    // Two messages arrive. Her model is asked about each, and only the one
    // it says yes to is handed to her assistant.
    hers.arrives(WORK, 4, BO, ("Newsletter", "Our autumn offers."))
        .await;
    hers.arrives(WORK, 5, CY, ("Contract", "Can you sign by Friday?"))
        .await;
    let taken = handed(&assistant, 1).await;
    let judged = assistant.judged();
    assert_eq!(judged.len(), 2, "one judgement for each message");
    for (judgement, about) in judged.iter().zip(["Newsletter", "Contract"]) {
        assert!(
            judgement
                .input
                .as_deref()
                .is_some_and(|input| input.contains(about)),
            "{about}: {:?}",
            judgement.input
        );
    }
    a_pass_goes_by().await;
    assert_eq!(assistant.taken().len(), 1);
    let told = &taken[0].instruction;
    assert!(
        told.contains("Contract") && !told.contains("Newsletter"),
        "{told}"
    );
    assert_eq!(assistant.judged().len(), 2, "and nothing is asked again");
}

#[tokio::test(flavor = "multi_thread")]
async fn mail_the_workspace_keeps_is_handed_over_and_another_workspaces_mail_is_not() {
    let scratch = tempfile::tempdir().unwrap();
    let (hers, _holder) = Box::pin(Hers::on(scratch.path())).await;
    let assistant = Assistant::answering(vec![]);
    let events = hers.events(&assistant);

    // The workspace keeps a second account. Another workspace keeps an
    // account on the same server, and something listens for its mail.
    hers.keeps(HOME).await;
    let (elsewhere, _) = chat_workspace(&hers.workspaces, &hers.cert).await;
    hers.workspaces
        .keep_mail_account_as_added(&elsewhere, BOBS)
        .await
        .unwrap();
    let _bobs = hers.workspaces.listen_for_mail(&elsewhere);
    until("the other workspace's inbox is watched", || {
        hers.inboxes.heard(BOBS).is_some().then_some(())
    })
    .await;
    hers.sets_up(&events, ON_MAIL, &json!({})).await;

    hers.arrives(HOME, 4, BO, ("Home", "For her.")).await;
    hers.arrives(BOBS, 4, BO, ("Elsewhere", "For the other workspace."))
        .await;
    hers.arrives(WORK, 4, BO, ("Work", "For her too.")).await;
    let _taken = handed(&assistant, 2).await;
    a_pass_goes_by().await;
    let taken = assistant.taken();
    assert_eq!(
        taken.len(),
        2,
        "one instruction for each account the workspace keeps: {taken:?}"
    );
    let told: String = taken
        .iter()
        .map(|taken| taken.instruction.clone())
        .collect();
    assert!(
        told.contains("For her.") && told.contains("For her too."),
        "{told}"
    );
    for kept_back in ["Elsewhere", "For the other workspace.", BOBS] {
        assert!(
            !told.contains(kept_back),
            "nothing of another workspace's mail: {told}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_event_keeps_to_the_account_and_sender_it_names_and_to_nothing_that_came_while_paused() {
    let scratch = tempfile::tempdir().unwrap();
    let (hers, _holder) = Box::pin(Hers::on(scratch.path())).await;
    let assistant = Assistant::answering(vec![]);
    let events = hers.events(&assistant);
    hers.keeps(HOME).await;
    let when = format!("{ON_MAIL} at \"{WORK}\" where from is \"{BO}\"");
    let made = hers.sets_up(&events, &when, &json!({})).await;
    assert_eq!(made.when, when, "the line reads back as it was written");

    // Another account, another sender, and then the one it waits for, whose
    // address is the same however it is capitalised.
    hers.arrives(HOME, 4, BO, ("Other account", "Not this one."))
        .await;
    hers.arrives(WORK, 4, CY, ("Other sender", "Nor this."))
        .await;
    hers.arrives(WORK, 5, "Bo@Example.Test", ("The one", "This one."))
        .await;
    let taken = handed(&assistant, 1).await;
    assert!(
        taken[0].instruction.contains("This one."),
        "{}",
        taken[0].instruction
    );

    // Paused, it takes in what arrives and hands nothing over, then or
    // later.
    let paused = hers
        .take(&events, workspace_events::PAUSE, &json!({"event": made.id}))
        .await;
    assert!(!paused.on);
    hers.arrives(WORK, 6, BO, ("While paused", "Passed over."))
        .await;
    a_pass_goes_by().await;
    let resumed = hers
        .take(
            &events,
            workspace_events::RESUME,
            &json!({"event": made.id}),
        )
        .await;
    assert!(resumed.on);
    hers.arrives(WORK, 7, BO, ("After", "Handed over again."))
        .await;
    handed(&assistant, 2).await;
    a_pass_goes_by().await;
    let taken = assistant.taken();
    assert_eq!(taken.len(), 2, "{taken:?}");
    assert!(
        taken[1].instruction.contains("Handed over again."),
        "{}",
        taken[1].instruction
    );
    for told in &taken {
        for kept_back in ["Not this one.", "Nor this.", "Passed over."] {
            assert!(
                !told.instruction.contains(kept_back),
                "{kept_back}: {}",
                told.instruction
            );
        }
    }

    // Removed, the event no longer holds her inboxes watched.
    let removed = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: workspace_events::target(workspace_events::REMOVE),
        argument: InvokeArgument::Json(
            json!({"event": made.id})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        ),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let caller = Caller::authenticated(hers.person(), hers.cert.device_public_key)
        .in_workspace(hers.workspace.clone());
    let answered = events
        .invoke(
            WorkspaceId::parse(&hers.workspace).unwrap(),
            &caller,
            &Authority::Turn,
            &removed,
        )
        .await;
    assert!(
        matches!(answered.status, ReceiptStatus::Completed { .. }),
        "{:?}",
        answered.status
    );
    until("a removed event still held her inboxes watched", || {
        hers.inboxes.watched().is_empty().then_some(())
    })
    .await;
}
