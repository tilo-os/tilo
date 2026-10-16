use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use ui::prelude::Id;

pub(super) fn owner(service: crate::Service) -> Owner {
    Owner {
        principal: "11".repeat(32),
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
pub(super) struct Gate {
    pub(super) owner: Owner,
    pub(super) revoked: Arc<AtomicBool>,
}
impl ExternalAuthority for Gate {
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
        let mut accounts = accounts::AccountStore::new();
        accounts.upsert(accounts::Account::new(
            "account",
            service_name(self.owner.service),
        ));
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
struct RetainingGate {
    gate: Gate,
    live: Arc<AtomicUsize>,
}
impl ExternalAuthority for RetainingGate {
    fn authorize(&self, route: &Route) -> Result<Authorization, SendError> {
        let auth = self.gate.authorize(route)?;
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
            Arc::new(RetainingGate {
                gate: Gate {
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
        let page = client.history().read(route(service)).await.unwrap();
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
            Arc::new(Gate {
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
        let page = client.history().read(route(service)).await.unwrap();
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
        revoked.store(true, Ordering::SeqCst);
        assert!(!client.supports(&route(service)));
        assert_eq!(
            client.history().read(route(service)).await,
            Err(HistoryError::Revoked)
        );
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
        assert!(
            !ciphertext
                .windows(b"private fixture message".len())
                .any(|w| w == b"private fixture message")
        );
    }
}

struct AccountOwnerFixture;
impl ui::framework::ResourceApp for AccountOwnerFixture {
    type Resource = tilo_messaging::PersonAccounts;
    const ID: &'static str = "so.tilo.settings";
    const TITLE: &'static str = "Account metadata owner fixture";
    fn view(cx: &mut ui::framework::ViewCx<Self::Resource>) -> ui::widget::Element {
        let _ = cx.shared_person::<tilo_messaging::PersonAccounts>();
        Box::new(ui::widget::Column::new())
    }
}

#[test]
fn messages_actions_use_hosted_whatsapp_and_sms_and_keep_text_outside_shared_state() {
    use messages::{MessagesEffect, MessagesShell};
    use ui::framework::testing::TestApp;
    for service in [crate::Service::Whatsapp, crate::Service::Sms] {
        let root = tempfile::tempdir().unwrap();
        let owner = owner(service);
        let route = route(service);
        let calls = Arc::new(AtomicUsize::new(0));
        let revoked = Arc::new(AtomicBool::new(false));
        let history = History::open(
            root.path(),
            &owner,
            secret_vault::VaultKey::from_bytes([7; 32]),
        )
        .unwrap();
        let broker = ExternalBroker::new(
            Box::new(Adapter {
                owner: owner.clone(),
                calls: calls.clone(),
            }),
            history,
            route.scope.clone(),
            Arc::new(Gate {
                owner: owner.clone(),
                revoked: revoked.clone(),
            }),
        )
        .unwrap();
        let mut app =
            TestApp::<MessagesShell>::for_person_with_lent(&owner.principal, broker.client())
                .unwrap();
        project_account_metadata(&owner, &route, &mut app);
        let directory = app
            .shared_person::<tilo_contact_directory::ContactDirectory>()
            .unwrap();
        directory
            .invoke(
                &tilo_contact_directory::ContactDirectory::PROJECT_CONTACT
                    .with((route.person.clone(), "Alex".into())),
            )
            .unwrap();
        directory
            .invoke(
                &tilo_contact_directory::ContactDirectory::CONFIRM_ACCOUNT.with((
                    route.person.clone(),
                    route.association.clone(),
                    route.service.clone(),
                    route.scope.clone(),
                    route.recipient.clone(),
                    route.revision.clone(),
                )),
            )
            .unwrap();
        app.settle();
        app.effect(&MessagesEffect::OpenPerson {
            person: route.person.as_str().into(),
        })
        .unwrap();
        let choice = serde_json::json!({"service":route.service,"account":owner.account,"recipient":route.recipient,"association":route.association.as_str(),"scope":route.scope,"revision":route.revision}).to_string();
        app.effect(&MessagesEffect::SelectRoute {
            choice: choice.clone(),
        })
        .unwrap();
        let send = MessagesEffect::Send {
            choice: choice.clone(),
            operation: "operation".into(),
            text: "private fixture message".into(),
        };
        assert!(app.effect(&send).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        app.effect_confirmed(&send).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            app.harness()
                .a11y_nodes()
                .iter()
                .any(|(_, node)| node.label() == Some("private fixture message"))
        );
        assert!(
            !app.harness()
                .dump_json()
                .contains("private fixture message")
        );
        assert!(app.effect_confirmed(&send).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        revoked.store(true, Ordering::SeqCst);
        assert!(app.effect(&MessagesEffect::ReadHistory { choice }).is_err());
    }
}

pub(crate) fn project_account_metadata(
    owner: &Owner,
    route: &Route,
    app: &mut ui::framework::testing::TestApp<messages::MessagesShell>,
) {
    let mut settings =
        ui::framework::testing::TestApp::<AccountOwnerFixture>::for_person(&owner.principal)
            .unwrap();
    let metadata = settings
        .shared_person::<tilo_messaging::PersonAccounts>()
        .unwrap();
    assert!(metadata.editable());
    metadata
        .invoke(&tilo_messaging::PersonAccounts::SAVE_ACCOUNT.with((
            owner.account.clone(),
            route.service.clone(),
            "Personal".into(),
            serde_json::json!({"messaging_scope":route.scope}),
            true,
            0,
        )))
        .unwrap();
    let accounts = app
        .shared_person::<tilo_messaging::PersonAccounts>()
        .unwrap();
    assert!(!accounts.editable());
    accounts
        .record()
        .merge_peer_document(metadata.record().document().unwrap())
        .unwrap();
}
