use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use ui::prelude::Id;

pub(super) fn owner(service: crate::Service) -> Owner {
    Owner {
        actor: "11".repeat(32),
        device: "device".into(),
        service,
        account: "account".into(),
        linking_generation: 1,
    }
}
pub(super) fn route(service: crate::Service) -> Route {
    Route {
        person: Id::from_raw("00000000-0000-4000-8000-000000000001"),
        association: Id::from_raw("association"),
        sender: Id::from_raw("account"),
        service: service_name(service).into(),
        scope: "verified-provider".into(),
        recipient: if service == crate::Service::Sms {
            "+12025550123".into()
        } else {
            "12025550123@s.whatsapp.net".into()
        },
        revision: "reviewed-identity".into(),
    }
}
pub(super) struct Hold {
    pub(super) owner: Owner,
    pub(super) revoked: Arc<AtomicBool>,
}
impl ExternalAuthority for Hold {
    fn incoming(&self, recipient: &str) -> Result<Authorization, SendError> {
        let current = route(self.owner.service);
        if current.recipient != recipient {
            return Err(SendError::Revoked);
        }
        self.authorize(&current)
    }
    fn authorize(&self, requested: &Route) -> Result<Authorization, SendError> {
        let current = route(self.owner.service);
        if self.revoked.load(Ordering::SeqCst) || current != *requested {
            return Err(SendError::Revoked);
        }
        let mut accounts = accounts::person::AccountList::default();
        accounts
            .keep(&accounts::Account::new(
                "account",
                service_name(self.owner.service),
            ))
            .map_err(|_| SendError::Revoked)?;
        let mut directory = tilo_contact_directory::ContactDirectory::default();
        directory
            .project_contact(current.person.clone(), "Fixture".into())
            .map_err(|_| SendError::Revoked)?;
        directory
            .confirm_account(
                current.person.clone(),
                current.association.clone(),
                current.service.clone(),
                current.scope.clone(),
                current.recipient.clone(),
                current.revision.clone(),
            )
            .map_err(|_| SendError::Revoked)?;
        Ok(Authorization::new(
            directory,
            accounts,
            crate::routing::LiveBinding {
                owner: self.owner.clone(),
                state: crate::routing::LinkState::Linked,
            },
            current,
        ))
    }
}
struct Adapter {
    owner: Owner,
    calls: Arc<AtomicUsize>,
}
impl NativeAdapter for Adapter {
    fn owner(&self) -> &Owner {
        &self.owner
    }
    fn send<'a>(
        &'a mut self,
        _: &'a Authorization,
        native: &'a crate::Route,
        request: &'a SendRequest,
    ) -> NativeSend<'a> {
        assert_eq!(native.account(), self.owner.account);
        assert_eq!(native.recipient(), request.route.recipient);
        self.calls.fetch_add(1, Ordering::SeqCst);
        let reference = match self.owner.service {
            crate::Service::Whatsapp => crate::ProtocolReference::Whatsapp {
                message_id: request.operation.clone(),
            },
            crate::Service::Sms => crate::ProtocolReference::Sms {
                modem_object: "/modem/1".into(),
                message_object: "/sms/2".into(),
                daemon_epoch: 1,
            },
            _ => {
                return Box::pin(std::future::ready(Err(
                    crate::signal_session::CheckedSendError::Transport(
                        crate::signal_session::SendError::Uncertain,
                    ),
                )));
            }
        };
        Box::pin(std::future::ready(Ok(reference)))
    }
}
fn request(service: crate::Service) -> SendRequest {
    SendRequest {
        route: route(service),
        operation: "operation".into(),
        text: zeroize::Zeroizing::new("private fixture message".into()),
    }
}

