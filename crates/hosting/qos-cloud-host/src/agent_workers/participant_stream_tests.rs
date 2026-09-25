//! Streamed agent typing over the real host worker path (lab proof6,
//! 2026-09-24: type-text continuation chunks landed inside the first
//! paragraph at a stale participant caret instead of after the paragraph the
//! same task had just appended).
//!
//! Two enrolled tasks run back to back on the real certified Writer worker,
//! each exactly as the Agent streams a reply: `append-paragraph` with the
//! first chunk, then `type-text` continuation chunks, all through the
//! enrolled step service (so the broker sets the task's participant on each
//! invocation). The second task must not inherit the first task's caret.
use super::*;

fn writer_texts(owner: &RuntimeOwner) -> Vec<String> {
    let bytes = owner
        .store()
        .unwrap()
        .peer()
        .unwrap()
        .document(WRITER, RECORD)
        .unwrap();
    let document = ui::automerge::AutoCommit::load(&bytes).unwrap();
    let model = qdoc::model::open(&document, &ui::automerge::ROOT).unwrap();
    qdoc::model::materialize(&document, &model)
        .blocks
        .into_iter()
        .map(|block| block.text)
        .collect()
}

/// One task's streamed reply: `append-paragraph(first)` then `type-text` for
/// every later chunk, step by step, each answered before the next.
async fn stream_reply(
    workers: &mut AgentWorkers,
    documents: &mut mpsc::Receiver<PendingHostDocument>,
    network: &mut mpsc::Receiver<NeighbourMsg>,
    chunks: &[&str],
) -> String {
    let task = enrolled_task(workers, documents, network, true).await;
    let broker = workers.app_broker().unwrap();
    let dispatch = broker
        .enrolled_task_dispatch(&task.channel, task.invocation)
        .unwrap();
    for (step, chunk) in chunks.iter().enumerate() {
        let (peer, resource) = writer_peer(workers);
        let action = if step == 0 {
            "append-paragraph"
        } else {
            "type-text"
        };
        let mut answer = workers
            .step_service
            .submit(qos_action_broker::enrolled_dispatch::EnrolledStepRequest {
                dispatch: dispatch.clone(),
                step: u32::try_from(step).unwrap(),
                peer,
                resource,
                action: action.into(),
                argument: Some((*chunk).into()),
            })
            .unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let reply = loop {
            workers.pump().unwrap();
            drain(workers, documents, network);
            if let Ok(reply) = answer.try_recv() {
                break reply.unwrap();
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{action} step {step} never answered"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        };
        assert!(
            reply.update_json.contains("\"succeeded\""),
            "{action} step {step}: {}",
            reply.update_json
        );
    }
    // Let the worker settle the last commit.
    for _ in 0..3 {
        workers.pump().unwrap();
        drain(workers, documents, network);
    }
    chunks.concat()
}

#[tokio::test(flavor = "current_thread")]
async fn streamed_type_text_continues_the_appended_paragraph() {
    let (directory, owner) = super::tests::selected();
    let (mut workers, mut documents, mut network) = started(directory.path(), owner).await;
    let before = writer_texts(&workers.owner);

    let first = stream_reply(
        &mut workers,
        &mut documents,
        &mut network,
        &[
            "Sea turtles are gentle travelers ",
            "of the ocean, spending much ",
            "of their lives at sea.",
        ],
    )
    .await;
    let after_first = writer_texts(&workers.owner);
    assert_eq!(after_first.last(), Some(&first), "{after_first:#?}");
    // append-paragraph fills a trailing empty paragraph instead of adding one.
    let filled: Vec<_> = before.iter().filter(|text| !text.is_empty()).collect();
    assert_eq!(
        after_first[..after_first.len() - 1]
            .iter()
            .collect::<Vec<_>>(),
        filled,
        "earlier paragraphs untouched: {after_first:#?}"
    );

    let second = stream_reply(
        &mut workers,
        &mut documents,
        &mut network,
        &[
            "Sea otters are playful, ",
            "intelligent swimmers that live ",
            "along the coasts of the Pacific.",
        ],
    )
    .await;
    let after_second = writer_texts(&workers.owner);
    assert_eq!(after_second.last(), Some(&second), "{after_second:#?}");
    assert_eq!(
        after_second[..after_second.len() - 1],
        after_first[..],
        "the first task's paragraph untouched: {after_second:#?}"
    );

    // The second task's caret is published at the end of its paragraph,
    // under the paragraph's `wp_block` key, in bytes.
    let last_block = {
        let bytes = workers
            .owner
            .store()
            .unwrap()
            .peer()
            .unwrap()
            .document(WRITER, RECORD)
            .unwrap();
        let document = ui::automerge::AutoCommit::load(&bytes).unwrap();
        let model = qdoc::model::open(&document, &ui::automerge::ROOT).unwrap();
        qdoc::model::blocks_in_order(&document, &model)
            .pop()
            .unwrap()
    };
    let key = ui::PresenceKey::Widget {
        path: vec![
            ui::PathSegment::Field("wp_block".into()),
            ui::PathSegment::Field(last_block),
        ],
    };
    let writer = workers
        .workers
        .iter()
        .find(|w| w.app == WRITER && w.record == RECORD)
        .unwrap();
    let presence = writer.runner.participant_presence();
    assert!(
        presence
            .iter()
            .any(|p| p.key == key && p.payload == ui::PresencePayload::Caret { pos: second.len() }),
        "{presence:#?}"
    );
}

/// MUST-13: an agent task's Chat `post` (the ordinary published operation,
/// through the enrolled step service, so the broker sets the task participant
/// on the invocation) is authored by that agent task and carries the agent's
/// display name for every member; the Chat worker's own identity is not the
/// author.
#[tokio::test(flavor = "current_thread")]
async fn agent_chat_post_is_authored_by_the_invoking_agent() {
    const CHAT: &str = "so.tilo.chat";
    let (directory, owner) = super::tests::selected();
    let (mut workers, mut documents, mut network) = started(directory.path(), owner).await;
    let grants = workers.publications().unwrap().grants;
    let task = enrolled_task(&mut workers, &mut documents, &mut network, true).await;
    let task_id = task.acceptance.task_id.clone();
    let active = [ActiveAgentTask {
        task: task_id.clone(),
        agent: "agent-1".into(),
        name: "Tide".into(),
        grants,
    }];
    for _ in 0..2 {
        workers.sync_agent_participants(&active).unwrap();
        workers.pump().unwrap();
        drain(&mut workers, &mut documents, &mut network);
    }
    let (peer, resource) = workers
        .broker
        .published_action_surfaces()
        .into_iter()
        .filter(|s| s.live && s.app_id == CHAT)
        .find_map(|s| {
            s.actions
                .iter()
                .find(|a| {
                    a.action == "post"
                        && a.resource.ends_with(&hex::encode("chat.channel:c_general"))
                })
                .map(|a| (s.peer_id, a.resource.clone()))
        })
        .expect("live Chat post on #general");
    let broker = workers.app_broker().unwrap();
    let dispatch = broker
        .enrolled_task_dispatch(&task.channel, task.invocation)
        .unwrap();
    let mut answer = workers
        .step_service
        .submit(qos_action_broker::enrolled_dispatch::EnrolledStepRequest {
            dispatch,
            step: 0,
            peer,
            resource,
            action: "post".into(),
            argument: Some("hello from Tide".into()),
        })
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let reply = loop {
        workers.pump().unwrap();
        drain(&mut workers, &mut documents, &mut network);
        if let Ok(reply) = answer.try_recv() {
            break reply.unwrap();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "post never answered"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };
    assert!(
        reply.update_json.contains("\"succeeded\""),
        "{}",
        reply.update_json
    );
    for _ in 0..3 {
        workers.pump().unwrap();
        drain(&mut workers, &mut documents, &mut network);
    }
    let bytes = workers
        .owner
        .store()
        .unwrap()
        .peer()
        .unwrap()
        .document(CHAT, "")
        .unwrap();
    let document = ui::automerge::AutoCommit::load(&bytes).unwrap();
    let message = chat::doc::ChatDoc::list_messages(&document, "c_general")
        .into_iter()
        .find(|m| m.text == "hello from Tide")
        .expect("agent post in #general");
    assert_eq!(message.author, format!("agent-task:{task_id}"));
    assert_eq!(
        chat::doc::ChatDoc::agent_authors(&document)
            .get(&message.author)
            .map(String::as_str),
        Some("Tide")
    );
}
