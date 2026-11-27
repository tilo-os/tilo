//! A moderation turn started by a Moderator's saved chat watch: the line of who asked that bounds
//! it, and what it may then do to what a member posted.
//!
//! Everything here is the production path: the turn begins in [`ServerAgents::triggered`], the
//! same entry the answer step of a fired shortcut calls, and its calls are decided as an app's
//! window decides them ([`tilo_action_policy::MemberAccess`]), which is the decision the agent's
//! provider reaches too.

#![allow(
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture failing is its test failing"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tilo_agent_session::model::PlanTurns;
use tilo_model_gateway::host_backend::protocol::{
    Content, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason, Usage,
};
use tilo_server_protocol::WorkspaceId;
use tilo_workspace_agent::{WorkspaceAgents, workspace_agents};

use super::{FixedKeys, Model, ServerAgents};
use tilo_workspace_service::agent_shortcuts::{CHAT, MESSAGE};

const CHANNEL: &str = "tl.so.tilo.chat.TextChannel";
const APP_MODERATION: &str = "tilo.moderation";
const SCOUT: &str = "scout";

/// A model that looks at the workspace once and then answers in words: what the turn asked it is
/// kept, so the test reads that the turn reached its tools.
struct Scripted {
    asked: Arc<Mutex<Vec<ModelRequest>>>,
}

impl PlanTurns for Scripted {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        _: std::time::Instant,
        _: bool,
    ) -> Result<ModelResponse, ModelError> {
        self.asked.lock().unwrap().push(request.clone());
        let answer = if self.asked.lock().unwrap().len() == 1 {
            ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![Content::ToolCall(
                        tilo_model_gateway::host_backend::protocol::ToolCall {
                            id: "call-1".into(),
                            name: "tilo_ls".into(),
                            input: json!({}),
                            extra_content: None,
                        },
                    )],
                },
                stop_reason: StopReason::ToolCalls,
                usage: Usage::default(),
                cost_micros: 0,
            }
        } else {
            ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![Content::Text {
                        text: "That message is not allowed here, so I removed it.".into(),
                    }],
                },
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                cost_micros: 0,
            }
        };
        Ok(answer)
    }

    fn model(&self) -> &'static str {
        "scripted"
    }
}

/// The change a member's message made in the channel: what the Moderator's saved watch fires on.
fn a_posted_message(
    workspace: &str,
    author: &str,
    message: &str,
) -> tilo_automation::dispatch::DocumentEvent {
    tilo_automation::dispatch::DocumentEvent {
        invoked: None,
        workspace: workspace.to_owned(),
        record: "general".to_owned(),
        app: CHAT.to_owned(),
        kind: MESSAGE.to_owned(),
        change: format!("change-{message}"),
        actor: tilo_member_types::Acting::person(author),
        did: tilo_actions::Did::verb(tilo_actions::ActionKey::new(CHAT, MESSAGE, "send"), []),
        arguments: serde_json::Map::new(),
        item: Some(tilo_automation::dispatch::ItemRef {
            kind: MESSAGE.to_owned(),
            id: message.to_owned(),
            label: "something".to_owned(),
        }),
        before: None,
        after: Some(json!({"$in": {"$id": "general"}, "body": "something"})),
        caused_by: None,
        origin: None,
        incoming_message: false,
        housekeeping: false,
        shortcut: None,
        invocation: None,
    }
}

/// A workspace's record of its agents, with `scout` kept by `owner`, authored as their device.
async fn keep_scout(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    workspace: &str,
    owner: &str,
) {
    workspaces
        .author_agents(workspace, |doc| {
            let mut made = tilo_resource::Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc))
                .in_app("so.tilo.model", "Model")
                .as_author(owner)
                .in_app(tilo_workspace_agent::OWNER, "Workspace");
            made.apply(
                WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
                    avatar: None,
                    name: tilo_resource::NonBlank::parse("Scout").unwrap(),
                    id: Some(SCOUT.into()),
                    model: None,
                    mcp_client: None,
                }),
            )
            .map_err(|error| error.to_string())?;
            std::mem::swap(doc, made.doc_mut());
            Ok(())
        })
        .await
        .unwrap();
}

