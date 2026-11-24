//! The workspace's skills, as its members share them: through tl Chat's own records and
//! actions, on the daemon that serves them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use serde_json::{Value, json};
use tilo_actions::ActionAddress;
use tilo_actions::remote::{ActionCallError, ActionProvider as _, Caller, ReceiptStatus};
use tilo_peer_state::join::Role;
use tilo_workspace_service::tl_records;
use tilo_workspace_service::workspaces::{AddMemberRequest, MemberPlace};
use tilolang_records as tl;

use crate::host;
use crate::host::Host;

const CHAT: &str = "so.tilo.chat";
/// The provider the built-in tl Chat record of a workspace is served under.
const CHAT_PROVIDER: &str = "record:so.tilo.chat:";
/// Chat's one root: the workspace's conversations and skills.
const CONVERSATIONS: &str = "tl:so.tilo.chat:root-lmdoxbnn";

/// What Chat's record publishes, as `person` reads it.
fn chat_rows(
    host: &Host,
    workspace: &str,
    person: &str,
) -> Vec<tilo_actions::remote::CatalogueRow> {
    host.daemon()
        .workspaces()
        .record_rows(workspace, &route(workspace), person)
        .unwrap_or_default()
}

fn route(workspace: &str) -> tilo_peer_state::document_frame::DocumentRoute {
    tilo_peer_state::document_frame::DocumentRoute {
        app_id: CHAT.to_owned(),
        cluster_id: workspace.to_owned(),
        document: String::new(),
    }
}

/// The address of Chat's action `name`.
fn action(host: &Host, workspace: &str, person: &str, name: &str) -> ActionAddress {
    let row = chat_rows(host, workspace, person)
        .into_iter()
        .filter_map(|row| row.action)
        .find(|candidate| candidate.address.action == name);
    row.unwrap_or_else(|| panic!("Chat publishes no {name}"))
        .address
}

/// `caller` invokes Chat's action `name` with `arguments`, as the daemon's dispatch admits it.
async fn invoke(
    host: &Host,
    caller: &Caller,
    name: &str,
    arguments: Value,
) -> Result<Value, ActionCallError> {
    let workspace = caller
        .workspace()
        .expect("the caller names its workspace")
        .to_owned();
    let person = caller
        .acting()
        .acted_for_person()
        .or_else(|| caller.acting().person_id())
        .expect("the caller acts for a person")
        .to_owned();
    let address = action(host, &workspace, &person, name);
    let receipt = host
        .dispatch()
        .invoke(caller, host::call(CHAT_PROVIDER, address, &arguments))
        .await
        .expect("the dispatch answers")?;
    match receipt.status {
        ReceiptStatus::Completed { output } => {
            Ok(output.value.expect("the action answers a value"))
        }
        ReceiptStatus::Failed { error } => Err(ActionCallError::Refused {
            reason: error.message,
        }),
        ReceiptStatus::Rejected { error } => Err(error),
        other => panic!("the call is done: {other:?}"),
    }
}

/// What Chat's record holds of the workspace, as `person` reads it through Chat's own package.
async fn read_root(host: &Host, workspace: &str, person: &str) -> Value {
    use tilo_actions::MemberId;
    use tl::runtime::Actor;
    let packages = tl::records::built_in(CHAT)
        .expect("the built-in package compiles")
        .expect("this build carries Chat's package");
    let viewer = Actor::Member(MemberId::person(person.to_owned()));
    let reader = viewer.clone();
    tl::records::with_tl_record(
        host.daemon().workspaces(),
        workspace,
        &packages,
        (viewer, person),
        move |opened| {
            opened
                .host()
                .answer_read(&json!({"item": CONVERSATIONS}), &reader)
        },
    )
    .await
    .unwrap_or_else(|error| panic!("Chat's record reads: {error}"))
}

