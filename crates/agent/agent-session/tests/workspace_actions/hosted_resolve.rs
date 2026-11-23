//! An agent's call that waits for its person's yes, answered by that person with the asked
//! record's own `resolve`, through the host that keeps it.
//!
//! Ada's agent Scout works on her server. A call its grants leave to Ada waits as its own
//! asked invocation record; Ada, at her page, approves it (it runs once, for Scout), declines
//! one (Scout reads that she said no), or approves one and lets Scout do the same from now on
//! (an ordinary grant of the workspace; Scout's next such call goes unasked). Nobody else
//! answers for her, and an approve Scout writes into its own call is refused whole.

use super::*;
use tilo_action_policy::{KindSet, Rule, TreePath};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::Caller;
use tilo_actions::remote::asked::{self, Approval, RecordRef};
use tilo_agent_session::workspace_actions::WorkspaceActions;

/// Scout, Ada's agent, on her server's device, held to asking her before any change when
/// `asks` holds.
fn scout(root: &ed25519_dalek::SigningKey, host: &Device, workspace: &str, asks: bool) -> Caller {
    let caller =
        Caller::authenticated(root.verifying_key().to_bytes(), host.cert.device_public_key)
            .delegated_to("scout")
            .named("Scout")
            .in_workspace(workspace)
            .in_new_execution();
    if asks {
        caller.on_turn(
            tilo_action_policy::turn::Turn::new("chat.workspace/asking", "m1"),
            vec![Rule::new(
                TreePath::workspace(workspace).rest(),
                KindSet::UNDOABLE,
                tilo_action_policy::Answer::Ask,
            )],
        )
    } else {
        caller
    }
}

/// What `caller` sending `message` to the host's workspace actions is answered.
async fn served(
    actions: &WorkspaceActions,
    workspace: WorkspaceId,
    caller: &Caller,
    message: RemoteActionMessage,
) -> RemoteActionMessage {
    actions
        .serve(workspace, caller, message)
        .expect("the host answers it")
        .await
        .expect("the host answers it")
}

/// `request`'s receipt, as `caller` is answered.
async fn invoked(
    actions: &WorkspaceActions,
    workspace: WorkspaceId,
    caller: &Caller,
    request: InvokeRequest,
) -> InvocationReceipt {
    match served(
        actions,
        workspace,
        caller,
        RemoteActionMessage::Invoke(request),
    )
    .await
    {
        RemoteActionMessage::Receipt(receipt) => receipt,
        other => panic!("an invocation is answered with its receipt: {other:?}"),
    }
}

/// How `invocation` stands, as `caller` follows it.
async fn stands(
    actions: &WorkspaceActions,
    workspace: WorkspaceId,
    caller: &Caller,
    invocation: InvocationId,
) -> InvocationUpdate {
    match served(
        actions,
        workspace,
        caller,
        RemoteActionMessage::observe(invocation),
    )
    .await
    {
        RemoteActionMessage::Update {
            state: Some(state), ..
        } => state.update,
        other => panic!("{invocation:?} is followed: {other:?}"),
    }
}

