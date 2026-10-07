use super::*;
use crate::external_broker::receipt_tests::{fixture, receipt, sdk};
use tilo_messaging::Delivery;
use whatsapp_rust::wacore::types::presence::ReceiptType;

#[tokio::test]
async fn subscribed_handler_drains_exact_receipts_into_private_history_without_network() {
    let fixture = fixture().await;
    let broker = Arc::new(fixture.broker.clone());
    let mut subscription = WhatsappReceiptSubscription::attach(&broker, fixture.source.clone())
        .await
        .unwrap();
    // Feed the registered production handler at the SDK event boundary. The SDK
    // client's internal bus is private; this fixture never connects to a server.
    let mut event = receipt(&fixture.route, ReceiptType::Delivered);
    event.message_ids = vec!["unrelated".into(), "operation".into(), "operation".into()];
    subscription
        .handler
        .handle_event(Arc::new(Event::Receipt(event)));
    assert_eq!(subscription.next().await, Ok(1));
    let history = broker.client().history();
    assert_eq!(
        history.read(fixture.route.clone()).await.unwrap().messages[0].delivery,
        Some(Delivery::RecipientDelivered)
    );
    subscription
        .handler
        .handle_event(Arc::new(Event::Receipt(receipt(
            &fixture.route,
            ReceiptType::Read,
        ))));
    assert_eq!(subscription.next().await, Ok(1));
    assert_eq!(
        history.read(fixture.route.clone()).await.unwrap().messages[0].delivery,
        Some(Delivery::RecipientRead)
    );
    fixture.revoked.store(true, Ordering::SeqCst);
    subscription
        .handler
        .handle_event(Arc::new(Event::Receipt(receipt(
            &fixture.route,
            ReceiptType::Read,
        ))));
    assert_eq!(subscription.next().await, Err(SendError::Revoked));
    assert_eq!(subscription.next().await, Err(SendError::Unavailable));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn subscriptions_refuse_foreign_sources_and_supervisor_loss() {
    let fixture = fixture().await;
    let broker = Arc::new(fixture.broker.clone());
    let foreign = sdk(&fixture.calls).await;
    assert!(matches!(
        WhatsappReceiptSubscription::attach(&broker, foreign).await,
        Err(SendError::Revoked)
    ));
    let mut subscription = WhatsappReceiptSubscription::attach(&broker, fixture.source.clone())
        .await
        .unwrap();
    drop(broker);
    // Even another cloned worker/client does not keep this supervisor binding.
    subscription
        .handler
        .handle_event(Arc::new(Event::Receipt(receipt(
            &fixture.route,
            ReceiptType::Read,
        ))));
    assert_eq!(subscription.next().await, Err(SendError::Unavailable));
    assert_eq!(
        fixture
            .broker
            .client()
            .history()
            .read(fixture.route.clone())
            .await
            .unwrap()
            .messages[0]
            .delivery,
        Some(Delivery::Uncertain)
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn overflow_or_oversized_metadata_fails_without_promoting_queued_evidence() {
    let fixture = fixture().await;
    let broker = Arc::new(fixture.broker.clone());
    let mut subscription = WhatsappReceiptSubscription::attach(&broker, fixture.source.clone())
        .await
        .unwrap();
    for _ in 0..=CAPACITY {
        subscription
            .handler
            .handle_event(Arc::new(Event::Receipt(receipt(
                &fixture.route,
                ReceiptType::Read,
            ))));
    }
    assert_eq!(subscription.next().await, Err(SendError::Unavailable));
    let mut fresh = WhatsappReceiptSubscription::attach(&broker, fixture.source.clone())
        .await
        .unwrap();
    let mut event = receipt(&fixture.route, ReceiptType::Read);
    event.message_ids = vec!["operation".into(); CAPACITY + 1];
    fresh.handler.handle_event(Arc::new(Event::Receipt(event)));
    assert_eq!(fresh.next().await, Err(SendError::Unavailable));
    assert_eq!(
        broker
            .client()
            .history()
            .read(fixture.route.clone())
            .await
            .unwrap()
            .messages[0]
            .delivery,
        Some(Delivery::Uncertain)
    );
    let handler = fresh.handler.clone();
    drop(fresh);
    assert!(handler.sender.is_closed());
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn wrong_recipient_and_persistence_failure_stop_subscription_without_false_delivery() {
    let fixture = fixture().await;
    let broker = Arc::new(fixture.broker.clone());
    let mut subscription = WhatsappReceiptSubscription::attach(&broker, fixture.source.clone())
        .await
        .unwrap();
    let mut event = receipt(&fixture.route, ReceiptType::Read);
    event.source.sender = "different@lid".parse().unwrap();
    subscription
        .handler
        .handle_event(Arc::new(Event::Receipt(event)));
    assert_eq!(subscription.next().await, Err(SendError::Revoked));
    assert_eq!(
        broker
            .client()
            .history()
            .read(fixture.route.clone())
            .await
            .unwrap()
            .messages[0]
            .delivery,
        Some(Delivery::Uncertain)
    );
    let path = fixture.root.path().join("history/history.enc");
    let ciphertext = std::fs::read(&path).unwrap();
    std::fs::rename(&path, path.with_extension("saved")).unwrap();
    std::fs::create_dir(&path).unwrap();
    let mut fresh = WhatsappReceiptSubscription::attach(&broker, fixture.source.clone())
        .await
        .unwrap();
    fresh.handler.handle_event(Arc::new(Event::Receipt(receipt(
        &fixture.route,
        ReceiptType::Read,
    ))));
    assert!(fresh.next().await.is_err());
    assert_eq!(fresh.next().await, Err(SendError::Unavailable));
    assert_eq!(
        broker.client().history().read(fixture.route.clone()).await,
        Err(tilo_messaging::HistoryError::Unavailable)
    );
    assert_eq!(
        std::fs::read(path.with_extension("saved")).unwrap(),
        ciphertext
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
}