/// Add the person `Bo` as a viewer of the daemon's workspace, with a device of theirs, and
/// answer the session their device calls with.
async fn viewer(host: &Host) -> (Caller, String, host::Member) {
    use ed25519_dalek::SigningKey;
    let (root, _) = tilo_user_identity::UserIdentity::generate("Bo", 1);
    let mut secret = [0_u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let key = SigningKey::from_bytes(&secret);
    let now = tilo_clock::unix_now_ns();
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "bo-browser".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: now,
        expires_at_ns: Some(now.saturating_add(86_400_000_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
    let daemon = host.daemon();
    let workspaces = daemon.workspaces();
    workspaces
        .add_member(
            host.workspace(),
            workspaces.device_cert(),
            AddMemberRequest {
                request_id: *blake3::hash(format!("Bo {now}").as_bytes()).as_bytes(),
                user_public_key: root.verifying_key().to_bytes(),
                place: MemberPlace::Role(Role::Viewer),
                name: Some("Bo".into()),
            },
        )
        .await
        .expect("the workspace adds its viewer");
    tilo_workspace_service::workspaces::fixtures::enter_key(workspaces, host.workspace(), &cert)
        .await;
    let bo = hex::encode(root.verifying_key().to_bytes());
    let caller = Caller::authenticated(root.verifying_key().to_bytes(), cert.device_public_key)
        .in_workspace(host.workspace());
    let member = host.dial(key, host.workspace()).await;
    (caller, bo, member)
}

/// The workspace's skills are one set its members share: a viewer reads them and writes none,
/// and another workspace has a set of its own.
#[test]
fn workspace_skills_are_shared_and_a_viewers_write_is_refused() {
    host::run(|host| async move {
        let host = host.as_ref();
        invoke(
            host,
            &host.caller(),
            "new_skill",
            json!({"name": "Weekly report"}),
        )
        .await
        .expect("the person starts a skill");
        let root = read_root(host, host.workspace(), host.person()).await;
        assert!(
            root.to_string().contains("Weekly report"),
            "the skill reads back: {root}"
        );

        // A member of the workspace reads what the person wrote.
        let (_bo_caller, bo, bo_member) = viewer(host).await;
        let reads = read_root(host, host.workspace(), &bo).await;
        assert!(
            reads.to_string().contains("Weekly report"),
            "a member reads the workspace's skills: {reads}"
        );

        // The viewer's write is refused, and the record stays as it was.
        let address = action(host, host.workspace(), host.person(), "new_skill");
        let refused = bo_member
            .call(CHAT_PROVIDER, address, &json!({"name": "Bo's report"}))
            .await;
        assert!(refused.is_err(), "a viewer writes no skill: {refused:?}");
        assert!(
            format!("{refused:?}").contains("may not invoke"),
            "the refusal says the viewer may not invoke it: {refused:?}"
        );
        let after = read_root(host, host.workspace(), host.person()).await;
        assert!(
            !after.to_string().contains("Bo's report"),
            "the refused write left nothing: {after}"
        );

        // Another workspace has its own skills.
        let other = host
            .daemon()
            .workspaces()
            .create("Elsewhere")
            .await
            .expect("a second workspace")
            .workspace;
        let viewer =
            tl::runtime::Actor::Member(tilo_actions::MemberId::person(host.person().to_owned()));
        let acting = tilo_action_policy::Acting::person(host.person().to_owned());
        let invoker = tilo_actions::ActionInvoker::admitted(&acting);
        let context = tilo_actions::ActionContext::default();
        let caller = Caller::local_person(host.person()).in_workspace(&other);
        let address = action(host, host.workspace(), host.person(), "new_skill");
        let arguments = json!({"name": "Elsewhere's report"})
            .as_object()
            .cloned()
            .unwrap_or_default();
        host.daemon()
            .workspaces()
            .apply_record_action(
                &other,
                tl_records::RecordCall {
                    app: CHAT,
                    viewer,
                    invoker: &invoker,
                    caller: &caller,
                    context: &context,
                    address: &address,
                    arguments: &arguments,
                    read: false,
                },
            )
            .await
            .expect("the person starts a skill elsewhere");
        let other_root = read_root(host, &other, host.person()).await;
        assert!(
            other_root.to_string().contains("Elsewhere's report")
                && !other_root.to_string().contains("Weekly report"),
            "the other workspace has its own skills: {other_root}"
        );
    });
}