/// How `invocation` ends, followed until it has.
async fn ended(
    actions: &WorkspaceActions,
    workspace: WorkspaceId,
    caller: &Caller,
    invocation: InvocationId,
) -> InvocationUpdate {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let update = stands(actions, workspace, caller, invocation).await;
        if update.is_terminal() {
            return update;
        }
        assert!(Instant::now() < deadline, "it never ended: {update:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The asked record `invocation` waits in, as its progress names it.
async fn waits_in(
    actions: &WorkspaceActions,
    workspace: WorkspaceId,
    caller: &Caller,
    invocation: InvocationId,
) -> RecordRef {
    match stands(actions, workspace, caller, invocation).await {
        InvocationUpdate::Progress {
            phase,
            message: Some(message),
            ..
        } if phase == asked::ASKED_PHASE => {
            let (workspace, record) = message.rsplit_once('/').expect("<workspace>/<record>");
            RecordRef {
                workspace: workspace.to_owned(),
                record: record.to_owned(),
            }
        }
        other => panic!("the call waits for its person: {other:?}"),
    }
}

/// `<record>#resolve` with `resolution`, and `choice` where it names one.
fn resolve(at: &RecordRef, resolution: &str, choice: Option<usize>) -> InvokeRequest {
    let mut arguments = serde_json::Map::new();
    arguments.insert("resolution".into(), resolution.into());
    if let Some(choice) = choice {
        arguments.insert("choice".into(), choice.into());
    }
    request(
        tilo_actions::remote::workspace_invocations::PROVIDER,
        ActionAddress::new(at.record.as_str(), asked::RESOLVE),
        InvokeArgument::Json(arguments),
    )
}

fn refused(receipt: &InvocationReceipt) -> String {
    match &receipt.status {
        ReceiptStatus::Rejected {
            error: ActionCallError::Refused { reason },
        } => reason.clone(),
        other => panic!("refused: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_call_waits_for_its_persons_answer() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let actions = hosted.roster.actions.clone();
    let workspace = browser.workspace;
    let book = browser.create(SHEETS);
    crate::workspace_actions::saved(&browser.peer.barrier().unwrap()).unwrap();
    let new_sheet = ActionAddress::new("", "new-sheet");
    let scout = scout(&root, &host, &browser.cluster_id, true);
    let call = |name: &str| {
        request(
            &resource_provider(&book),
            new_sheet.clone(),
            sheet_named(name),
        )
    };

    // Scout's call waits in its asked record.
    let approved = call("Approved");
    let receipt = invoked(&actions, workspace, &scout, approved.clone()).await;
    assert!(
        !matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "it does not run unasked: {receipt:?}"
    );
    let at = waits_in(&actions, workspace, &scout, approved.invocation).await;
    assert_eq!(at.record, approved.invocation.0.to_string());

    // Nobody but Ada answers it: not Scout, not Bo.
    let (bo, _) = tilo_user_identity::UserIdentity::generate("Bo", 2);
    let bo = Caller::authenticated(bo.verifying_key().to_bytes(), [4; 32])
        .in_workspace(&browser.cluster_id);
    for (who, caller) in [("Scout", &scout), ("Bo", &bo)] {
        let receipt = invoked(&actions, workspace, caller, resolve(&at, "approve", None)).await;
        assert!(
            !matches!(receipt.status, ReceiptStatus::Completed { .. }),
            "{who} cannot answer it: {receipt:?}"
        );
    }
    // An approve Scout writes into a call of its own is refused whole: nothing runs, and the
    // record still waits.
    let mut forged = call("Forged");
    forged.confirmation = Some(Confirmation::Asked(Approval {
        record: at.clone(),
        choice: None,
    }));
    let receipt = invoked(&actions, workspace, &scout, forged.clone()).await;
    assert_eq!(refused(&receipt), asked::FORGED);
    let mut replayed = approved.clone();
    replayed.confirmation = forged.confirmation.clone();
    let receipt = invoked(&actions, workspace, &scout, replayed).await;
    assert_eq!(refused(&receipt), asked::FORGED);
    assert_eq!(
        waits_in(&actions, workspace, &scout, approved.invocation).await,
        at,
        "it still waits for Ada"
    );
    // A choice the question does not offer is no answer.
    refused(&browser.invoke(resolve(&at, "approve", Some(0))).await);

    // Ada approves: it runs once, for Scout, under its own id.
    let receipt = browser.invoke(resolve(&at, "approve", None)).await;
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "{receipt:?}"
    );
    assert!(
        matches!(
            ended(&actions, workspace, &scout, approved.invocation).await,
            InvocationUpdate::Succeeded { .. }
        ),
        "the approved call ran"
    );
    let names = after_a_marker(&mut browser, &book, &new_sheet, "Marker").await;
    assert!(names.iter().any(|name| name == "Approved"), "{names:?}");
    assert!(!names.iter().any(|name| name == "Forged"), "{names:?}");
    assert_eq!(
        refused(&browser.invoke(resolve(&at, "approve", None)).await),
        asked::ANSWERED,
        "a second approve runs nothing"
    );

    // Ada declines another: Scout reads that she said no, and nothing of it is written.
    let declined = call("Declined");
    invoked(&actions, workspace, &scout, declined.clone()).await;
    let at = waits_in(&actions, workspace, &scout, declined.invocation).await;
    let receipt = browser.invoke(resolve(&at, "decline", None)).await;
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "{receipt:?}"
    );
    assert_eq!(
        ended(&actions, workspace, &scout, declined.invocation).await,
        InvocationUpdate::Cancelled {
            invocation: declined.invocation,
            message: Some(asked::DECLINED.to_owned()),
        }
    );
    let names = after_a_marker(&mut browser, &book, &new_sheet, "After").await;
    assert!(!names.iter().any(|name| name == "Declined"), "{names:?}");
}

/// The id of the sheet `name` in `record`, as the page holds it.
fn sheet_id(browser: &mut Browser, record: &DocumentRoute, name: &str) -> String {
    browser
        .mount(record)
        .read(|doc| {
            <sheets::Workbook as ui::tilo_resource::Resource>::read(doc)
                .ok()
                .and_then(|workbook| {
                    workbook
                        .sheets
                        .iter()
                        .find(|(_, sheet)| sheet.name.0 == name)
                        .map(|(id, _)| id.to_string())
                })
        })
        .ok()
        .flatten()
        .unwrap_or_else(|| panic!("the page holds {name}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approve_and_grant_lets_the_next_such_call_go_unasked() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let actions = hosted.roster.actions.clone();
    let workspace = browser.workspace;
    let book = browser.create(SHEETS);
    crate::workspace_actions::saved(&browser.peer.barrier().unwrap()).unwrap();
    let new_sheet = ActionAddress::new("", "new-sheet");
    for name in ["First", "Second"] {
        let receipt = browser
            .invoke_settled(request(
                &resource_provider(&book),
                new_sheet.clone(),
                sheet_named(name),
            ))
            .await;
        assert!(
            matches!(receipt.status, ReceiptStatus::Completed { .. }),
            "{receipt:?}"
        );
    }
    after_a_marker(&mut browser, &book, &new_sheet, "Third").await;
    let scout = scout(&root, &host, &browser.cluster_id, false);
    let delete = |browser: &mut Browser, name: &str| {
        let mut arguments = serde_json::Map::new();
        arguments.insert("sheet".into(), sheet_id(browser, &book, name).into());
        InvokeRequest {
            target: RemoteActionTarget {
                provider: resource_provider(&book),
                address: ActionAddress::new("", "delete-sheet"),
            },
            argument: InvokeArgument::Json(arguments),
            ..request("", ActionAddress::new("", ""), InvokeArgument::None)
        }
    };

    // Deleting a sheet for good asks Ada first.
    let first = delete(&mut browser, "First");
    let receipt = invoked(&actions, workspace, &scout, first.clone()).await;
    assert!(
        !matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "it does not run unasked: {receipt:?}"
    );
    let at = waits_in(&actions, workspace, &scout, first.invocation).await;

    // Ada approves it and lets Scout delete sheets from now on.
    let receipt = browser
        .invoke(resolve(&at, "approve_and_grant", None))
        .await;
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "{receipt:?}"
    );
    assert!(
        matches!(
            ended(&actions, workspace, &scout, first.invocation).await,
            InvocationUpdate::Succeeded { .. }
        ),
        "the approved call ran"
    );
    // The grant is an ordinary one of the workspace, given by Ada, listed with the others.
    let grants = hosted
        .workspaces()
        .expect("the host keeps the workspace")
        .workspace_grants(&browser.cluster_id)
        .await
        .unwrap();
    let given = grants
        .iter()
        .find(|grant| {
            tilo_action_policy::agent_answers::is_answer(grant)
                && grant.grantee == tilo_action_policy::Grantee::agent("scout")
        })
        .expect("the grant Ada gave");
    assert_eq!(
        given.granted_by,
        tilo_action_policy::Grantee::person(hex::encode(root.verifying_key().to_bytes()))
    );
    assert_eq!(
        tilo_action_policy::agent_answers::AnswerScope::of_node(&given.node),
        Some(tilo_action_policy::agent_answers::AnswerScope::action(
            SHEETS,
            "sheets.workbook",
            "delete-sheet"
        ))
    );

    // Scout's next delete goes on unasked.
    let second = delete(&mut browser, "Second");
    let receipt = invoked(&actions, workspace, &scout, second.clone()).await;
    let receipt = if matches!(receipt.status, ReceiptStatus::Accepted) {
        ended(&actions, workspace, &scout, second.invocation).await
    } else {
        match receipt.status {
            ReceiptStatus::Completed { output } => InvocationUpdate::Succeeded {
                invocation: second.invocation,
                output,
            },
            other => panic!("it runs unasked: {other:?}"),
        }
    };
    assert!(
        matches!(receipt, InvocationUpdate::Succeeded { .. }),
        "it runs unasked: {receipt:?}"
    );

    // Ada takes the grant back where every grant of the workspace is taken back, and Scout's
    // next delete asks her again.
    let revoke = request(
        tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER,
        ActionAddress::new(
            tilo_actions::remote::workspace_permission(&browser.cluster_id, &given.id.0),
            "revoke",
        ),
        InvokeArgument::None,
    );
    let revoked = browser.invoke(revoke.clone()).await;
    assert!(
        matches!(revoked.status, ReceiptStatus::Completed { .. }),
        "Ada takes back the grant she gave: {revoked:?}"
    );
    let grants = hosted
        .workspaces()
        .expect("the host keeps the workspace")
        .workspace_grants(&browser.cluster_id)
        .await
        .unwrap();
    assert!(
        grants.iter().all(|grant| grant.id != given.id),
        "the grant is gone"
    );
    let third = delete(&mut browser, "Third");
    let receipt = invoked(&actions, workspace, &scout, third.clone()).await;
    assert!(
        !matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "without the grant it asks again: {receipt:?}"
    );
    waits_in(&actions, workspace, &scout, third.invocation).await;
    let again = browser
        .invoke(InvokeRequest {
            invocation: InvocationId::new(),
            ..revoke
        })
        .await;
    assert!(
        matches!(
            &again.status,
            ReceiptStatus::Rejected {
                error: ActionCallError::Refused { .. }
            }
        ),
        "a grant taken back is not taken back twice: {again:?}"
    );
}

/// A call that waits for its person's yes is suspended in its record, and waits there across
/// the host's restart: Ada approves it on the host that started again, it runs once, for Scout,
/// and a second approve finds nothing to answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_waits_for_its_person_across_the_hosts_restart() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let book = browser.create(SHEETS);
    crate::workspace_actions::saved(&browser.peer.barrier().unwrap()).unwrap();
    let new_sheet = ActionAddress::new("", "new-sheet");
    let scout = scout(&root, &host, &browser.cluster_id, true);
    let waits = request(
        &resource_provider(&book),
        new_sheet.clone(),
        sheet_named("After the restart"),
    );
    let actions = hosted.roster.actions.clone();
    invoked(&actions, browser.workspace, &scout, waits.clone()).await;
    let at = waits_in(&actions, browser.workspace, &scout, waits.invocation).await;
    drop(actions);

    // The host stops and starts again on the state it kept.
    browser.peer.close();
    drop(browser);
    let crate::harness::hosted::Hosted {
        server,
        web,
        agents,
        ..
    } = hosted;
    drop(agents);
    drop(web);
    Box::pin(server.shutdown()).await;
    let hosted = start(&root, &host, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;

    let receipt = browser.invoke(resolve(&at, "approve", None)).await;
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "the record still waits for Ada: {receipt:?}"
    );
    arrives(&mut browser, &book, "After the restart").await;
    let names = after_a_marker(&mut browser, &book, &new_sheet, "Marker").await;
    assert_eq!(
        names
            .iter()
            .filter(|name| name.as_str() == "After the restart")
            .count(),
        1,
        "the approved call ran once: {names:?}"
    );
    assert_eq!(
        refused(&browser.invoke(resolve(&at, "approve", None)).await),
        asked::ANSWERED,
        "a second approve runs nothing"
    );
    browser.peer.close();
}

/// Wait until the sheet `name` reached the page's copy of `record`.
async fn arrives(browser: &mut Browser, record: &DocumentRoute, name: &str) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        browser.pump();
        if sheet_names(&browser.mount(record))
            .iter()
            .any(|sheet| sheet == name)
        {
            return;
        }
        assert!(Instant::now() < deadline, "{name} never arrived");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
