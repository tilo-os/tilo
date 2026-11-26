//! A call that waits for its person's yes, through this host's own dispatch: it is kept as its
//! asked record, listed to the person it asks and to nobody else, and their approve of the
//! record carries it out once; their decline ends it with nothing done.

use serde_json::{Value, json};
use tilo_actions::remote::asked::{self, Asked, Offer, Resolve};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvocationReceipt, InvokeArgument, InvokeRequest,
    ReceiptStatus, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
use tilo_remote_actions::asked::AskedStore;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::workspace_vault;

use crate::fixtures::{admit, caller, settled};
use crate::fixtures::{hosting_device, test_host};
use tilo_workspace_service::workspaces::actions::ServiceWorkspaceActions;

/// The agent that asks here, by its id in the workspace.
const AGENT: &str = "scout";

/// A workspace of `workspaces` and the dispatch that serves it here: a host keeps one store of
/// the calls that wait for a person's yes and hands its approve back to its one dispatch, so a
/// call that waits is followed through that one and never through a second made beside it.
async fn hosted(
    workspaces: &crate::PersonServices,
) -> (String, WorkspaceId, ServiceWorkspaceActions) {
    let summary = workspaces.create("Team").await.unwrap();
    let service = crate::fixtures::host_dispatch(workspaces);
    (summary.workspace, summary.workspace_id, service)
}

