//! Pinned SDK send binding for a supervisor-owned, already linked client.
//!
//! The supervisor must retain credential custody, configure encrypted SDK
//! storage and bind one SDK client to one verified account/link generation.
//! No default storage, transport, login or linking is created here. SDK write
//! completion is submitted evidence only; server and recipient receipts remain
//! separate. This initial binding supports direct text, not full rich content.

use std::sync::Arc;

use tilo_clock::Duration;
use whatsapp_rust::Client;
use whatsapp_rust::send::SendOptions;
use whatsapp_rust::wacore::proto_helpers::MessageBuilderExt;
use whatsapp_rust::waproto::whatsapp as wa;
use zeroize::Zeroizing;

use crate::journal::{EncryptedJournal, Owner, RecipientEvidence, Status};
use crate::routing::LiveBinding;
use crate::signal_session::{CheckedSendError, Completion, SendError, SendJournal};
use crate::{Error, ProtocolReference, Route, Service};

mod incoming;
pub(crate) use incoming::text_body as group_text_body;
pub use incoming::{IncomingContext, IncomingText};

/// Host-held subscription context; authority must not come from app action input.
/// The host must revalidate the contact association and retain its credential
/// grant while admitting events, just as it does before sending.
pub struct ReceiptContext<'a> {
    pub accounts: &'a accounts::AccountStore,
    pub binding: &'a LiveBinding,
    pub source: &'a Arc<Client>,
    pub route: &'a Route,
    pub operation: &'a str,
}

pub struct WhatsappRuntime {
    client: Arc<Client>,
    journal: EncryptedJournal,
    owner: Owner,
    usable: bool,
}

impl WhatsappRuntime {
    /// Bind an already linked, host-custodied SDK instance to its local journal.
    /// The host must hold exclusive credential authority throughout a send.
    ///
    /// # Errors
    /// Refuses service or journal ownership mismatches.
    pub fn new(
        owner: Owner,
        client: Arc<Client>,
        journal: EncryptedJournal,
    ) -> Result<Self, Error> {
        if owner.service != Service::Whatsapp || journal.owner() != &owner {
            return Err(Error::WrongAccount);
        }
        Ok(Self {
            client,
            journal,
            owner,
            usable: true,
        })
    }

    pub(crate) fn owns_source(&self, source: &Arc<Client>) -> bool {
        Arc::ptr_eq(source, &self.client)
    }

    /// Persist a frozen direct-text operation before calling the real SDK with
    /// its exact stanza ID. A deadline/error after reservation is uncertain;
    /// neither this object nor a new object can initially submit that ID again.
    /// Call on a dedicated broker worker, never the app/render thread.
    ///
    /// # Errors
    /// Refuses stale routes/unlinked clients/invalid requests before submission.
    /// Post-reservation errors never imply that no external write occurred.
    pub async fn send(
        &mut self,
        accounts: &accounts::AccountStore,
        binding: &LiveBinding,
        route: &Route,
        operation: &str,
        text: &str,
        timeout: Duration,
    ) -> Result<ProtocolReference, CheckedSendError> {
        crate::routing::check(accounts, route, &self.owner, binding)
            .map_err(CheckedSendError::Routing)?;
        if !self.usable || !self.client.is_connected() || !self.client.is_logged_in() {
            return Err(before(Error::ConnectionUnavailable));
        }
        if timeout.is_zero() || timeout > Duration::from_secs(60) {
            return Err(before(Error::InvalidRequest));
        }
        crate::whatsapp::check_route(route, &self.owner.account).map_err(before)?;
        let prepared = prepare(route, operation, text).map_err(before)?;
        self.journal
            .begin(operation, route, &prepared.frozen)
            .map_err(|error| CheckedSendError::Transport(SendError::Journal(error)))?;
        let result = tokio::time::timeout(
            timeout,
            self.client.send_message_with_options(
                prepared.recipient,
                prepared.message,
                prepared.options,
            ),
        )
        .await;
        let reference = ProtocolReference::Whatsapp {
            message_id: operation.into(),
        };
        let succeeded = matches!(&result, Ok(Ok(sent)) if sent.message_id == operation && sent.to.to_string() == route.recipient());
        let completion = if succeeded {
            Completion::Submitted(reference.clone())
        } else {
            Completion::Uncertain
        };
        if self.journal.complete(operation, completion).is_err() || !succeeded {
            self.usable = false;
            return Err(CheckedSendError::Transport(SendError::Uncertain));
        }
        Ok(reference)
    }