struct RetainedLease {
    live: Arc<AtomicUsize>,
}
impl Drop for RetainedLease {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::SeqCst);
    }
}
struct RetainingHold {
    hold: Hold,
    live: Arc<AtomicUsize>,
}
impl ExternalAuthority for RetainingHold {
    fn authorize(&self, route: &Route) -> Result<Authorization, SendError> {
        let auth = self.hold.authorize(route)?;
        self.live.fetch_add(1, Ordering::SeqCst);
        Ok(auth.retaining(Arc::new(RetainedLease {
            live: self.live.clone(),
        })))
    }
}
struct RevokingAdapter {
    adapter: Adapter,
    revoked: Arc<AtomicBool>,
    live: Arc<AtomicUsize>,
}
impl NativeAdapter for RevokingAdapter {
    fn owner(&self) -> &Owner {
        self.adapter.owner()
    }
    fn send<'a>(
        &'a mut self,
        auth: &'a Authorization,
        route: &'a crate::Route,
        request: &'a SendRequest,
    ) -> NativeSend<'a> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            assert!(self.live.load(Ordering::SeqCst) > 0);
            let reference = self.adapter.send(auth, route, request).await?;
            self.revoked.store(true, Ordering::SeqCst);
            Ok(reference)
        })
    }
}

#[tokio::test]
async fn authority_revoked_during_native_submission_keeps_uncertain_reservation_and_lease() {
    for service in [crate::Service::Whatsapp, crate::Service::Sms] {
        let root = tempfile::tempdir().unwrap();
        let owner = owner(service);
        let revoked = Arc::new(AtomicBool::new(false));
        let live = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let broker = ExternalBroker::new(
            Box::new(RevokingAdapter {
                adapter: Adapter {
                    owner: owner.clone(),
                    calls: calls.clone(),
                },
                revoked: revoked.clone(),
                live: live.clone(),
            }),
            History::open(
                root.path(),
                &owner,
                secret_vault::VaultKey::from_bytes([7; 32]),
            )
            .unwrap(),
            "verified-provider".into(),
            Arc::new(RetainingHold {
                hold: Hold {
                    owner: owner.clone(),
                    revoked: revoked.clone(),
                },
                live: live.clone(),
            }),
        )
        .unwrap();
        let client = broker.client();
        let wakes = Arc::new(AtomicUsize::new(0));
        let count = wakes.clone();
        let _observer = client.history().observe(Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        }));
        assert!(matches!(
            client.send(request(service)).await,
            Err(SendError::Uncertain)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(live.load(Ordering::SeqCst), 0);
        assert_eq!(wakes.load(Ordering::SeqCst), 0);
        assert!(!client.supports(&route(service)));
        revoked.store(false, Ordering::SeqCst);
        let page = client.history().read(route(service), None).await.unwrap();
        assert_eq!(page.messages.len(), 1);
        assert_eq!(
            page.messages.first().unwrap().delivery,
            Some(Delivery::Uncertain)
        );
        assert!(matches!(
            client.send(request(service)).await,
            Err(SendError::AlreadyReserved)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(live.load(Ordering::SeqCst), 0);
        drop(client);
        drop(broker);
        let history = History::open(
            root.path(),
            &owner,
            secret_vault::VaultKey::from_bytes([7; 32]),
        )
        .unwrap();
        assert_eq!(
            history.check_operation("operation"),
            Err(SendError::AlreadyReserved)
        );
        assert_eq!(
            history
                .page(route(service), 128)
                .unwrap()
                .messages
                .first()
                .unwrap()
                .delivery,
            Some(Delivery::Uncertain)
        );
    }
}

#[tokio::test]
async fn native_whatsapp_and_sms_submit_through_private_client_without_false_delivery_or_replay() {
    for service in [crate::Service::Whatsapp, crate::Service::Sms] {
        let root = tempfile::tempdir().unwrap();
        let owner = owner(service);
        let calls = Arc::new(AtomicUsize::new(0));
        let revoked = Arc::new(AtomicBool::new(false));
        let key = || secret_vault::VaultKey::from_bytes([7; 32]);
        let history = History::open(root.path(), &owner, key()).unwrap();
        let broker = ExternalBroker::new(
            Box::new(Adapter {
                owner: owner.clone(),
                calls: calls.clone(),
            }),
            history,
            "verified-provider".into(),
            Arc::new(Hold {
                owner: owner.clone(),
                revoked: revoked.clone(),
            }),
        )
        .unwrap();
        let client = broker.client();
        for mutation in ["account", "scope", "recipient", "revision", "service"] {
            let mut wrong = request(service);
            match mutation {
                "account" => wrong.route.sender = Id::from_raw("other-account"),
                "scope" => wrong.route.scope = "other-provider".into(),
                "recipient" => wrong.route.recipient = "+12025550124".into(),
                "revision" => wrong.route.revision = "old-identity".into(),
                _ => wrong.route.service = "signal".into(),
            }
            assert!(!client.supports(&wrong.route));
            assert!(client.send(wrong).await.is_err());
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
        assert_eq!(
            client.send(request(service)).await.unwrap().delivery,
            Delivery::Submitted
        );
        let page = client.history().read(route(service), None).await.unwrap();
        assert_eq!(page.messages.len(), 1);
        assert_eq!(
            page.messages.first().unwrap().delivery,
            Some(Delivery::Submitted)
        );
        assert_eq!(
            page.messages.first().unwrap().text.as_str(),
            "private fixture message"
        );
        assert!(matches!(
            client.send(request(service)).await,
            Err(SendError::AlreadyReserved)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let engine = messages_send_once_and_keep_text_on_the_device(&owner, &broker, &calls);
        revoked.store(true, Ordering::SeqCst);
        assert_eq!(
            engine.run("history", &ask(&route(service), None)),
            Err("revoked".to_owned()),
            "a revoked route reads no history"
        );
        assert!(!client.supports(&route(service)));
        assert_eq!(
            client.history().read(route(service), None).await,
            Err(HistoryError::Revoked)
        );
        drop(engine);
        drop(client);
        drop(broker);
        let history = History::open(root.path(), &owner, key()).unwrap();
        assert_eq!(
            history.check_operation("operation"),
            Err(SendError::AlreadyReserved)
        );
        assert_eq!(
            history
                .page(route(service), 128)
                .unwrap()
                .messages
                .first()
                .unwrap()
                .delivery,
            Some(Delivery::Submitted)
        );
        let ciphertext = std::fs::read(root.path().join("history.enc")).unwrap();
        for text in ["private fixture message", "private app message"] {
            assert!(!ciphertext.windows(text.len()).any(|w| w == text.as_bytes()));
        }
    }
}

/// Messages' engine sends through `broker` once, answers the sent text in the page it reads back
/// on this device, and refuses the same operation again without asking the service twice.
fn messages_send_once_and_keep_text_on_the_device(
    owner: &Owner,
    broker: &ExternalBroker,
    calls: &AtomicUsize,
) -> tilo_messaging::engine::Engine {
    let engine = tilo_messaging::engine::Engine::new(Some(broker.client()), None);
    let request = serde_json::json!({
        "route": route(owner.service),
        "operation": "app-operation",
        "text": "private app message",
    })
    .to_string();
    let before = calls.load(Ordering::SeqCst);
    let sent = engine.run("send", &request).unwrap();
    assert_eq!(sent["status"], "submitted");
    assert_eq!(calls.load(Ordering::SeqCst), before.saturating_add(1));
    let shown: Vec<&str> = sent["page"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["text"].as_str())
        .collect();
    assert!(shown.contains(&"private app message"), "{shown:?}");
    assert_eq!(engine.run("send", &request), Err("reserved".to_owned()));
    assert_eq!(calls.load(Ordering::SeqCst), before.saturating_add(1));
    engine
}

/// What Messages asks its engine about `route`, from the message `before` when it names one.
pub(super) fn ask(route: &Route, before: Option<&str>) -> String {
    serde_json::json!({"route": route, "before": before}).to_string()
}