/// The call of `target` with `argument`, under a fresh invocation id.
fn calling(target: RemoteActionTarget, argument: Value) -> InvokeRequest {
    let Value::Object(argument) = argument else {
        panic!("arguments are an object");
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target,
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// A login for `site` its agent would keep in the workspace's vault.
fn keeping(site: &str) -> InvokeRequest {
    calling(
        workspace_vault::target(workspace_vault::ADD),
        json!({"kind": "login", "belongsTo": [site], "username": "ada", "secret": "s3cret"}),
    )
}

/// `request` by `caller`, over the connection `device` certifies to the workspace.
async fn invoked(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    (device, caller): (&DeviceCert, &Caller),
    request: InvokeRequest,
) -> Result<InvocationReceipt, ActionCallError> {
    settled(
        service
            .device_page(workspace_id, device)
            .invoke(caller, request),
    )
    .await
}

/// The questions that wait for the person `who` signs as in `workspace`, oldest first, as the
/// reading of the workspace's invocations names each.
fn waiting(store: &AskedStore, workspace: &str, who: &Caller) -> Vec<Asked> {
    let person = who.user_public_key().map(hex::encode);
    store
        .waiting_in(workspace)
        .into_iter()
        .filter(|question| Some(&question.person) == person.as_ref())
        .map(|question| question.asked)
        .collect()
}

/// `answer` given to the asked record `record` by `who`.
async fn resolve(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    record: &asked::RecordRef,
    answer: Offer,
) -> Result<InvocationReceipt, ActionCallError> {
    let resolving = Resolve {
        resolution: answer,
        ..Resolve::default()
    };
    answered(service, workspace_id, who, record, &resolving).await
}

/// `resolving` given to the asked record `record` by `who`.
async fn answered(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    record: &asked::RecordRef,
    resolving: &Resolve,
) -> Result<InvocationReceipt, ActionCallError> {
    let request = calling(
        RemoteActionTarget {
            provider: tilo_action_services::workspace_invocations::PROVIDER.to_owned(),
            address: ActionAddress::new(&record.record, asked::RESOLVE),
        },
        serde_json::to_value(resolving).unwrap(),
    );
    invoked(service, workspace_id, who, request).await
}

/// How `invocation` of `caller` ended, once it did.
async fn ended(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    (device, caller): (&DeviceCert, &Caller),
    invocation: InvocationId,
) -> InvocationUpdate {
    let here = service.device_page(workspace_id, device);
    tilo_test_env::within("the approved call's end", async {
        loop {
            match here.observe(caller, &invocation).map(|state| state.update) {
                Some(
                    update @ (InvocationUpdate::Succeeded { .. }
                    | InvocationUpdate::Failed { .. }
                    | InvocationUpdate::Cancelled { .. }),
                ) => return update,
                _ => tokio::task::yield_now().await,
            }
        }
    })
    .await
}

/// What `caller`'s vault lists.
async fn logins(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
) -> usize {
    places(service, workspace_id, who).await.len()
}

/// Where each item `caller`'s vault lists belongs, in the order it lists them.
async fn places(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
) -> Vec<Value> {
    let listing = calling(workspace_vault::target(workspace_vault::LIST), json!({}));
    let receipt = invoked(service, workspace_id, who, listing).await.unwrap();
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the vault lists at once: {receipt:?}");
    };
    output.value.unwrap()["items"]
        .as_array()
        .map(|items| items.iter().map(|item| item["belongsTo"].clone()).collect())
        .unwrap_or_default()
}

/// An agent's change waits in its record for its person, who alone finds it among what waits
/// for them and answers it: their approve keeps the login once, a second approve finds nothing,
/// and their decline of another keeps nothing. Another member finds none of it, and neither he
/// nor the agent itself answers it.
#[tokio::test]
async fn a_call_that_waits_is_its_persons_alone_and_their_approve_carries_it_out_once() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let store = workspaces
        .asked_store()
        .expect("the host keeps one store of the calls that wait");
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let (as_ada, as_bo) = (caller(&ada), caller(&bo));
    let agent = as_ada.clone().delegated_to(AGENT);
    let before = logins(&service, workspace_id, (&ada, &as_ada)).await;

    let adding = keeping("https://example.test");
    let receipt = invoked(&service, workspace_id, (&ada, &agent), adding.clone())
        .await
        .unwrap();
    assert!(!receipt.status.is_terminal(), "it waits: {receipt:?}");
    let declined = keeping("https://elsewhere.test");
    invoked(&service, workspace_id, (&ada, &agent), declined.clone())
        .await
        .unwrap();

    // Hers alone, oldest first.
    let hers = waiting(&store, &workspace_id.to_string(), &as_ada);
    assert_eq!(hers.len(), 2, "{hers:?}");
    let (first, second) = (&hers[0], &hers[1]);
    assert_eq!(first.workspace, workspace_id.to_string());
    assert!(first.offers.contains(&Offer::Approve), "{first:?}");
    let his = waiting(&store, &workspace_id.to_string(), &as_bo);
    assert!(his.is_empty(), "{his:?}");

    // Nobody but her answers it.
    for (who, device, asking) in [
        ("another member", &bo, as_bo.clone()),
        ("her own agent", &ada, agent.clone()),
    ] {
        let refused = resolve(
            &service,
            workspace_id,
            (device, &asking),
            &first.record,
            Offer::Approve,
        )
        .await;
        assert!(refused.is_err(), "{who}: {refused:?}");
    }
    assert_eq!(
        logins(&service, workspace_id, (&ada, &as_ada)).await,
        before
    );

    // Her approve carries it out, once.
    resolve(
        &service,
        workspace_id,
        (&ada, &as_ada),
        &first.record,
        Offer::Approve,
    )
    .await
    .unwrap();
    let kept = ended(&service, workspace_id, (&ada, &agent), adding.invocation).await;
    assert!(
        matches!(kept, InvocationUpdate::Succeeded { .. }),
        "{kept:?}"
    );
    let again = resolve(
        &service,
        workspace_id,
        (&ada, &as_ada),
        &first.record,
        Offer::Approve,
    )
    .await;
    assert!(again.is_err(), "approved once: {again:?}");

    // Her decline ends the other with nothing done.
    resolve(
        &service,
        workspace_id,
        (&ada, &as_ada),
        &second.record,
        Offer::Decline,
    )
    .await
    .unwrap();
    let ended_declined = ended(&service, workspace_id, (&ada, &agent), declined.invocation).await;
    assert!(
        matches!(ended_declined, InvocationUpdate::Cancelled { .. }),
        "{ended_declined:?}"
    );
    assert_eq!(
        logins(&service, workspace_id, (&ada, &as_ada)).await,
        before + 1
    );
    let left = waiting(&store, &workspace_id.to_string(), &as_ada);
    assert!(left.is_empty(), "{left:?}");
    server.shutdown().await;
}

