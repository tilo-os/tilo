//! Which workspace observations a person's live assistant grant admits to their Chat.

use super::*;
use crate::workspace_actions::hosted_mail_turn::{a_message_arrives, linked_inbox};
use crate::workspace_actions::hosted_secret_use::Hosted;
use tilo_server_client::MemberApprover;
use tilo_server_client::pairing::{answer_agent_grant, pending_agent_grants};
use tilo_server_protocol::agents::{AgentGrantAction, AgentGrantStatus, GrantState};

pub(super) const WORK_APPS: [&str; 4] = [CHAT, TASKS, ui::apps::ASSISTANT, ui::apps::SHORTCUTS];
const LAST_UPDATE: &str = "The private appointment is at Marlow Quay.";
const SUMMARY: &str = "The private crossing ends at Staveley Pier.";
const FACT: &str = "They prefer the Alden landing.";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_manual_and_a_scheduled_look_use_only_the_assistants_current_grant() {
    let mut hosted = Hosted::start().await;
    let mail = linked_inbox(&hosted).await;
    let workspace = hosted.workspace.clone();
    let workspace_id = WorkspaceId::parse(&workspace).unwrap();
    let person = hosted.person();
    let keeper = hosted.root.verifying_key().to_bytes();
    let me = Caller::authenticated(keeper, hosted.browser.cert.device_public_key)
        .in_workspace(workspace.clone());
    let catalogue = crate::common::catalogue();
    hosted
        .workspaces
        .prepare_application(
            &workspace,
            hosted.workspaces.governor(),
            &catalogue,
            TASKS,
            &workspace_resource(&catalogue, TASKS),
        )
        .await
        .unwrap();
    let listening = hosted.workspaces.listen_for_mail(&workspace);
    hosted.workspaces.mail_watches_settled().await;
    let hands = Arc::new(HandsOver {
        actions: hosted.actions.clone(),
        person: me.clone(),
        conversation: chat::Chat::direct_id([
            MemberId::person(person.clone()),
            MemberId::agent(SCOUT),
        ])
        .to_string(),
        handed: Mutex::default(),
    });
    previous_updates(&hands, &workspace).await;
    hosted
        .turn(
            "Remember my landing preference and confirm the appointment.",
            vec![
                Reply::tool_call(
                    "note-the-preference",
                    "learn",
                    serde_json::json!({"text":FACT,"source":"The keeper told me"}),
                ),
                Reply::text(LAST_UPDATE),
                Reply::text(SUMMARY),
            ],
        )
        .await;
    let (initiative, events) = Initiative::start_with(
        hosted.workspaces.clone(),
        hands.clone(),
        Arc::new(tilo_server::workspace_bots::installed::EventRunner),
    );
    let steps = (initiative.before())(hands.clone());
    let schedule = look_round(
        &initiative,
        &workspace,
        &me,
        workspace_initiative::TURN_ON,
        serde_json::json!({"when": "every 3600 seconds"}),
    )
    .await
    .event
    .unwrap();
    let check = async || {
        take_of(
            &initiative,
            &workspace,
            &me,
            (
                workspace_initiative::LOOK_ROUND,
                workspace_initiative::LOOK_NOW,
            ),
            serde_json::json!({}),
        )
        .await
    };
    let blocked = async || {
        assert!(
            check().await.is_err(),
            "the manual look must refuse the handoff"
        );
        assert!(
            fires(&steps, &person, &workspace, &schedule).await.is_err(),
            "the scheduled job must refuse the handoff"
        );
    };
    // The keeper withdraws the account setup's grant. A pending or denied request
    // and an explicitly approved Chat-only grant all leave the assistant unable to look.
    let before = hosted.model.asked().len();
    member_grant_action(&mut hosted.page, AgentGrantAction::Revoke).await;
    member_grant_action(&mut hosted.page, AgentGrantAction::RequestWork).await;
    blocked().await;
    let approver = MemberApprover::new(ServerAddress::at(hosted.server.node_addr()));
    let proposals = pending_agent_grants(&approver, &hosted.root, workspace_id)
        .await
        .unwrap();
    let [pending] = proposals.as_slice() else {
        panic!("one pending grant")
    };
    answer_agent_grant(&approver, &hosted.root, workspace_id, pending, false)
        .await
        .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let target = AgentGrantAction::Status.target(SCOUT);
        let receipt = hosted
            .page
            .invoke(request(
                &target.provider,
                target.address,
                InvokeArgument::None,
            ))
            .await;
        let ReceiptStatus::Completed { output } = receipt.status else {
            panic!("the keeper could not read the grant")
        };
        let status: AgentGrantStatus = serde_json::from_value(output.value.unwrap()).unwrap();
        if status.state == GrantState::Denied {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the keeper's denial never took effect"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    blocked().await;
    member_grant_action(&mut hosted.page, AgentGrantAction::RequestChat).await;
    approve_member_grant(&hosted.server, &hosted.root, workspace_id, &[CHAT]).await;
    blocked().await;
    assert!(hands.handed.lock().unwrap().is_empty());
    assert_eq!(hosted.model.asked().len(), before);

    // A Work grant admits Task metadata. The person's own mail listener stays live,
    // so an arrival after the first successful look is genuinely new in this inbox.
    member_grant_action(&mut hosted.page, AgentGrantAction::Revoke).await;
    member_grant_action(&mut hosted.page, AgentGrantAction::RequestWork).await;
    approve_member_grant(&hosted.server, &hosted.root, workspace_id, &WORK_APPS).await;
    fires(&steps, &person, &workspace, &schedule).await.unwrap();
    assert!(hands.handed.lock().unwrap().is_empty());
    a_message_arrives(&hosted, &mail);
    assert_eq!(
        hosted
            .workspaces
            .mail_arrivals_for(workspace_id, &keeper, 0)
            .await
            .arrivals
            .len(),
        1
    );
    files(&hosted.actions, &workspace, &me, "Call the bank").await;
    let result: LookRound = serde_json::from_value(check().await.unwrap()).unwrap();
    let look = result.looks.first().unwrap();
    assert_eq!(look.ended, LookEnded::Told);
    assert!(look.found.changed > 0);
    assert_eq!(look.found.came_in, 0);
    {
        let posted = hands.handed.lock().unwrap();
        assert_eq!(posted.len(), 1);
        let findings: serde_json::Value =
            serde_json::from_str(posted[0].lines().last().unwrap()).unwrap();
        assert_eq!(
            findings.pointer("/cameIn/mail"),
            Some(&serde_json::json!(0))
        );
        assert!(
            findings["changed"]["named"]
                .as_array()
                .unwrap()
                .iter()
                .any(|change| change["app"] == TASKS)
        );
    }

    // The server has already dequeued the look, but has made no provider call.
    // The narrower replacement must still prevent that specific Chat turn.
    hosted.agents = hosted
        .agents
        .clone()
        .with_initiative(Some(initiative.clone()))
        .with_events(Some(events));
    let queued = hosted.agents.turns(&workspace).await.unwrap();
    let stale = chat::cause::message_id(&MemberId::person(person.clone()), &look.id);
    let turn = queued
        .iter()
        .find(|turn| matches!(&turn.ask, Ask::Message {message, ..} if message == stale.as_str()))
        .expect("the observation the assistant owes");
    let scout = MemberId::agent(SCOUT);
    let stale_reply = chat::chat::reply_id(&stale, &scout);
    member_grant_action(&mut hosted.page, AgentGrantAction::Revoke).await;
    files(&hosted.actions, &workspace, &me, "After revocation").await;
    blocked().await;
    member_grant_action(&mut hosted.page, AgentGrantAction::RequestChat).await;
    approve_member_grant(&hosted.server, &hosted.root, workspace_id, &[CHAT]).await;
    hosted.agents.answer(&workspace, turn).await.unwrap();
    assert_eq!(hosted.model.asked().len(), before);
    assert!(
        !conversation_part(&hosted.workspaces, &workspace)
            .await
            .messages
            .contains(&stale_reply)
    );
    let reply = "I can answer this new message.";
    let fresh = write_to_agent(
        &hosted.actions,
        &hosted.workspaces,
        &workspace,
        (&hosted.root, &hosted.browser),
        "Hello after renewing Chat.",
    )
    .await;
    let turns = hosted.agents.turns(&workspace).await.unwrap();
    let fresh_turn = turns
        .iter()
        .find(|turn| turn.typed && turn.cause == fresh.as_str())
        .unwrap();
    let recalled = chat::recall::instruction_with_history(&fresh_turn.request, &fresh_turn.history);
    for private in [LAST_UPDATE, SUMMARY, FACT] {
        assert!(recalled.contains(private), "{recalled}");
    }
    hosted.model.then(Reply::text(reply));
    hosted.agents.answer(&workspace, fresh_turn).await.unwrap();
    let messages = conversation_part(&hosted.workspaces, &workspace)
        .await
        .messages;
    assert!(
        messages
            .iter()
            .any(|(_, message)| message.text == reply && message.author == scout)
    );
    assert!(!messages.contains(&stale_reply));
    assert_eq!(hosted.model.asked().len(), before + 1);

    member_grant_action(&mut hosted.page, AgentGrantAction::Revoke).await;
    member_grant_action(&mut hosted.page, AgentGrantAction::RequestWork).await;
    approve_member_grant(&hosted.server, &hosted.root, workspace_id, &WORK_APPS).await;
    // A new work grant admits fresh findings. The older queued look stays refused, and
    // neither the earlier private discussion nor its saved summary reaches this model call.
    fires(&steps, &person, &workspace, &schedule).await.unwrap();
    let turns = hosted.agents.turns(&workspace).await.unwrap();
    let automatic = turns
        .iter()
        .find(|turn| turn.look_grant.is_some() && turn.cause != stale.as_str())
        .expect("a new authorized look can run after a stale one");
    assert!(automatic.history.is_empty());
    assert!(automatic.fold.is_none());
    for private in [LAST_UPDATE, SUMMARY, FACT] {
        assert!(!automatic.request.contains(private));
    }
    assert!(automatic.request.contains(TASKS), "{}", automatic.request);
    hosted.model.then(Reply::text("One fresh task changed."));
    hosted.agents.answer(&workspace, automatic).await.unwrap();
    let asked = hosted.model.asked();
    assert_eq!(asked.len(), before + 2);
    let sent = format!("{:?}", asked.last().unwrap().request.messages);
    assert!(sent.contains(TASKS));
    for private in [LAST_UPDATE, SUMMARY, FACT] {
        assert!(!sent.contains(private));
    }
    assert_eq!(hosted.server.agents.supervisor().expire(u64::MAX), [SCOUT]);
    blocked().await;
    assert_eq!(hands.handed.lock().unwrap().len(), 2);
    assert_eq!(hosted.model.asked().len(), before + 2);
    drop(listening);
}