/// The turn a Moderator's chat watch starts answers the author of the message and is bounded by
/// the Moderator who saved the watch too: its calls carry both people, and the agent's own
/// person, so what it may do is only what all of them allow.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_moderation_turns_caller_carries_the_saver_and_the_author() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = tilo_workspace_service::workspaces::fixtures::hosting_device();
    let server =
        tilo_workspace_service::workspaces::fixtures::test_host(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Moderation").await.unwrap().workspace;
    let owner = hex::encode(cert.user_public_key);
    keep_scout(&workspaces, &workspace, &owner).await;

    // A member posts; a Moderator, someone else, saved the watch that sees it.
    let author = "a1".repeat(32);
    let moderator = "c3".repeat(32);
    let event = a_posted_message(&workspace, &author, "m-1");

    let provider = Arc::new(tilo_actions::remote::TestProvider::new());
    let asked = Arc::new(Mutex::new(Vec::new()));
    let model: Model = {
        let asked = Arc::clone(&asked);
        Arc::new(move || {
            Some(Box::new(Scripted {
                asked: Arc::clone(&asked),
            }) as Box<dyn PlanTurns>)
        })
    };
    let agents = ServerAgents::on_device(workspaces.clone(), provider.clone(), Arc::new(FixedKeys))
        .scripted(model);
    let started = tokio::time::timeout(
        Duration::from_secs(30),
        agents.triggered(
            &workspace,
            SCOUT,
            tilo_workspace_service::work::Where::of(&event)
                .expect("a message is a change an agent answers"),
            event,
            moderator.clone(),
            (tilo_actions::InvocationId::new(), Arc::default()),
        ),
    )
    .await
    .expect("the turn ends")
    .unwrap();
    assert!(
        matches!(started, tilo_workspace_service::work::Started::Taken(_)),
        "this device takes the turn: {started:?}"
    );
    assert!(
        agents.draining().finish(Duration::from_secs(30)).await,
        "the turn ends"
    );

    let callers = provider.callers();
    assert!(
        !callers.is_empty(),
        "the turn asked the workspace something"
    );
    assert_eq!(
        callers[0].acting().people(),
        vec![author.as_str(), owner.as_str(), moderator.as_str()],
        "the author whose message it answers, the agent's own person, and the Moderator who \
         saved the watch"
    );
    assert_eq!(
        callers[0].acting().agent_id(),
        Some(SCOUT),
        "every call is the agent's own"
    );
    server.shutdown().await;
}