/// A call whose action offers no form to change (the vault keeps what an agent hands it as it
/// is) is not changed by its person's approve: an edit is refused with what it named, the call
/// still waits, and their approve as asked keeps what the agent asked for.
#[tokio::test]
async fn a_call_whose_action_offers_no_form_runs_only_as_it_was_asked() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let store = workspaces
        .asked_store()
        .expect("the host keeps one store of the calls that wait");
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let agent = as_ada.clone().delegated_to(AGENT);
    let her = (&ada, &as_ada);
    let before = places(&service, workspace_id, her).await;

    let adding = keeping("https://example.test");
    invoked(&service, workspace_id, (&ada, &agent), adding.clone())
        .await
        .unwrap();
    let asked_now = waiting(&store, &workspace_id.to_string(), &as_ada);
    assert!(asked_now[0].facts.form.is_empty(), "{asked_now:?}");
    let changing = Resolve {
        resolution: Offer::Approve,
        edits: vec![tilo_actions::DraftEdit {
            path: vec!["belongsTo".into()],
            text: "https://edited.test".into(),
        }],
        ..Resolve::default()
    };
    let refused = answered(&service, workspace_id, her, &asked_now[0].record, &changing).await;
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason }) if reason == "belongsTo cannot be changed here"),
        "{refused:?}"
    );
    let still = waiting(&store, &workspace_id.to_string(), &as_ada);
    assert_eq!(still.len(), 1, "it still waits: {still:?}");

    resolve(
        &service,
        workspace_id,
        her,
        &still[0].record,
        Offer::Approve,
    )
    .await
    .unwrap();
    let kept = ended(&service, workspace_id, (&ada, &agent), adding.invocation).await;
    assert!(
        matches!(kept, InvocationUpdate::Succeeded { .. }),
        "{kept:?}"
    );
    let after = places(&service, workspace_id, her).await;
    assert_eq!(after.len(), before.len() + 1, "{after:?}");
    assert!(
        after.contains(&json!(["https://example.test"])),
        "{after:?}"
    );
    server.shutdown().await;
}

