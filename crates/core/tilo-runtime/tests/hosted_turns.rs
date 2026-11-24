//! The workspace's skills, as its members share them: through tl Chat's own records and
//! actions, on the daemon that serves them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tilo_actions::ActionAddress;
use tilo_actions::remote::{ActionCallError, ActionProvider as _, Caller, ReceiptStatus};
use tilo_peer_state::join::Role;
use tilo_workspace_service::tl_records;
use tilo_workspace_service::workspaces::model_answers::scripted::{Reply, ScriptedProvider};
use tilo_workspace_service::workspaces::model_answers::{
    ModelDescriptor, ProviderProfile, ProviderProtocol,
};
use tilo_workspace_service::workspaces::this_computer::Linking;
use tilo_workspace_service::workspaces::{AddMemberRequest, MemberPlace};
use tilolang_records as tl;

use crate::host;
use crate::host::Host;

const CHAT: &str = "so.tilo.chat";
/// The assistant a workspace starts with, as its list of agents names it, and the id a mention
/// of it carries.
const SCOUT: &str = "Scout";
const SCOUT_ID: &str = "scout";
/// What the person asks their assistant to change about a skill, and the change it makes.
const INSTRUCTIONS: &str = "Put the weekly numbers first.";
const CORRECTION: &str = "Put revenue and its source first.";
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
    workspaces
        .enter(host.workspace(), (&cert, &key))
        .await
        .expect("Bo's device lists itself");
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

/// Make the person's assistant `Scout` in the workspace, as the Assistant app's own action does:
/// the agent whose triggers a mention fires.
async fn make_scout(host: &Host) {
    let target = tilo_actions::remote::RemoteActionTarget::workspace_governance(
        host.workspace(),
        tilo_actions::remote::WORKSPACE_AGENT_CREATE,
    );
    let receipt = host
        .dispatch()
        .invoke(
            &host.caller(),
            host::call(
                &target.provider,
                target.address,
                &json!({"name": SCOUT, "id": SCOUT_ID}),
            ),
        )
        .await
        .expect("the dispatch answers")
        .unwrap_or_else(|error| panic!("the person makes their assistant: {error:?}"));
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "the agent is made: {receipt:?}"
    );
}

/// A value that mentions the workspace's assistant in Chat's rich text: what fires its trigger.
fn mentioning(text: &str) -> Value {
    let label = format!("@{SCOUT}");
    json!({"blocks": [{
        "id": "",
        "kind": "paragraph",
        "text": format!("{label} {text}"),
        "marks": [{
            "start": 0,
            "end": label.chars().count(),
            "name": "mention",
            "value": format!("{{\"agent\":\"{SCOUT_ID}\"}}"),
        }],
        "attrs": {},
        "children": [],
    }]})
}

/// The person writes `words` in Chat's channel `channel`, mentioning their assistant.
async fn write_to_scout(host: &Host, channel: &str, words: &str) -> (String, bool) {
    crate::chat::said(
        &host
            .agent()
            .tool(
                "tilo_do",
                &json!({"path": format!("so.tilo.chat/{channel}#send"),
                        "args": {"body": mentioning(words)}}),
            )
            .await,
    )
}

/// Link a model on this computer and answer the workspace's calls through `replies`, one per
/// call: the model its agents answer a turn on.
fn scripted(host: &Host, replies: impl IntoIterator<Item = Reply>) -> Arc<ScriptedProvider> {
    let daemon = host.daemon();
    let workspaces = daemon.workspaces();
    let profile = ProviderProfile {
        id: "scripted".to_owned(),
        label: "Scripted".to_owned(),
        protocol: ProviderProtocol::OpenAiCompatible,
        endpoint: "http://127.0.0.1:9/v1".to_owned(),
        models: vec![ModelDescriptor {
            tool_calling: true,
            ..ModelDescriptor::new("scripted")
        }],
        default_model: Some("scripted".to_owned()),
        ..ProviderProfile::local_vllm()
    };
    let mut config = workspaces.workspace_models(host.workspace());
    config.default_profile = Some(profile.id.clone());
    config.profiles = vec![profile];
    assert!(
        workspaces
            .link_this_computer(host.workspace(), &config, Linking::Chosen)
            .expect("the computer's model choice is linked"),
        "the workspace runs on the model this computer chose"
    );
    let scripted = ScriptedProvider::answering(replies);
    workspaces.answer_models_with(scripted.clone());
    scripted
}