    /// Project an event delivered by this account-owned SDK instance. The host
    /// retains the event subscription and must never mix events across clients.
    ///
    /// # Errors
    /// Refuses unrelated message/account/chat/class or a server rejection.
    pub fn server_ack(
        &self,
        route: &Route,
        operation: &str,
        event: &whatsapp_rust::wacore::types::events::ServerAck,
    ) -> Result<crate::whatsapp::Evidence, Error> {
        let from = event.from.as_ref().map(ToString::to_string);
        crate::whatsapp::acknowledgment(
            route,
            operation,
            &crate::whatsapp::ServerAck {
                account: &self.owner.account,
                stanza_id: &event.id,
                class: event.class.as_deref(),
                from: from.as_deref(),
                rejected: event.error.is_some(),
            },
        )
    }

    /// Admit an actual SDK recipient receipt from this runtime's subscription.
    /// Exact direct JIDs are required; alternate phone/LID identities are never
    /// guessed. Self/group receipts cannot supply recipient delivery evidence.
    ///
    /// # Errors
    /// Refuses stale account/link authority, another SDK client, unknown or
    /// differently routed operations, and persistence failures. No send occurs.
    pub fn observe_receipt(
        &mut self,
        context: &ReceiptContext<'_>,
        event: &whatsapp_rust::wacore::types::events::Receipt,
    ) -> Result<crate::whatsapp::Evidence, Error> {
        if !Arc::ptr_eq(context.source, &self.client) {
            return Err(Error::WrongAccount);
        }
        crate::routing::check(
            context.accounts,
            context.route,
            &self.owner,
            context.binding,
        )
        .map_err(|_| Error::WrongAccount)?;
        self.journal
            .check_observation(context.operation, context.route)
            .map_err(|_| Error::WrongRequest)?;
        if event.source.is_group || event.source.is_from_me {
            return Ok(crate::whatsapp::Evidence::NoRecipientEvidence);
        }
        if event.source.sender.to_string() != context.route.recipient() {
            return Err(Error::InvalidIdentity);
        }
        if event.message_ids.len() > 4096 {
            return Err(Error::WrongRequest);
        }
        if matches!(
            event.r#type,
            whatsapp_rust::wacore::types::presence::ReceiptType::Other(_)
        ) {
            return Err(Error::Unsupported);
        }
        let chat = event.source.chat.to_string();
        let ids: Vec<&str> = event.message_ids.iter().map(String::as_str).collect();
        let evidence = crate::whatsapp::receipt(
            context.route,
            context.operation,
            &crate::whatsapp::Receipt {
                account: &self.owner.account,
                chat_jid: &chat,
                wire_type: event.r#type.as_wire_str(),
                message_ids: &ids,
                feature_incapable: false, // The SDK has already downgraded this to Sent.
            },
        )?;
        let recipient = match evidence {
            crate::whatsapp::Evidence::RecipientDelivered => Some(RecipientEvidence::Delivered),
            crate::whatsapp::Evidence::RecipientRead => Some(RecipientEvidence::Read),
            _ => None,
        };
        if let Some(recipient) = recipient {
            self.journal
                .observe_recipient(context.operation, context.route, recipient)
                .map_err(|_| Error::ConnectionUnavailable)?;
        }
        Ok(evidence)
    }

    pub fn recipient_evidence(&self, operation: &str) -> Option<RecipientEvidence> {
        self.journal.recipient_evidence(operation)
    }

    /// Frozen native owner used by the trusted mounting host.
    pub fn owner(&self) -> &Owner {
        &self.owner
    }

