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
use tilo_workspace_service::workspaces::model_answers::scripted::{Reply, ScriptedProvider};
use tilo_workspace_service::workspaces::{AddMemberRequest, MemberPlace};
use tilolang_workspace as tl;

use crate::language::host;
use crate::language::host::Host;

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
/// Chat's package under the tree's `apps/tl`, which the device installs to run it.
const CHAT_PACKAGE: &str = "chat";

/// Chat started in `workspace` with no window, as the daemon runs its workspace's apps: its
/// instance opens the record there, so what Chat publishes is what the workspace's record holds.
async fn start_chat(host: &Host, workspace: &str) {
    use tilo_actions::remote::{APPLICATION_RESOURCE_PREFIX, APPLICATION_START_ACTION};
    let target = tilo_actions::remote::RemoteActionTarget {
        provider: tilo_actions::remote::workspace_provider(workspace),
        address: ActionAddress::new(
            format!("{APPLICATION_RESOURCE_PREFIX}{CHAT}"),
            APPLICATION_START_ACTION,
        ),
    };
    host::output(host, target, Value::Null)
        .await
        .unwrap_or_else(|refused| panic!("the workspace starts Chat: {refused:?}"));
}

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

/// The person runs Chat's action `name` with `arguments`, through the daemon's dispatch, followed
/// to its end: what it answered.
async fn invoke(host: &Host, name: &str, arguments: Value) -> Result<Value, ActionCallError> {
    let address = action(host, host.workspace(), host.person(), name);
    let output = host::output(
        host,
        tilo_actions::remote::RemoteActionTarget {
            provider: CHAT_PROVIDER.to_owned(),
            address,
        },
        arguments,
    )
    .await?;
    Ok(output.value.unwrap_or_default())
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
    host::run_with(&[CHAT_PACKAGE], |host| async move {
        let host = host.as_ref();
        start_chat(host, host.workspace()).await;
        invoke(host, "new_skill", json!({"name": "Weekly report"}))
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
            format!("{refused:?}").contains("do not let this device run"),
            "the refusal says the viewer may not invoke it: {refused:?}"
        );
        let after = read_root(host, host.workspace(), host.person()).await;
        assert!(
            !after.to_string().contains("Bo's report"),
            "the refused write left nothing: {after}"
        );

        // Another workspace has its own skills: a session in it starts Chat's record there, and
        // what it writes stays there.
        let other = host
            .daemon()
            .workspaces()
            .create("Elsewhere")
            .await
            .expect("a second workspace")
            .workspace;
        let elsewhere = host.agent_in(&other);
        // The workspace's first look at Chat makes its record there, as opening the app does.
        let _ = elsewhere
            .tool("tilo_ls", &json!({"path": "so.tilo.chat"}))
            .await;
        let (made, failed) = crate::language::chat::said(
            &elsewhere
                .tool(
                    "tilo_do",
                    &json!({"path": format!("so.tilo.chat/{CONVERSATIONS}#new_skill"),
                            "args": {"name": "Elsewhere's report"}}),
                )
                .await,
        );
        assert!(!failed, "the person starts a skill elsewhere: {made}");
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
/// A mention's mark names its member as [`tilo_action_policy::MemberId::mark_value`] writes it,
/// which is what the workspace's triggers look for in a message.
fn mentioning(person: &str, text: &str) -> Value {
    let label = format!("@{SCOUT}");
    let member = tilo_action_policy::MemberId::agent(SCOUT_ID, person).mark_value();
    json!({"rich": [{
        "id": "",
        "kind": "paragraph",
        "text": format!("{label} {text}"),
        "marks": [{
            "start": 0,
            "end": label.chars().count(),
            "name": "mention",
            "value": {"Str": member},
        }],
        "attrs": {},
        "children": [],
    }]})
}

/// The provider of the workspace's running Chat, as the person's dispatch reaches it: the
/// instance that opens Chat's record there and answers its actions.
async fn chat_provider(host: &Host) -> String {
    host::listed(host, CHAT)
        .await
        .expect("Chat's catalogue reads")
        .providers
        .into_iter()
        .find(|provider| {
            provider.app_id == CHAT
                && provider.workspace == host.workspace()
                && provider.provider.parse::<u64>().is_ok()
        })
        .map(|provider| provider.provider)
        .unwrap_or_else(|| panic!("Chat runs in the worker's workspace"))
}

/// The person writes `words` in Chat's channel `channel`, mentioning their assistant, through
/// the app instance their page writes through and the daemon's dispatch as themselves: the
/// person's own write, not their agent's, which the agent's triggers leave out. The instance
/// takes the channel as its record delivers it, so a write that comes before it holds the
/// channel is asked again.
async fn write_to_scout(host: &Host, channel: &str, words: &str) -> (String, bool) {
    let address = chat_rows(host, host.workspace(), host.person())
        .into_iter()
        .filter_map(|row| row.action)
        .map(|action| action.address)
        .find(|address| address.resource == channel && address.action == "send")
        .unwrap_or_else(|| panic!("Chat publishes a send on {channel}"));
    let arguments = json!({"body": mentioning(host.person(), words)});
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let target = tilo_actions::remote::RemoteActionTarget {
            provider: chat_provider(host).await,
            address: address.clone(),
        };
        match host::output(host, target, arguments.clone()).await {
            Ok(output) => {
                return (
                    output
                        .value
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    false,
                );
            }
            Err(ActionCallError::UnknownAction | ActionCallError::Unavailable { .. })
                if Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(refused) => return (format!("{refused:?}"), true),
        }
    }
}

/// Link a model on this computer and answer the workspace's calls through `replies`, one per
/// call: the model its agents answer a turn on.
/// Link a model on this computer and answer the workspace's calls through `replies`, one per
/// call: the model its agents answer a turn on.
fn scripted(host: &Host, replies: impl IntoIterator<Item = Reply>) -> Arc<ScriptedProvider> {
    host.scripted_models(replies)
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

/// The person starts the skill `name` through Chat's record, named by the published id its
/// action answered with: what a resource of the record is called in the app's paths.
async fn told_skill(host: &Host, name: &str) -> String {
    let target = tilo_actions::remote::RemoteActionTarget {
        provider: CHAT_PROVIDER.to_owned(),
        address: action(host, host.workspace(), host.person(), "new_skill"),
    };
    let output = host::output(host, target, json!({"name": name}))
        .await
        .expect("the person starts a skill");
    output
        .resources
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("new_skill names the skill it made: {output:?}"))
}

/// A turn on the workspace's host changes the workspace's skill through the same actions its
/// members use: the model calls `correct_skill` on the skill the person wrote, the change lands
/// in the record members read, and the turn goes on after its tool answered.
#[test]
fn a_hosted_agent_uses_the_same_workspace_skills_actions() {
    host::run_with(&[CHAT_PACKAGE], |host| async move {
        let host = host.as_ref();
        start_chat(host, host.workspace()).await;
        make_scout(host).await;
        let skill = told_skill(host, "Weekly report").await;
        invoke(
            host,
            "correct_skill",
            json!({"work": "Write the weekly report", "how": INSTRUCTIONS}),
        )
        .await
        .expect("the person says what the skill is for and how it is done");
        // The path a skill is called by: the resource the person's own action named it as, as
        // the app's paths name a resource of its record.
        let path = format!("so.tilo.chat/{skill}#correct_skill");

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
        let channel =
            crate::language::chat::made_channel(&host.agent(), crate::language::chat::CHANNEL)
                .await;
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
            crate::language::chat::chat_holds_soon(host, "I updated the workspace skill.").await,
            "the turn says what it changed once its tool answered"
        );
        let read = read_root(host, host.workspace(), host.person()).await;
        assert!(
            read.to_string().contains(CORRECTION) && !read.to_string().contains(INSTRUCTIONS),
            "the workspace's one skill holds the agent's correction: {read}"
        );
    });
}