/// Wait until a turn asked its model `count` times.
async fn asked(scripted: &ScriptedProvider, count: usize) {
    let deadline = tilo_test_env::hang_deadline();
    while scripted.asked().len() < count {
        assert!(
            Instant::now() < deadline,
            "the model was asked {} times of {count}",
            scripted.asked().len()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// What the tools answered in `request`, in the order asked: each answer's words, and whether
/// it failed.
fn tool_answers(
    request: &tilo_workspace_service::workspaces::model_answers::protocol::ModelRequest,
) -> Vec<(String, bool)> {
    use tilo_workspace_service::workspaces::model_answers::protocol::Content;
    request
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|content| match content {
            Content::ToolResult(result) => Some((result.content.clone(), result.is_error)),
            _ => None,
        })
        .collect()
}

/// `tilo_ls` of `arguments` as the workspace's agent reads it.
async fn listed(agent: &crate::host::Agent, arguments: Value) -> String {
    let (listed, failed) = crate::chat::said(&agent.tool("tilo_ls", &arguments).await);
    assert!(!failed, "Chat lists {arguments}: {listed}");
    listed
}

/// The path a listing gives the item named `name`, or the action `action` of it.
fn path_in(listed: &str, name: &str, action: Option<&str>) -> Option<String> {
    listed
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .find(|path| {
            path.contains(name) && action.is_none_or(|action| path.ends_with(&format!("#{action}")))
        })
        .map(str::to_owned)
}

/// The path at which Chat publishes its action `action` on its skill `name`, as an agent lists
/// it: what a turn's `tilo_do` call names.
async fn skill_path(host: &Host, name: &str, action: &str) -> String {
    let agent = host.agent();
    let root = listed(&agent, json!({"path": "so.tilo.chat"})).await;
    if let Some(path) = path_in(&root, name, Some(action)) {
        return path;
    }
    // The workspace's listing of an app reaches the app's own page; its collections are read
    // where they are published, each item by the name people know it by.
    let item = path_in(&root, name, None).unwrap_or_else(|| format!("so.tilo.chat/skills/{name}"));
    let at_item = listed(&agent, json!({"path": item, "kind": "actions"})).await;
    path_in(&at_item, name, Some(action))
        .or_else(|| path_in(&at_item, "", Some(action)))
        .unwrap_or_else(|| panic!("Chat lists {name}'s {action}: {at_item}"))
}

/// A turn on the workspace's host changes the workspace's skill through the same actions its
/// members use: the model calls `correct_skill` on the skill the person wrote, the change lands
/// in the record members read, and the turn goes on after its tool answered.
#[test]
fn a_hosted_agent_uses_the_same_workspace_skills_actions() {
    host::run(|host| async move {
        let host = host.as_ref();
        make_scout(host).await;
        invoke(
            host,
            &host.caller(),
            "new_skill",
            json!({"name": "Weekly report"}),
        )
        .await
        .expect("the person starts a skill");
        invoke(
            host,
            &host.caller(),
            "correct_skill",
            json!({"work": "Write the weekly report", "how": INSTRUCTIONS}),
        )
        .await
        .expect("the person says what the skill is for and how it is done");
        let path = skill_path(host, "Weekly report", "correct_skill").await;

        let scripted = scripted(
            host,
            [
                Reply::tool_call(
                    "call-1",
                    "tilo_do",
                    json!({"path": path, "args": {"how": CORRECTION}}),
                ),
                Reply::text("I updated the workspace skill."),
            ],
        );
        let channel = crate::chat::made_channel(&host.agent(), crate::chat::CHANNEL).await;
        let (wrote, failed) = write_to_scout(
            host,
            &channel,
            "In Workspace Skills, correct how the weekly report is done.",
        )
        .await;
        assert!(!failed, "the person writes to their assistant: {wrote}");

        asked(&scripted, 2).await;
        let requests = scripted.asked();
        assert!(
            requests[1..]
                .iter()
                .flat_map(|call| tool_answers(&call.request))
                .all(|(answer, failed)| !failed && !answer.contains("failed:")),
            "the turn's call is the workspace's action, as a member makes it: {:?}",
            tool_answers(&requests[1].request)
        );
        assert!(
            crate::chat::chat_holds(host, "I updated the workspace skill.")
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            "the turn says what it changed once its tool answered"
        );
        let read = read_root(host, host.workspace(), host.person()).await;
        assert!(
            read.to_string().contains(CORRECTION) && !read.to_string().contains(INSTRUCTIONS),
            "the workspace's one skill holds the agent's correction: {read}"
        );
    });
}