    pub fn status(&self, operation: &str) -> Option<Status> {
        self.journal.status(operation)
    }
}

fn before(error: Error) -> CheckedSendError {
    CheckedSendError::Transport(SendError::BeforeSubmission(error))
}

struct Prepared {
    recipient: whatsapp_rust::Jid,
    message: wa::Message,
    options: SendOptions,
    frozen: Zeroizing<Vec<u8>>,
}

fn prepare(route: &Route, operation: &str, text: &str) -> Result<Prepared, Error> {
    crate::whatsapp::check_route(route, route.account())?;
    if !crate::identifier(operation)
        || text.trim().is_empty()
        || text.len() > crate::signal::MAX_TEXT_BYTES
    {
        return Err(Error::InvalidRequest);
    }
    let recipient: whatsapp_rust::Jid = route
        .recipient()
        .parse()
        .map_err(|_| Error::InvalidIdentity)?;
    if recipient.to_string() != route.recipient() {
        return Err(Error::InvalidIdentity);
    }
    // This freezes the exact SDK inputs, not its subsequently randomized E2EE
    // bytes. The SDK's crypto/ratchet state remains under host custody.
    let frozen = Zeroizing::new(
        serde_json::to_vec(&serde_json::json!({
            "route":route,"stanza_id":operation,"text":text
        }))
        .map_err(|_| Error::InvalidRequest)?,
    );
    Ok(Prepared {
        recipient,
        message: wa::Message::text(text),
        options: SendOptions::default().with_message_id(operation),
        frozen,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct NoNetwork(Arc<AtomicUsize>);

    #[whatsapp_rust::async_trait]
    impl whatsapp_rust::http::HttpClient for NoNetwork {
        async fn execute(
            &self,
            _: whatsapp_rust::http::HttpRequest,
        ) -> whatsapp_rust::anyhow::Result<whatsapp_rust::http::HttpResponse> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(whatsapp_rust::anyhow::anyhow!("fixture forbids network"))
        }
    }

    #[whatsapp_rust::async_trait]
    impl whatsapp_rust::transport::TransportFactory for NoNetwork {
        async fn create_transport(
            &self,
        ) -> whatsapp_rust::anyhow::Result<(
            Arc<dyn whatsapp_rust::transport::Transport>,
            whatsapp_rust::async_channel::Receiver<whatsapp_rust::transport::TransportEvent>,
        )> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(whatsapp_rust::anyhow::anyhow!("fixture forbids network"))
        }
    }

    struct Fixture {
        calls: Arc<AtomicUsize>,
        owner: Owner,
        root: tempfile::TempDir,
        binding: LiveBinding,
        runtime: WhatsappRuntime,
        accounts: accounts::AccountStore,
        route: Route,
        client: Arc<Client>,
    }

    async fn fixture() -> Fixture {
        let calls = Arc::new(AtomicUsize::new(0));
        let backend = Arc::new(whatsapp_rust::wacore::store::in_memory::InMemoryBackend::new());
        let manager = Arc::new(
            whatsapp_rust::store::persistence_manager::PersistenceManager::new(backend)
                .await
                .unwrap(),
        );
        let (client, _worker) = Client::builder()
            .with_runtime(whatsapp_rust::TokioRuntime)
            .with_persistence_manager(manager)
            .with_http_client(NoNetwork(calls.clone()))
            .with_transport_factory(NoNetwork(calls.clone()))
            .build()
            .await
            .unwrap()
            .into_parts();
        let owner = Owner {
            principal: "11".repeat(32),
            device: "device-a".into(),
            service: Service::Whatsapp,
            account: "tilo-account".into(),
            linking_generation: 1,
        };
        let root = tempfile::tempdir().unwrap();
        let journal = EncryptedJournal::open(
            root.path(),
            &owner,
            secret_vault::VaultKey::from_bytes([7; 32]),
        )
        .unwrap();
        let binding = LiveBinding {
            owner: owner.clone(),
            state: crate::routing::LinkState::Linked,
        };
        let runtime = WhatsappRuntime::new(owner.clone(), client.clone(), journal).unwrap();
        let mut accounts = accounts::AccountStore::new();
        accounts.upsert(accounts::Account::new("tilo-account", "whatsapp"));
        let route = Route::new(Service::Whatsapp, "tilo-account".into(), "123@lid".into()).unwrap();
        Fixture {
            calls,
            owner,
            root,
            binding,
            runtime,
            accounts,
            route,
            client,
        }
    }

    #[tokio::test]
    async fn actual_unlinked_sdk_refuses_send_without_reserving_or_network_access() {
        let Fixture {
            calls,
            root: _root,
            binding,
            mut runtime,
            accounts,
            route,
            ..
        } = fixture().await;
        assert_eq!(
            runtime
                .send(
                    &accounts,
                    &binding,
                    &route,
                    "op",
                    "private text",
                    Duration::from_secs(1)
                )
                .await,
            Err(before(Error::ConnectionUnavailable))
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(runtime.status("op").is_none());
        let ack = whatsapp_rust::wacore::types::events::ServerAck::builder()
            .id("op".into())
            .class("message".into())
            .from("123@lid".parse().unwrap())
            .build();
        assert_eq!(
            runtime.server_ack(&route, "op", &ack),
            Ok(crate::whatsapp::Evidence::ServerAcknowledged)
        );
        assert_eq!(
            runtime.server_ack(&route, "another-op", &ack),
            Err(Error::WrongRequest)
        );
    }

    fn receipt_fixture() -> whatsapp_rust::wacore::types::events::Receipt {
        whatsapp_rust::wacore::types::events::Receipt::builder()
            .source(whatsapp_rust::wacore::types::message::MessageSource {
                chat: "123@lid".parse().unwrap(),
                sender: "123@lid".parse().unwrap(),
                ..Default::default()
            })
            .message_ids(vec!["op".into()])
            .timestamp("2026-10-06T00:00:00Z".parse().unwrap())
            .r#type(whatsapp_rust::wacore::types::presence::ReceiptType::Delivered)
            .offline(false)
            .build()
    }

    fn exercise_receipt_refusals(
        runtime: &mut WhatsappRuntime,
        context: &ReceiptContext<'_>,
        event: &mut whatsapp_rust::wacore::types::events::Receipt,
    ) {
        event.r#type = whatsapp_rust::wacore::types::presence::ReceiptType::Sent;
        assert_eq!(
            runtime.observe_receipt(context, event),
            Ok(crate::whatsapp::Evidence::SentOnly)
        );
        assert_eq!(runtime.recipient_evidence("op"), None);
        event.r#type =
            whatsapp_rust::wacore::types::presence::ReceiptType::Other("delivery".into());
        assert_eq!(
            runtime.observe_receipt(context, event),
            Err(Error::Unsupported)
        );
        assert_eq!(runtime.recipient_evidence("op"), None);
        event.r#type = whatsapp_rust::wacore::types::presence::ReceiptType::Delivered;
        event.source.is_from_me = true;
        assert_eq!(
            runtime.observe_receipt(context, event),
            Ok(crate::whatsapp::Evidence::NoRecipientEvidence)
        );
        assert_eq!(runtime.recipient_evidence("op"), None);
        event.source.is_from_me = false;
        event.source.sender = "456@lid".parse().unwrap();
        assert_eq!(
            runtime.observe_receipt(context, event),
            Err(Error::InvalidIdentity)
        );
        event.source.sender = "123@lid".parse().unwrap();
        event.message_ids = vec!["other-op".into()];
        assert_eq!(
            runtime.observe_receipt(context, event),
            Err(Error::WrongRequest)
        );
        event.message_ids = vec!["op".into()];
        let wrong_route =
            Route::new(Service::Whatsapp, "tilo-account".into(), "456@lid".into()).unwrap();
        let wrong_context = ReceiptContext {
            route: &wrong_route,
            ..*context
        };
        assert_eq!(
            runtime.observe_receipt(&wrong_context, event),
            Err(Error::WrongRequest)
        );
        let revoked = LiveBinding {
            owner: context.binding.owner.clone(),
            state: crate::routing::LinkState::Revoked,
        };
        let revoked_context = ReceiptContext {
            binding: &revoked,
            ..*context
        };
        assert_eq!(
            runtime.observe_receipt(&revoked_context, event),
            Err(Error::WrongAccount)
        );
    }

    #[tokio::test]
    async fn actual_sdk_receipts_reconcile_exact_operations_durably_without_resending() {
        let Fixture {
            calls,
            owner,
            root,
            binding,
            mut runtime,
            accounts,
            route,
            client,
        } = fixture().await;
        let mut event = receipt_fixture();
        let context = ReceiptContext {
            accounts: &accounts,
            binding: &binding,
            source: &client,
            route: &route,
            operation: "op",
        };
        assert_eq!(
            runtime.observe_receipt(&context, &event),
            Err(Error::WrongRequest)
        );
        runtime
            .journal
            .begin("op", &route, b"frozen fixture")
            .unwrap();
        runtime
            .journal
            .complete("op", Completion::Uncertain)
            .unwrap();
        let other = fixture().await;
        let other_context = ReceiptContext {
            source: &other.client,
            ..context
        };
        assert_eq!(
            runtime.observe_receipt(&other_context, &event),
            Err(Error::WrongAccount)
        );
        assert_eq!(other.calls.load(Ordering::SeqCst), 0);
        exercise_receipt_refusals(&mut runtime, &context, &mut event);
        assert_eq!(
            runtime.observe_receipt(&context, &event),
            Ok(crate::whatsapp::Evidence::RecipientDelivered)
        );
        assert_eq!(
            runtime.recipient_evidence("op"),
            Some(RecipientEvidence::Delivered)
        );
        event.r#type = whatsapp_rust::wacore::types::presence::ReceiptType::Read;
        assert_eq!(
            runtime.observe_receipt(&context, &event),
            Ok(crate::whatsapp::Evidence::RecipientRead)
        );
        event.r#type = whatsapp_rust::wacore::types::presence::ReceiptType::Delivered;
        runtime.observe_receipt(&context, &event).unwrap();
        assert_eq!(
            runtime.recipient_evidence("op"),
            Some(RecipientEvidence::Read)
        );
        // Late receipt reconciliation never repeats the original uncertain send.
        assert_eq!(runtime.status("op"), Some(Status::Uncertain));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        drop(runtime);
        let reopened = EncryptedJournal::open(
            root.path(),
            &owner,
            secret_vault::VaultKey::from_bytes([7; 32]),
        )
        .unwrap();
        assert_eq!(
            reopened.recipient_evidence("op"),
            Some(RecipientEvidence::Read)
        );
    }

    #[test]
    fn real_sdk_inputs_keep_lid_route_stanza_and_text_exact() {
        let route = Route::new(Service::Whatsapp, "tilo-account".into(), "123@lid".into()).unwrap();
        let prepared = prepare(&route, "stanza-id", "private text").unwrap();
        assert_eq!(prepared.recipient.to_string(), "123@lid");
        assert_eq!(prepared.options.message_id.as_deref(), Some("stanza-id"));
        assert_eq!(
            prepared.message.conversation.as_deref(),
            Some("private text")
        );
        let frozen: serde_json::Value = serde_json::from_slice(&prepared.frozen).unwrap();
        assert_eq!(frozen["route"]["account"], "tilo-account");
        assert_eq!(frozen["stanza_id"], "stanza-id");
        assert_eq!(frozen["text"], "private text");
        for recipient in [
            "123@g.us",
            "123:2@s.whatsapp.net",
            "name@lid",
            "status@broadcast",
        ] {
            let unsupported =
                Route::new(Service::Whatsapp, "tilo-account".into(), recipient.into()).unwrap();
            assert!(prepare(&unsupported, "stanza-id", "private text").is_err());
        }
    }
}
