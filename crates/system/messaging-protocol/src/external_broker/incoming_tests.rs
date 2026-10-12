use super::receipt_tests::{Fixture, fixture, messages, sdk};
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use whatsapp_rust::wacore::types::message::MessageSource;

fn incoming(route: &Route, text: &str) -> whatsapp_rust::wacore::types::events::InboundMessage {
    use whatsapp_rust::wacore::proto_helpers::MessageBuilderExt;
    use whatsapp_rust::wacore::types::message::MessageInfo;
    whatsapp_rust::wacore::types::events::InboundMessage::builder()
        .message(Arc::new(whatsapp_rust::waproto::whatsapp::Message::text(
            text,
        )))
        .info(Arc::new(MessageInfo {
            source: MessageSource {
                chat: route.recipient.parse().unwrap(),
                sender: route.recipient.parse().unwrap(),
                ..Default::default()
            },
            id: "inbound-stanza".into(),
            timestamp: "2026-10-06T00:00:00Z".parse().unwrap(),
            ..Default::default()
        }))
        .build()
}
async fn admit(
    fixture: &Fixture,
    event: whatsapp_rust::wacore::types::events::InboundMessage,
) -> Result<bool, SendError> {
    fixture
        .broker
        .observe_whatsapp_message(fixture.source.clone(), event)
        .await
}
#[tokio::test]
async fn actual_whatsapp_inbound_is_private_durable_and_idempotent_without_network_or_ack() {
    let fixture = fixture().await;
    let wakes = Arc::new(AtomicUsize::new(0));
    let changed = wakes.clone();
    let guard = fixture
        .broker
        .observe(Arc::new(move || {
            changed.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();
    let event = incoming(&fixture.route, "private incoming text");
    assert!(admit(&fixture, event.clone()).await.unwrap());
    assert!(
        !admit(&fixture, event.clone()).await.unwrap(),
        "same SDK redelivery must be idempotent"
    );
    let mut conflicting = event.clone();
    Arc::make_mut(&mut conflicting.message).conversation = Some("different private text".into());
    assert_eq!(
        admit(&fixture, conflicting).await,
        Err(SendError::InvalidRequest)
    );
    let mut changed_time = event;
    Arc::make_mut(&mut changed_time.info).timestamp = "2026-10-06T00:00:01Z".parse().unwrap();
    assert_eq!(
        admit(&fixture, changed_time).await,
        Err(SendError::InvalidRequest)
    );
    let mut extended = incoming(&fixture.route, "extended private text");
    Arc::make_mut(&mut extended.info).id = "extended-stanza".into();
    let message = Arc::make_mut(&mut extended.message);
    message.conversation = None;
    message.extended_text_message.get_or_insert_default().text =
        Some("extended private text".into());
    assert!(admit(&fixture, extended).await.unwrap());
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
    let page = fixture
        .broker
        .client()
        .history()
        .read(fixture.route.clone())
        .await
        .unwrap();
    assert_eq!(page.messages.len(), 3);
    let incoming_rows: Vec<_> = page
        .messages
        .iter()
        .filter(|m| m.direction == tilo_messaging::Direction::Incoming)
        .collect();
    assert_eq!(incoming_rows.len(), 2);
    assert!(incoming_rows.iter().all(|m| m.delivery.is_none()));
    assert!(
        incoming_rows
            .iter()
            .any(|m| m.text.as_str() == "private incoming text")
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.revoked.store(true, Ordering::SeqCst);
    assert_eq!(
        admit(&fixture, incoming(&fixture.route, "revoked text")).await,
        Err(SendError::Revoked)
    );
    assert_eq!(
        fixture
            .broker
            .client()
            .history()
            .read(fixture.route.clone())
            .await,
        Err(HistoryError::Revoked)
    );
    let Fixture {
        root,
        owner,
        route,
        broker,
        ..
    } = fixture;
    drop(guard);
    drop(broker);
    let reopened = History::open(
        &root.path().join("history"),
        &owner,
        secret_vault::VaultKey::from_bytes([8; 32]),
    )
    .unwrap();
    let page = reopened.page(route, 128).unwrap();
    assert_eq!(page.messages.len(), 3);
    assert!(
        page.messages
            .iter()
            .any(|m| m.text.as_str() == "private incoming text")
    );
    let ciphertext = std::fs::read(root.path().join("history/history.enc")).unwrap();
    assert!(
        !ciphertext
            .windows(b"private incoming text".len())
            .any(|b| b == b"private incoming text")
    );
}
#[tokio::test]
async fn foreign_or_unsupported_whatsapp_inbound_cannot_mutate_private_history() {
    let fixture = fixture().await;
    let foreign = sdk(&fixture.calls).await;
    assert_eq!(
        fixture
            .broker
            .observe_whatsapp_message(foreign, incoming(&fixture.route, "foreign text"))
            .await,
        Err(SendError::Revoked)
    );
    for mutation in [
        "sender",
        "chat",
        "self",
        "group",
        "broadcast",
        "expiry",
        "placeholder",
        "edit",
        "multicast",
        "wrapper",
        "empty-wrapper",
        "empty-media",
        "mixed",
        "blank",
        "large",
        "zero-time",
        "long-id",
    ] {
        let mut event = incoming(&fixture.route, "untrusted text");
        let info = Arc::make_mut(&mut event.info);
        match mutation {
            "sender" => info.source.sender = "12025550124@s.whatsapp.net".parse().unwrap(),
            "chat" => info.source.chat = "12025550124@s.whatsapp.net".parse().unwrap(),
            "self" => info.source.is_from_me = true,
            "group" => info.source.is_group = true,
            "broadcast" => {
                info.source.broadcast_list_owner = Some(fixture.route.recipient.parse().unwrap());
            }
            "expiry" => info.ephemeral_expiration = Some(86400),
            "placeholder" => info.unavailable_request_id = Some("recovery-request".into()),
            "edit" => info.edit = whatsapp_rust::wacore::types::message::EditAttribute::MessageEdit,
            "multicast" => info.multicast = true,
            "zero-time" => info.timestamp = "1970-01-01T00:00:00Z".parse().unwrap(),
            "long-id" => info.id = "x".repeat(513),
            "wrapper" => {
                Arc::make_mut(&mut event.message)
                    .ephemeral_message
                    .get_or_insert_default()
                    .message
                    .get_or_insert_default()
                    .conversation = Some("wrapped text".into());
            }
            "empty-wrapper" => {
                Arc::make_mut(&mut event.message)
                    .view_once_message
                    .get_or_insert_default();
            }
            "empty-media" => {
                Arc::make_mut(&mut event.message)
                    .image_message
                    .get_or_insert_default();
            }
            "mixed" => {
                Arc::make_mut(&mut event.message)
                    .image_message
                    .get_or_insert_default()
                    .caption = Some("image text".into());
            }
            "blank" => Arc::make_mut(&mut event.message).conversation = Some(" ".into()),
            _ => Arc::make_mut(&mut event.message).conversation = Some("x".repeat(32769)),
        }
        assert!(
            admit(&fixture, event).await.is_err(),
            "must refuse {mutation}"
        );
        assert_eq!(
            fixture
                .broker
                .client()
                .history()
                .read(fixture.route.clone())
                .await
                .unwrap()
                .messages
                .len(),
            1,
            "{mutation} must not create a row"
        );
    }
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn actual_whatsapp_inbound_wakes_selected_messages_view_without_shared_plaintext() {
    let fixture = fixture().await;
    let mut app = messages(&fixture);
    assert!(
        admit(
            &fixture,
            incoming(&fixture.route, "new private WhatsApp arrival")
        )
        .await
        .unwrap()
    );
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(5), |h| {
                h.a11y_nodes()
                    .iter()
                    .any(|(_, node)| node.label() == Some("new private WhatsApp arrival"))
            }),
        "native inbound arrival must refresh the selected private view"
    );
    assert!(
        !app.harness()
            .dump_json()
            .contains("new private WhatsApp arrival")
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
}

struct TwoContacts {
    primary: tests::Gate,
    other: Route,
}
impl ExternalAuthority for TwoContacts {
    fn authorize(&self, route: &Route) -> Result<Authorization, SendError> {
        let current = tests::route(crate::Service::Whatsapp);
        let mut auth = self.primary.authorize(&current)?;
        if route == &current {
            return Ok(auth);
        }
        if route != &self.other {
            return Err(SendError::Revoked);
        }
        auth.directory
            .project_contact(self.other.person.clone(), "Alex".into())
            .map_err(|_| SendError::Revoked)?;
        auth.directory
            .confirm_account(
                self.other.person.clone(),
                self.other.association.clone(),
                self.other.service.clone(),
                self.other.scope.clone(),
                self.other.recipient.clone(),
                self.other.revision.clone(),
            )
            .map_err(|_| SendError::Revoked)?;
        auth.route = self.other.clone();
        Ok(auth)
    }
    fn incoming(&self, recipient: &str) -> Result<Authorization, SendError> {
        let route = if recipient == self.other.recipient {
            self.other.clone()
        } else {
            tests::route(crate::Service::Whatsapp)
        };
        if route.recipient != recipient {
            return Err(SendError::Revoked);
        }
        self.authorize(&route)
    }
}
#[tokio::test]
async fn same_whatsapp_stanza_id_from_two_direct_chats_never_merges_people_or_messages() {
    let mut fixture = fixture().await;
    let mut other = fixture.route.clone();
    other.person = ui::prelude::Id::from_raw("00000000-0000-4000-8000-000000000002");
    other.association = ui::prelude::Id::from_raw("other-association");
    other.recipient = "12025550124@s.whatsapp.net".into();
    fixture.broker.authority = Arc::new(TwoContacts {
        primary: tests::Gate {
            owner: fixture.owner.clone(),
            revoked: fixture.revoked.clone(),
        },
        other: other.clone(),
    });
    assert!(
        admit(&fixture, incoming(&fixture.route, "first person's text"))
            .await
            .unwrap()
    );
    assert!(
        admit(&fixture, incoming(&other, "second person's text"))
            .await
            .unwrap()
    );
    let history = fixture.broker.client().history();
    let first = history.read(fixture.route.clone()).await.unwrap();
    let second = history.read(other.clone()).await.unwrap();
    let first_in = first
        .messages
        .iter()
        .find(|m| m.direction == tilo_messaging::Direction::Incoming)
        .unwrap();
    assert_eq!(first.messages.len(), 2);
    assert_eq!(second.messages.len(), 1);
    assert_eq!(first_in.text.as_str(), "first person's text");
    assert_eq!(second.messages[0].text.as_str(), "second person's text");
    assert_ne!(first_in.id, second.messages[0].id);
    assert!(
        !admit(&fixture, incoming(&other, "second person's text"))
            .await
            .unwrap()
    );
    assert_eq!(history.read(other).await.unwrap().messages.len(), 1);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn ordinary_whatsapp_transport_metadata_stays_in_sdk_custody_and_never_weakens_retention() {
    let fixture = fixture().await;
    let mut event = incoming(&fixture.route, "text with native transport metadata");
    let context = Arc::make_mut(&mut event.message)
        .message_context_info
        .get_or_insert_default();
    context.message_secret = Some(vec![171; 32]);
    context.reporting_token_version = Some(2);
    context.device_list_metadata_version = Some(2);
    let device = context.device_list_metadata.get_or_insert_default();
    device.sender_key_hash = Some(vec![1; 16]);
    device.sender_timestamp = Some(1);
    device.sender_key_indexes = vec![1, 2];
    device.recipient_key_hash = Some(vec![2; 16]);
    device.recipient_timestamp = Some(2);
    device.recipient_key_indexes = vec![3];
    assert!(admit(&fixture, event.clone()).await.unwrap());
    let page = fixture
        .broker
        .client()
        .history()
        .read(fixture.route.clone())
        .await
        .unwrap();
    assert!(
        page.messages
            .iter()
            .any(|m| m.text.as_str() == "text with native transport metadata")
    );
    let plain = {
        let worker = fixture.broker.worker.lock().await;
        let auth = fixture.broker.authorized(&fixture.route).unwrap();
        let native = crate::Route::new(
            crate::Service::Whatsapp,
            fixture.owner.account.clone(),
            fixture.route.recipient.clone(),
        )
        .unwrap();
        worker
            .adapter
            .whatsapp_incoming(
                &crate::whatsapp_runtime::IncomingContext {
                    accounts: &auth.accounts,
                    binding: &auth.binding,
                    source: &fixture.source,
                    route: &native,
                },
                &event,
            )
            .unwrap()
    };
    assert_eq!(plain.text.as_str(), "text with native transport metadata");
    for mutation in [
        "sharing",
        "addon",
        "long-secret",
        "large-indexes",
        "large-hash",
    ] {
        let mut altered = event.clone();
        Arc::make_mut(&mut altered.info).id = mutation.into();
        let context = Arc::make_mut(&mut altered.message)
            .message_context_info
            .get_or_insert_default();
        match mutation {
            "sharing" => {
                context.limit_sharing.get_or_insert_default();
            }
            "addon" => context.message_add_on_duration_in_secs = Some(86400),
            "long-secret" => context.message_secret = Some(vec![171; 33]),
            "large-indexes" => {
                context
                    .device_list_metadata
                    .get_or_insert_default()
                    .sender_key_indexes = vec![1; 65];
            }
            _ => {
                context
                    .device_list_metadata
                    .get_or_insert_default()
                    .recipient_key_hash = Some(vec![1; 33]);
            }
        }
        assert!(
            admit(&fixture, altered).await.is_err(),
            "must refuse {mutation}"
        );
    }
    assert_eq!(
        fixture
            .broker
            .client()
            .history()
            .read(fixture.route.clone())
            .await
            .unwrap()
            .messages
            .len(),
        2
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn sdk_durability_hook_commits_batches_atomically_before_success() {
    use whatsapp_rust::types::durability_hook::InboundDurabilityHook;
    let (fixture, hook) = super::receipt_tests::durability_fixture().await;
    let event = incoming(&fixture.route, "durable first text");
    let broker = Arc::new(fixture.broker.clone());
    hook.bind(&broker, &fixture.source).await.unwrap();
    let mut app = messages(&fixture);
    let wakes = Arc::new(AtomicUsize::new(0));
    let counted = wakes.clone();
    let guard = broker
        .observe(Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();
    let mut invalid = event.clone();
    Arc::make_mut(&mut invalid.info).id = "unsupported-batch-item".into();
    Arc::make_mut(&mut invalid.message)
        .view_once_message
        .get_or_insert_default();
    assert!(
        hook.on_messages(fixture.source.clone(), &[event.clone(), invalid])
            .await
            .is_err()
    );
    assert_eq!(history_count(&broker, &fixture.route).await, 1);
    assert_eq!(wakes.load(Ordering::SeqCst), 0);
    let mut second = event.clone();
    Arc::make_mut(&mut second.info).id = "second-stanza".into();
    Arc::make_mut(&mut second.message).conversation = Some("durable second text".into());
    let mut conflict = second.clone();
    Arc::make_mut(&mut conflict.message).conversation = Some("conflicting batch text".into());
    assert!(
        hook.on_messages(fixture.source.clone(), &[second.clone(), conflict])
            .await
            .is_err()
    );
    assert_eq!(history_count(&broker, &fixture.route).await, 1);
    let batch = vec![event.clone(), second.clone(), second];
    hook.on_messages(fixture.source.clone(), &batch)
        .await
        .unwrap();
    hook.on_messages(fixture.source.clone(), &batch)
        .await
        .unwrap();
    assert_eq!(history_count(&broker, &fixture.route).await, 3);
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(5), |h| {
                h.a11y_nodes()
                    .iter()
                    .any(|(_, node)| node.label() == Some("durable second text"))
            })
    );
    assert!(!app.harness().dump_json().contains("durable first text"));
    assert!(!app.harness().dump_json().contains("durable second text"));
    fixture.revoked.store(true, Ordering::SeqCst);
    assert!(
        hook.on_messages(fixture.source.clone(), &batch)
            .await
            .is_err()
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.revoked.store(false, Ordering::SeqCst);
    drop(app);
    drop(guard);
    drop(broker);
    let Fixture {
        root,
        owner,
        route,
        broker,
        ..
    } = fixture;
    drop(broker);
    let reopened = History::open(
        &root.path().join("history"),
        &owner,
        secret_vault::VaultKey::from_bytes([8; 32]),
    )
    .unwrap();
    assert_eq!(reopened.page(route, 128).unwrap().messages.len(), 3);
    let ciphertext = std::fs::read(root.path().join("history/history.enc")).unwrap();
    assert!(
        !ciphertext
            .windows(b"durable first text".len())
            .any(|b| b == b"durable first text")
    );
}

#[tokio::test]
async fn durability_hook_refuses_limits_and_storage_failure_without_success_or_view_wake() {
    use whatsapp_rust::types::durability_hook::InboundDurabilityHook;
    let (fixture, hook) = super::receipt_tests::durability_fixture().await;
    let broker = Arc::new(fixture.broker.clone());
    hook.bind(&broker, &fixture.source).await.unwrap();
    let event = incoming(&fixture.route, "not committed on failure");
    assert!(hook.on_messages(fixture.source.clone(), &[]).await.is_err());
    assert!(
        hook.on_messages(fixture.source.clone(), &vec![event.clone(); 65])
            .await
            .is_err()
    );
    let mut batch = Vec::new();
    for index in 0..9 {
        let mut item = incoming(&fixture.route, &"x".repeat(32768));
        Arc::make_mut(&mut item.info).id = format!("large-item-{index}");
        batch.push(item);
    }
    assert!(
        hook.on_messages(fixture.source.clone(), &batch)
            .await
            .is_err()
    );
    assert_eq!(
        broker
            .client()
            .history()
            .read(fixture.route.clone())
            .await
            .unwrap()
            .messages
            .len(),
        1
    );
    let wakes = Arc::new(AtomicUsize::new(0));
    let counted = wakes.clone();
    let _guard = broker
        .observe(Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();
    // Obstruct the actual atomic store destination in this private fixture.
    let path = fixture.root.path().join("history/history.enc");
    let previous = fixture.root.path().join("history/previous.enc");
    let before = std::fs::read(&path).unwrap();
    std::fs::rename(&path, &previous).unwrap();
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("blocker"), b"private fixture blocker").unwrap();
    assert!(
        hook.on_messages(fixture.source.clone(), std::slice::from_ref(&event))
            .await
            .is_err()
    );
    assert_eq!(wakes.load(Ordering::SeqCst), 0);
    assert_eq!(
        broker.client().history().read(fixture.route.clone()).await,
        Err(HistoryError::Unavailable)
    );
    assert_eq!(std::fs::read(previous).unwrap(), before);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn sdk_durability_hook_refuses_foreign_replaced_and_lost_supervisors() {
    use whatsapp_rust::types::durability_hook::InboundDurabilityHook;
    let (fixture, hook) = super::receipt_tests::durability_fixture().await;
    let event = incoming(&fixture.route, "supervised direct text");
    let batch = std::slice::from_ref(&event);
    assert!(
        hook.on_messages(fixture.source.clone(), batch)
            .await
            .is_err()
    );
    let foreign = sdk(&fixture.calls).await;
    let broker = Arc::new(fixture.broker.clone());
    assert_eq!(hook.bind(&broker, &foreign).await, Err(SendError::Revoked));
    let other = super::receipt_tests::fixture().await;
    assert_eq!(
        hook.bind(&Arc::new(other.broker), &fixture.source).await,
        Err(SendError::Revoked)
    );
    let mut wrong_owner = fixture.owner.clone();
    wrong_owner.linking_generation = 2;
    let wrong_hook = WhatsappInboxHook::new(wrong_owner).unwrap();
    assert_eq!(
        wrong_hook.bind(&broker, &fixture.source).await,
        Err(SendError::Revoked)
    );
    hook.bind(&broker, &fixture.source).await.unwrap();
    assert_eq!(
        hook.bind(&broker, &fixture.source).await,
        Err(SendError::AlreadyReserved)
    );
    hook.on_messages(fixture.source.clone(), batch)
        .await
        .unwrap();
    assert!(hook.on_messages(foreign, batch).await.is_err());
    assert!(fixture.broker.client().supports(&fixture.route));
    drop(broker);
    // Current grants and a client loan remain valid; only the supervisor is gone.
    assert!(fixture.broker.client().supports(&fixture.route));
    assert!(
        hook.on_messages(fixture.source.clone(), batch)
            .await
            .is_err()
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
}

async fn history_count(broker: &ExternalBroker, route: &Route) -> usize {
    broker
        .client()
        .history()
        .read(route.clone())
        .await
        .unwrap()
        .messages
        .len()
}
