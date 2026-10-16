//! `WhatsApp` acknowledgment/receipt contract for an account-bound SDK adapter.
//! Independently specified from whatsapp-rust's `ServerAck` and `ReceiptType`.
//! This module neither implements cryptography nor links a `WhatsApp` device.

use crate::{Error, Route, Service};

/// Protocol observations stay separate from document/UI delivery state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evidence {
    ServerAcknowledged,
    SentOnly,
    RecipientDelivered,
    RecipientRead,
    ReconciliationRequired,
    NoRecipientEvidence,
}

/// Project only public routing/correlation data from an SDK `ServerAck` event.
pub struct ServerAck<'a> {
    pub account: &'a str,
    pub stanza_id: &'a str,
    pub class: Option<&'a str>,
    pub from: Option<&'a str>,
    pub rejected: bool,
}

/// Admit an exact message acknowledgment; call/receipt acks cannot complete it.
/// A caller must bind the event to the actual account-owned SDK instance.
///
/// # Errors
/// Refuses another account/service, mismatched IDs/chat/class and server nacks.
pub fn acknowledgment(
    route: &Route,
    message_id: &str,
    event: &ServerAck<'_>,
) -> Result<Evidence, Error> {
    check_route(route, event.account)?;
    if event.stanza_id != message_id || event.class != Some("message") {
        return Err(Error::WrongRequest);
    }
    if event.from.is_some_and(|from| from != route.recipient()) {
        return Err(Error::InvalidIdentity);
    }
    if event.rejected {
        return Err(Error::RemoteRejected);
    }
    Ok(Evidence::ServerAcknowledged)
}

/// The SDK's processed receipt semantics, not Rust enum serialization labels.
pub struct Receipt<'a> {
    pub account: &'a str,
    pub chat_jid: &'a str,
    pub wire_type: &'a str,
    pub message_ids: &'a [&'a str],
    /// A feature-incapable LID error downgrades apparent delivery to sent-only.
    pub feature_incapable: bool,
}

/// Correlate one direct conversation receipt with the exact original route/id.
/// LID and phone JIDs are distinct until a separately verified protocol mapping
/// is approved; display names and stripped phone digits never resolve them.
///
/// # Errors
/// Refuses another account/chat, unrelated IDs, oversized lists or unknown kinds.
pub fn receipt(route: &Route, message_id: &str, event: &Receipt<'_>) -> Result<Evidence, Error> {
    check_route(route, event.account)?;
    if event.chat_jid != route.recipient() {
        return Err(Error::InvalidIdentity);
    }
    if event.message_ids.len() > 4096 || !event.message_ids.contains(&message_id) {
        return Err(Error::WrongRequest);
    }
    match event.wire_type {
        "" | "delivery" if event.feature_incapable => Ok(Evidence::SentOnly),
        "" | "delivery" => Ok(Evidence::RecipientDelivered),
        "sent" => Ok(Evidence::SentOnly),
        "read" | "played" => Ok(Evidence::RecipientRead),
        "retry" | "server-error" => Ok(Evidence::ReconciliationRequired),
        "sender" | "read-self" | "played-self" | "inactive" | "peer_msg" | "hist_sync" => {
            Ok(Evidence::NoRecipientEvidence)
        }
        _ => Err(Error::Unsupported),
    }
}

pub(crate) fn check_route(route: &Route, account: &str) -> Result<(), Error> {
    if route.service() != Service::Whatsapp {
        return Err(Error::Unsupported);
    }
    if route.account() != account {
        return Err(Error::WrongAccount);
    }
    let (user, server) = route
        .recipient()
        .split_once('@')
        .ok_or(Error::InvalidIdentity)?;
    if !["s.whatsapp.net", "lid"].contains(&server)
        || user.is_empty()
        || !user.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(Error::Unsupported);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn route() -> Route {
        Route::new(Service::Whatsapp, "account-a".into(), "123@lid".into()).unwrap()
    }

    #[test]
    fn call_and_other_account_acks_never_complete_a_message() {
        let mut ack = ServerAck {
            account: "account-a",
            stanza_id: "message-id",
            class: Some("message"),
            from: Some("123@lid"),
            rejected: false,
        };
        assert_eq!(
            acknowledgment(&route(), "message-id", &ack),
            Ok(Evidence::ServerAcknowledged)
        );
        ack.class = Some("call");
        assert_eq!(
            acknowledgment(&route(), "message-id", &ack),
            Err(Error::WrongRequest)
        );
        ack.class = Some("message");
        ack.account = "account-b";
        assert_eq!(
            acknowledgment(&route(), "message-id", &ack),
            Err(Error::WrongAccount)
        );
        ack.account = "account-a";
        ack.rejected = true;
        assert_eq!(
            acknowledgment(&route(), "message-id", &ack),
            Err(Error::RemoteRejected)
        );
    }

    #[test]
    fn self_reads_and_feature_incapable_receipts_do_not_invent_recipient_delivery() {
        let mut event = Receipt {
            account: "account-a",
            chat_jid: "123@lid",
            wire_type: "read-self",
            message_ids: &["message-id"],
            feature_incapable: false,
        };
        assert_eq!(
            receipt(&route(), "message-id", &event),
            Ok(Evidence::NoRecipientEvidence)
        );
        event.wire_type = "delivery";
        event.feature_incapable = true;
        assert_eq!(
            receipt(&route(), "message-id", &event),
            Ok(Evidence::SentOnly)
        );
        event.feature_incapable = false;
        assert_eq!(
            receipt(&route(), "message-id", &event),
            Ok(Evidence::RecipientDelivered)
        );
        event.wire_type = "read";
        assert_eq!(
            receipt(&route(), "message-id", &event),
            Ok(Evidence::RecipientRead)
        );
        event.chat_jid = "123@s.whatsapp.net";
        assert_eq!(
            receipt(&route(), "message-id", &event),
            Err(Error::InvalidIdentity)
        );
        event.chat_jid = "123@lid";
        assert_eq!(
            receipt(&route(), "another-message", &event),
            Err(Error::WrongRequest)
        );
    }
}