/// The events the workspace's host keeps, as its person's own call lists them.
async fn events_of(host: &Host) -> Vec<tilo_actions::remote::workspace_events::EventView> {
    use tilo_actions::remote::workspace_events;
    let address = ActionAddress::new(workspace_events::RESOURCE, workspace_events::LIST);
    let receipt = host
        .dispatch()
        .invoke(
            &host.caller(),
            host::call(workspace_events::PROVIDER, address, &json!({})),
        )
        .await
        .expect("the dispatch answers")
        .unwrap_or_else(|error| panic!("the events list: {error:?}"));
    match receipt.status {
        ReceiptStatus::Completed { output } => {
            serde_json::from_value::<workspace_events::Events>(
                output.value.expect("the events answer a value"),
            )
            .expect("the list reads as events")
            .events
        }
        other => panic!("the events list: {other:?}"),
    }
}

/// A reminder the person's assistant set up itself, from the asking to the answer. The person
/// writes to their assistant and nothing of theirs stays on: the assistant's own tool call sets
/// the reminder up on the host; the moment comes; the host asks the assistant itself, as the
/// person's reminder; the answer is in their conversation, once. The host starts again and
/// keeps the reminder as invoked once, asking nothing twice.
#[test]
fn a_reminder_the_assistant_set_up_itself_starts_its_turn_with_its_person_closed() {
    use tilo_actions::remote::workspace_events::{Ended, Sort};
    host::run_with(&[CHAT_PACKAGE], |host| async move {
        let host = host.as_ref();
        start_chat(host, host.workspace()).await;
        make_scout(host).await;
        let channel =
            crate::language::chat::made_channel(&host.agent(), crate::language::chat::CHANNEL)
                .await;
        let at_ms = tilo_clock::unix_now_ms_i64() + 2_000;
        let when = format!(
            "once at {}",
            ui::format::Timestamp::from_millisecond(at_ms).expect("a moment")
        );

        // Her one turn sets the reminder up with the host's own action, through the tools every
        // turn has, and says so.
        let scripted = scripted(
            host,
            [
                Reply::tool_call(
                    "call-1",
                    "tilo_do",
                    json!({
                        "path": "tilo.events/events#set-up",
                        "args": {
                            "name": "Call home",
                            "when": when,
                            "instruction": "Say that it is time to call home.",
                        },
                    }),
                ),
                Reply::text("I will remind you to call home."),
                Reply::text("It is time to call home."),
            ],
        );
        let (wrote, failed) =
            write_to_scout(host, &channel, "Remind me in a moment to call home.").await;
        assert!(!failed, "the person writes to their assistant: {wrote}");
        asked(&scripted, 2).await;
        let answers = tool_answers(&scripted.asked()[1].request);
        assert!(
            matches!(answers.as_slice(), [(answer, false)] if !answer.contains("failed")),
            "the assistant's own call sets the reminder up: {answers:?}"
        );
        assert!(
            crate::language::chat::chat_holds_soon(host, "I will remind you to call home.").await,
            "the assistant says it will remind her"
        );
        let kept = events_of(host).await;
        let [reminder] = kept.as_slice() else {
            panic!("the assistant's own call sets one reminder up: {kept:?}")
        };
        assert_eq!(
            (reminder.name.as_str(), reminder.sort, reminder.runs),
            ("Call home", Sort::Scheduled, 0),
            "it is on a clock and has not run: {reminder:?}"
        );

        // The moment comes with nothing of hers on. The host asks her assistant itself, as her
        // reminder and for her.
        asked(&scripted, 3).await;
        assert!(
            tilo_clock::unix_now_ms_i64() >= at_ms,
            "it asks at its moment, not before"
        );
        let asked_at = format!("{:?}", scripted.asked()[2].request);
        assert!(
            asked_at.contains("Call home")
                && asked_at.contains("Say that it is time to call home."),
            "the assistant is told what started the turn and what to do: {asked_at}"
        );
        let deadline = tilo_test_env::hang_deadline();
        while !crate::language::chat::chat_holds(host, "It is time to call home.")
            .await
            .unwrap_or(false)
        {
            assert!(
                Instant::now() < deadline,
                "the reminder's answer is in her conversation"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        // It reads as having run once, at its moment, and asks nothing more.
        let ran = events_of(host).await;
        let [reminder] = ran.as_slice() else {
            panic!("one reminder: {ran:?}")
        };
        assert_eq!(
            (reminder.runs, reminder.next_at_ms),
            (1, None),
            "invoked once, and not again: {reminder:?}"
        );
        let last = reminder.last_run.as_ref().expect("the run is kept");
        assert_eq!(last.ended, Ended::Ran, "{last:?}");
        assert!(last.at_ms >= at_ms, "at its moment, not before: {last:?}");
        asked(&scripted, 3).await;

        // The host starts again: it keeps the reminder as invoked once, and asks nothing a
        // second time.
        drop(scripted);
        host.restart().await;
        let after = events_of(host).await;
        let [reminder] = after.as_slice() else {
            panic!("the reminder is kept across a restart: {after:?}")
        };
        assert_eq!(
            (reminder.name.as_str(), reminder.runs),
            ("Call home", 1),
            "kept as invoked once: {reminder:?}"
        );
    });
}
