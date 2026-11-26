//! `WhatsApp` receipt contract for an account-bound SDK adapter.
//! Independently specified from whatsapp-rust's `ReceiptType`.
//! This module neither implements cryptography nor links a `WhatsApp` device.

use crate::{Error, Route, Service};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evidence {
    SentOnly,
    RecipientDelivered,
    RecipientRead,
    ReconciliationRequired,
    NoRecipientEvidence,
}

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