/// A message is removed only when both the agent's own grants and the person who saved the watch
/// allow it: the Moderator who may delete another's message has it removed, while a member whose
/// grants do not allow that has the same turn refused and the message stays.
#[test]
fn a_moderation_turn_removes_a_message_only_when_the_moderator_who_saved_it_may() {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    use tilo_action_policy::{
        Answer, Context, Grant, Grantee, MemberAccess, Role, TreePath, WorkspacePolicy,
    };
    use tilo_action_types::{ActionKind, KindSet};

    let root = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
    let owner = hex::encode(root.verifying_key().to_bytes());
    let author = "b0".repeat(32);
    let member = "c0".repeat(32);
    let id = WorkspaceId::from_u128(61).to_string();

    let base = || {
        let mut policy = WorkspacePolicy::new(id.clone(), owner.clone());
        policy.members.insert(author.clone(), Role::Editor);
        policy.members.insert(member.clone(), Role::Editor);
        policy
    };
    // The agent's own grant: it may take the chat's changes, whichever person's turn it answers,
    // and record a decision on the moderation record.
    let with_agent = || {
        let mut policy = base();
        policy.grants.push(agent_answers::grant(
            &id,
            (SCOUT, owner.as_str()),
            (&AnswerScope::kind(CHAT, CHANNEL), ActionKind::Undoable),
            Answer::Allow,
            Grantee::person(owner.clone()),
        ));
        policy.grants.push(agent_answers::grant(
            &id,
            (SCOUT, owner.as_str()),
            (
                &AnswerScope::kind(CHAT, APP_MODERATION),
                ActionKind::Undoable,
            ),
            Answer::Allow,
            Grantee::person(owner.clone()),
        ));
        policy
    };
    // The member's grants do not allow changing the channel's messages, nor deciding reports.
    let denying = || {
        let mut policy = with_agent();
        for kind in [CHANNEL, APP_MODERATION] {
            policy.grants.push(Grant::new(
                format!("member-no-{kind}"),
                Grantee::person(member.clone()),
                AnswerScope::kind(CHAT, kind).node(&id),
                KindSet::UNDOABLE,
                Answer::Deny,
                Grantee::person(owner.clone()),
            ));
        }
        policy
    };

    // The delete action of the chat, on the channel the message was posted in, decided as an
    // app's window decides it: from the node of the channel's action.
    let delete_at = || {
        tilo_actions::action_node(
            &TreePath::workspace(&id),
            CHAT,
            CHANNEL,
            "general",
            None,
            "delete",
        )
    };
    // The caller the engine builds for a triggered turn of `scout` answering `author`, saved by
    // `saver`: delegated by the agent's person, answering the author, bounded by the saver.
    let caller = |saver: &str| {
        tilo_actions::remote::Caller::delegated_agent(
            SCOUT,
            root.verifying_key().to_bytes(),
            [7; 32],
        )
        .in_workspace(id.clone())
        .on_roster(1, person_bytes(&author))
        .agent_of(saver)
    };
    let removes = |policy: &WorkspacePolicy, saver: &str, messages: &mut Vec<String>| {
        let caller = caller(saver);
        let access =
            MemberAccess::new(policy.clone(), caller.acting().clone()).granted(caller.granted());
        let now = tilo_clock::wall();
        let answered = access.answer(&delete_at(), ActionKind::Undoable, &Context::at_time(now));
        if answered == Answer::Allow {
            messages.retain(|message| message != "m-1");
        }
        answered
    };

    let moderator = with_agent();
    let mut posted = vec!["m-1".to_owned()];
    assert_eq!(
        removes(&moderator, &owner, &mut posted),
        Answer::Allow,
        "the Moderator who saved the watch may delete another's message, and so may the agent"
    );
    assert!(posted.is_empty(), "the message is removed from the channel");

    // The turn also records its decision on the moderation record, under the same line.
    let decides = |policy: &WorkspacePolicy, saver: &str| {
        let caller = caller(saver);
        let access =
            MemberAccess::new(policy.clone(), caller.acting().clone()).granted(caller.granted());
        let node = tilo_actions::action_node(
            &TreePath::workspace(&id),
            CHAT,
            APP_MODERATION,
            "report-1",
            None,
            "decide-reports",
        );
        access.answer(
            &node,
            ActionKind::Undoable,
            &Context::at_time(tilo_clock::wall()),
        )
    };
    assert_eq!(
        decides(&moderator, &owner),
        Answer::Allow,
        "the agent records its decision where it may and the Moderator allows"
    );
    assert_eq!(
        decides(&denying(), &member),
        Answer::Deny,
        "a member who may not decide reports bounds the decision too"
    );

    let mut posted = vec!["m-1".to_owned()];
    assert_eq!(
        removes(&denying(), &member, &mut posted),
        Answer::Deny,
        "a member who saved the watch may not delete another's message, so the turn may not"
    );
    assert_eq!(posted, ["m-1"], "the message stays");

    // The agent's own grants refuse it too, even where the person who saved the watch may: the
    // workspace's answer for `scout` on the channel's kind says no.
    let mut refusing = with_agent();
    refusing.grants.push(agent_answers::grant(
        &id,
        (SCOUT, owner.as_str()),
        (&AnswerScope::kind(CHAT, CHANNEL), ActionKind::Undoable),
        Answer::Deny,
        Grantee::person(owner.clone()),
    ));
    let mut posted = vec!["m-1".to_owned()];
    assert_eq!(
        removes(&refusing, &owner, &mut posted),
        Answer::Deny,
        "the agent's own grants must allow it too"
    );
    assert_eq!(posted, ["m-1"], "the message stays");
}

/// The 32 bytes of a person named by their hex key.
fn person_bytes(person: &str) -> [u8; 32] {
    hex::decode(person)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .unwrap_or_default()
}
