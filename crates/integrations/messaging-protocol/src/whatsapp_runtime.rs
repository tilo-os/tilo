//! Pinned SDK send binding for a supervisor-owned, already linked client.

use std::sync::Arc;

use tilo_clock::time::Duration;
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
    pub accounts: &'a accounts::person::AccountList,
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
    /// Bind an already linked, host-owned SDK instance to its local journal.
    /// The host must hold exclusive credential authority throughout a send.
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
        accounts: &accounts::person::AccountList,
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
    // bytes. The SDK's crypto/ratchet state remains under host ownership.
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