/// A person answers a call the moment its record shows, before the host has recorded the call
/// as waiting: their answer is carried out rather than refused, and it is the end the call is
/// left with — the call's place among the host's asked calls is what says it waits, and the
/// waiting state the record's own write goes on to record never overwrites that end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answer_that_meets_the_record_is_carried_out() {
    for approve in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = &crate::PersonServices::of(server.workspaces());
        let store = workspaces
            .asked_store()
            .expect("the host keeps one store of the calls that wait");
        let (workspace, workspace_id, service) = hosted(workspaces).await;
        let ada = admit(workspaces, &workspace, &owner, 1).await;
        let as_ada = caller(&ada);
        let agent = as_ada.clone().delegated_to(AGENT);
        let before = logins(&service, workspace_id, (&ada, &as_ada)).await;
        let adding = keeping("https://example.test");
        let invocation = adding.invocation;

        // The call runs on a worker of its own, so its record can be answered while its host
        // is still taking it in.
        let asking = {
            let (service, ada, agent) = (service.clone(), ada.clone(), agent.clone());
            tokio::spawn(
                async move { invoked(&service, workspace_id, (&ada, &agent), adding).await },
            )
        };
        let record = tilo_test_env::within("the record shows", async {
            loop {
                if let Some(asked) = waiting(&store, &workspace_id.to_string(), &as_ada).first() {
                    return asked.record.clone();
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        resolve(
            &service,
            workspace_id,
            (&ada, &as_ada),
            &record,
            if approve {
                Offer::Approve
            } else {
                Offer::Decline
            },
        )
        .await
        .expect("an answer that meets the record is carried out, not refused");

        let receipt = asking.await.expect("the call answers");
        assert!(receipt.is_ok(), "the call is not refused: {receipt:?}");
        if approve {
            // A caller follows the call the moment its approve returns: it finds the call it
            // was handed back, never one there is nothing to follow.
            let here = service.in_workspace_as(workspace_id, &ada);
            let now = tilo_clock::now();
            let until = now.checked_add(tilo_test_env::hang_bound()).unwrap_or(now);
            let followed = settled(tilo_actions::remote::settle_with(
                &here, &agent, invocation, None, until,
            ))
            .await
            .expect("an admitted call is followed from the moment of the approve");
            // Under way or ended, whichever this host is at: never nothing to follow.
            assert!(
                matches!(
                    followed.update,
                    InvocationUpdate::Accepted { .. } | InvocationUpdate::Succeeded { .. }
                ),
                "{followed:?}"
            );
            let ended = ended(&service, workspace_id, (&ada, &agent), invocation).await;
            assert!(
                matches!(ended, InvocationUpdate::Succeeded { .. }),
                "{ended:?}"
            );
        }
        match ended(&service, workspace_id, (&ada, &agent), invocation).await {
            InvocationUpdate::Succeeded { .. } if approve => {}
            InvocationUpdate::Cancelled { .. } if !approve => {}
            other => panic!("their answer is what ends it, approve={approve}: {other:?}"),
        }
        assert!(
            waiting(&store, &workspace_id.to_string(), &as_ada).is_empty(),
            "the answered record is not left waiting"
        );
        assert_eq!(
            logins(&service, workspace_id, (&ada, &as_ada)).await,
            before + usize::from(approve),
            "an approve keeps the login, a decline keeps nothing"
        );
        server.shutdown().await;
    }
}

/// A rule of `workspace` that asks the person `who` signs as first, before anything of `app`:
/// what an owner sets in Settings to have a member asked.
async fn ask_first(
    workspaces: &crate::PersonServices,
    workspace: &str,
    owner: &DeviceCert,
    (who, app): (&DeviceCert, &str),
) {
    use tilo_action_policy::{Answer, Grant, Grantee, KindSet, TreePath};
    let person = hex::encode(who.user_public_key);
    let rule = Grant::new(
        format!("ask-first:{app}:{person}"),
        Grantee::person(person),
        TreePath::workspace(workspace).app(app.to_owned()),
        KindSet::ALL,
        Answer::Ask,
        Grantee::person(hex::encode(owner.user_public_key)),
    );
    let request_id = *blake3::hash(rule.id.as_str().as_bytes()).as_bytes();
    workspaces
        .put_workspace_grants(workspace, owner, request_id, vec![rule])
        .await
        .unwrap();
}

/// `request`, carrying the approve of the asked record `record` its caller wrote itself.
fn carrying_approve(mut request: InvokeRequest, record: &asked::RecordRef) -> InvokeRequest {
    request.confirmation = Some(tilo_actions::remote::Confirmation::Asked(
        asked::Approval::new(record.clone(), None),
    ));
    request
}

/// What a frame carrying an approve no resolve handed back is refused with.
fn forged(answer: &Result<InvocationReceipt, ActionCallError>) -> bool {
    matches!(answer, Err(ActionCallError::Refused { reason }) if reason == asked::FORGED)
}

/// Where the rules ask a person first, making the call is not their answer, in the vault and in
/// Mail alike: their own call waits in its asked record, and only their approve of that record
/// runs it, once. The approve answers that one call: written into the call by its caller, sent
/// again with another invocation once it ran, or moved onto another call, it is refused whole
/// where the call comes in; and their agent answers nothing for them.
#[tokio::test]
async fn a_persons_own_call_the_rules_ask_first_runs_only_on_their_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let store = workspaces
        .asked_store()
        .expect("the host keeps one store of the calls that wait");
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let her = (&ada, &as_ada);
    let before = logins(&service, workspace_id, her).await;
    for app in [tilo_app_id::apps::PASSWORDS, "so.tilo.mail"] {
        ask_first(workspaces, &workspace, &owner, (&ada, app)).await;
    }
    let record_of = |request: &InvokeRequest| {
        store
            .record(&request.invocation.0.to_string())
            .unwrap_or_else(|| panic!("{:?} waits in its asked record", request.target))
    };

    // Her own calls wait, nothing of them done: a login she keeps, and a look at Mail's accounts.
    let adding = keeping("https://example.test");
    let listing = calling(
        tilo_workspace_service::workspace_accounts::target(
            tilo_workspace_service::workspace_accounts::LIST,
        ),
        json!({}),
    );
    for call in [&adding, &listing] {
        let receipt = invoked(&service, workspace_id, her, call.clone())
            .await
            .unwrap();
        assert!(
            !receipt.status.is_terminal(),
            "{:?} waits for her yes: {receipt:?}",
            call.target
        );
    }
    let (kept_at, listed_at) = (record_of(&adding), record_of(&listing));
    let hers = waiting(&store, &workspace_id.to_string(), &as_ada);
    assert_eq!(hers.len(), 2, "{hers:?}");
    assert_eq!(
        logins_unasked(&store, &service, workspace_id, her).await,
        before
    );

    // An approve she writes into a call herself is none: refused whole, the record still waits.
    let written = invoked(
        &service,
        workspace_id,
        her,
        carrying_approve(keeping("https://written.test"), &kept_at),
    )
    .await;
    assert!(forged(&written), "{written:?}");
    // Her agent answers nothing for her.
    let agent = as_ada.clone().delegated_to(AGENT);
    let by_agent = resolve(
        &service,
        workspace_id,
        (&ada, &agent),
        &kept_at,
        Offer::Approve,
    )
    .await;
    assert!(by_agent.is_err(), "{by_agent:?}");

    // Her approve runs the login's call, once.
    resolve(&service, workspace_id, her, &kept_at, Offer::Approve)
        .await
        .unwrap();
    let kept = ended(&service, workspace_id, her, adding.invocation).await;
    assert!(
        matches!(kept, InvocationUpdate::Succeeded { .. }),
        "{kept:?}"
    );
    assert_eq!(
        logins_unasked(&store, &service, workspace_id, her).await,
        before + 1
    );

    // The approve answered that call alone.
    an_approve_answers_that_call_alone(
        (&service, workspace_id),
        her,
        &store,
        (&kept_at, before),
        (&adding, &listing),
    )
    .await;

    // Mail's call runs on her approve of its own record.
    resolve(&service, workspace_id, her, &listed_at, Offer::Approve)
        .await
        .unwrap();
    let listed = ended(&service, workspace_id, her, listing.invocation).await;
    assert!(
        matches!(listed, InvocationUpdate::Succeeded { .. }),
        "{listed:?}"
    );
    let left = waiting(&store, &workspace_id.to_string(), &as_ada);
    assert!(left.is_empty(), "{left:?}");
    server.shutdown().await;
}

/// The approve answered that call alone: the same invocation sent again answers what it came
/// to and runs nothing again, and the call sent again as another invocation, or another call, is
/// refused.
async fn an_approve_answers_that_call_alone(
    (service, workspace_id): (&ServiceWorkspaceActions, WorkspaceId),
    who: (&DeviceCert, &Caller),
    store: &AskedStore,
    (kept_at, before): (&asked::RecordRef, usize),
    (adding, listing): (&InvokeRequest, &InvokeRequest),
) {
    let resent = invoked(
        service,
        workspace_id,
        who,
        carrying_approve(adding.clone(), kept_at),
    )
    .await;
    assert!(
        matches!(&resent, Ok(receipt) if receipt.invocation == adding.invocation),
        "resent: {resent:?}"
    );
    let again = invoked(
        service,
        workspace_id,
        who,
        carrying_approve(
            InvokeRequest {
                invocation: InvocationId::new(),
                ..adding.clone()
            },
            kept_at,
        ),
    )
    .await;
    assert!(forged(&again), "replayed: {again:?}");
    let moved = invoked(
        service,
        workspace_id,
        who,
        carrying_approve(keeping("https://moved.test"), kept_at),
    )
    .await;
    assert!(forged(&moved), "moved: {moved:?}");
    let onto_mail = invoked(
        service,
        workspace_id,
        who,
        carrying_approve(listing.clone(), kept_at),
    )
    .await;
    assert!(forged(&onto_mail), "moved onto Mail's call: {onto_mail:?}");
    assert_eq!(
        logins_unasked(store, service, workspace_id, who).await,
        before.saturating_add(1)
    );
}

/// What `who`'s vault lists, read under the rule that asks them first: the read waits in its
/// asked record too, and their approve answers it.
async fn logins_unasked(
    store: &AskedStore,
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
) -> usize {
    let listing = calling(workspace_vault::target(workspace_vault::LIST), json!({}));
    let receipt = invoked(service, workspace_id, who, listing.clone())
        .await
        .unwrap();
    let output = if let ReceiptStatus::Completed { output } = receipt.status {
        output
    } else {
        let at = store
            .record(&listing.invocation.0.to_string())
            .expect("the read waits in its asked record");
        resolve(service, workspace_id, who, &at, Offer::Approve)
            .await
            .unwrap();
        match ended(service, workspace_id, who, listing.invocation).await {
            InvocationUpdate::Succeeded { output, .. } => output,
            other => panic!("the read ends listed: {other:?}"),
        }
    };
    output.value.unwrap()["items"]
        .as_array()
        .map_or(0, Vec::len)
}
