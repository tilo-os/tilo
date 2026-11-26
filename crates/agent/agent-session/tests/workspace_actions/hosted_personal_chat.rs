//! A keeper uses the hosted Chat record their Swift app uses, on a newly admitted device.
use super::super::member_roles::json;
use super::*;

const ASK: &str = "Please repeat the words copper meadow.";

pub(super) struct Conversation {
    workspace: String,
    channel: Id,
    person: String,
    agent: String,
    post: Id,
}

impl Conversation {
    /// Open Chat on this device and find its messages through the published catalogue.
    async fn messages(&self, page: &mut Browser) -> (String, String) {
        let open = tilo_actions::remote::application_open_target(&self.workspace, CHAT);
        let opened = page
            .invoke_on(&open.provider, open.address, InvokeArgument::None)
            .await
            .unwrap()
            .value
            .unwrap();
        let provider = opened["provider"].as_str().unwrap();
        let until = crate::harness::page::write_deadline();
        loop {
            let listed = page
                .catalogue(CatalogueQuery {
                    provider: Some(provider.into()),
                    resource_name: Some("chat.messages".into()),
                    ..Default::default()
                })
                .await
                .unwrap();
            if let Some(read) =
                listed
                    .rows
                    .into_iter()
                    .filter_map(|row| row.action)
                    .find(|action| {
                        action.resource_kind == "chat.messages" && action.address.action == "read"
                    })
            {
                return (provider.into(), read.address.resource);
            }
            assert!(
                Instant::now() < until,
                "the host did not publish Chat messages"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// A signed device reads both the original message and the matching agent's durable reply.
    pub(super) async fn read_reply(&self, page: &mut Browser) {
        let (provider, resource) = self.messages(page).await;
        let reply = chat::chat::reply_id(&self.post, &MemberId::agent(&self.agent));
        let until = tilo_test_env::hang_deadline();
        loop {
            let read = page
                .invoke_on(
                    &provider,
                    ActionAddress::new(&resource, "read"),
                    json(serde_json::json!({
                        "where": {"channel": self.channel},
                        "fields": ["channel", "author", "text"], "pageSize": 10,
                    })),
                )
                .await
                .unwrap()
                .value
                .unwrap();
            let rows = read["messages"].as_array().unwrap();
            if let Some(row) = rows.iter().find(|row| row["id"] == reply.as_str()) {
                let value = &row["value"];
                assert_eq!(value["channel"], self.channel.as_str());
                assert_eq!(value["author"], serde_json::json!({"agent": self.agent}));
                let text = value["text"].as_str().unwrap();
                if text.contains(model::REPLY) && text.contains(ASK) {
                    let posted = rows
                        .iter()
                        .find(|row| row["id"] == self.post.as_str())
                        .unwrap();
                    assert_eq!(
                        posted["value"]["author"],
                        serde_json::json!({"person": self.person})
                    );
                    assert_eq!(posted["value"]["text"], ASK);
                    return;
                }
            }
            assert!(
                Instant::now() < until,
                "the linked model's reply never arrived: {read}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

pub(super) async fn ask(server: &Hosted, assistant: &mut PersonalAssistant) -> Conversation {
    let provider = model::link(server, assistant).await;
    let person = hex::encode(assistant.member.verifying_key().to_bytes());
    let agent = assistant.agent.clone();
    let delivery = InvocationId::new();
    let conversation = Conversation {
        workspace: assistant.workspace_id.to_string(),
        channel: chat::Chat::direct_id([MemberId::person(&person), MemberId::agent(&agent)]),
        person,
        agent,
        post: Id::from_raw(delivery.0.to_string()),
    };
    let (chat, messages) = conversation.messages(&mut assistant.page).await;
    let agents = ServerAgents::with_sessions(
        server.workspaces().unwrap().clone(),
        server.roster.actions.clone(),
        Arc::new(server.server.agent_link()),
        server.roster.sessions.clone(),
    );
    let draining = agents.draining();
    let mut worker = tokio::task::JoinSet::new();
    worker.spawn(agents.run());

    let mut post = request(
        &chat,
        ActionAddress::new(messages, "post"),
        json(serde_json::json!({
            "id": conversation.post, "channel": conversation.channel, "text": ASK, "attachments": [],
        })),
    );
    post.invocation = delivery;
    let posted = assistant.page.invoke_settled(post).await;
    assert!(
        matches!(posted.status, ReceiptStatus::Completed { .. }),
        "{posted:?}"
    );
    conversation.read_reply(&mut assistant.page).await;
    model::assert_streamed(&provider);
    assert!(draining.finish(Duration::from_secs(30)).await);
    worker.shutdown().await;
    conversation
}
