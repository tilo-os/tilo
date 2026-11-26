//! signal-cli multi-account JSON-RPC codec, independently implemented from
//! <https://github.com/AsamK/signal-cli/blob/master/man/signal-cli-jsonrpc.5.adoc>.
//! Requests always include account, including numberless ACI accounts.

use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::{Error, Route, Service};

pub const MAX_FRAME_BYTES: usize = 256 * 1024;
pub const MAX_TEXT_BYTES: usize = 32 * 1024;

/// Encode exactly one direct recipient; never fan out or use a default account.
/// The correlation ID is not a server idempotency guarantee.
pub fn send(route: &Route, operation: &str, text: &str) -> Result<Zeroizing<Vec<u8>>, Error> {
    send_on(route, route.account(), operation, text)
}

/// Encode with the host's verified signal-cli account. Tilo's account registry
/// ID is distinct from a Signal phone number/ACI and stays in the frozen route.
pub fn send_on(
    route: &Route,
    signal_account: &str,
    operation: &str,
    text: &str,
) -> Result<Zeroizing<Vec<u8>>, Error> {
    if route.service() != Service::Signal {
        return Err(Error::Unsupported);
    }
    if !crate::identifier(signal_account)
        || !crate::identifier(operation)
        || text.trim().is_empty()
        || text.len() > MAX_TEXT_BYTES
    {
        return Err(Error::InvalidRequest);
    }
    let payload = json!({"jsonrpc":"2.0", "id":operation, "method":"send",
        "params":{"account":signal_account, "recipient":[route.recipient()], "message":text}});
    let mut encoded =
        Zeroizing::new(serde_json::to_vec(&payload).map_err(|_| Error::InvalidRequest)?);
    encoded.push(b'\n');
    Ok(encoded)
}

/// A matching adapter response. Timestamp is a protocol correlation value,
/// never evidence of recipient delivery/read or permission to retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Acknowledgment {
    pub timestamp: u64,
}

pub fn acknowledgment(frame: &[u8], operation: &str) -> Result<Acknowledgment, Error> {
    let value = decode(frame)?;
    if value.get("id").and_then(Value::as_str) != Some(operation) {
        return Err(Error::WrongRequest);
    }
    if value.get("error").is_some() {
        return Err(Error::RemoteRejected);
    }
    let result = value.get("result").ok_or(Error::InvalidFrame)?;
    let timestamp = result
        .get("timestamp")
        .and_then(Value::as_u64)
        .filter(|time| *time > 0)
        .ok_or(Error::InvalidFrame)?;
    Ok(Acknowledgment { timestamp })
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct IncomingId {
    pub account: String,
    pub sender_aci: String,
    pub timestamp: u64,
}

/// Only supported direct plain text is admitted here. Rich/ephemeral/group
/// events need their own explicit semantics instead of a lossy text projection.
pub struct Incoming {
    pub id: IncomingId,
    pub text: Zeroizing<String>,
}

impl std::fmt::Debug for Incoming {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Incoming(<private>)")
    }
}

pub fn incoming(frame: &[u8], account: &str) -> Result<Incoming, Error> {
    let value = decode(frame)?;
    if value.get("method").and_then(Value::as_str) != Some("receive") {
        return Err(Error::Unsupported);
    }
    let params = value.get("params").ok_or(Error::InvalidFrame)?;
    let received = params.get("result").unwrap_or(params);
    if received.get("account").and_then(Value::as_str) != Some(account) {
        return Err(Error::WrongAccount);
    }
    let envelope = received.get("envelope").ok_or(Error::InvalidFrame)?;
    if [
        "editMessage",
        "storyMessage",
        "syncMessage",
        "callMessage",
        "receiptMessage",
        "typingMessage",
    ]
    .iter()
    .any(|field| envelope.get(field).is_some_and(|value| !value.is_null()))
    {
        return Err(Error::Unsupported);
    }
    let sender = envelope
        .get("sourceUuid")
        .and_then(Value::as_str)
        .filter(|sender| crate::identifier(sender))
        .ok_or(Error::InvalidIdentity)?;
    let sender = uuid::Uuid::parse_str(sender).map_err(|_| Error::InvalidIdentity)?;
    let message = envelope.get("dataMessage").ok_or(Error::Unsupported)?;
    let fields = message.as_object().ok_or(Error::InvalidFrame)?;
    if fields.keys().any(|field| {
        ![
            "timestamp",
            "message",
            "viewOnce",
            "isExpirationUpdate",
            "isEndSession",
            "isProfileKeyUpdate",
            "hasProfileKey",
            "expiresInSeconds",
            "attachments",
            "mentions",
            "contacts",
        ]
        .contains(&field.as_str())
    }) || [
        "viewOnce",
        "isExpirationUpdate",
        "isEndSession",
        "isProfileKeyUpdate",
    ]
    .iter()
    .any(|key| {
        message
            .get(key)
            .is_some_and(|value| value.as_bool() != Some(false))
    }) || message
        .get("hasProfileKey")
        .is_some_and(|value| value.as_bool().is_none())
        || message
            .get("expiresInSeconds")
            .is_some_and(|value| value.as_u64() != Some(0))
        || ["attachments", "mentions", "contacts"].iter().any(|key| {
            message
                .get(key)
                .is_some_and(|value| value.as_array().is_none_or(|items| !items.is_empty()))
        })
    {
        return Err(Error::Unsupported);
    }
    let timestamp = message
        .get("timestamp")
        .and_then(Value::as_u64)
        .filter(|time| *time > 0)
        .ok_or(Error::InvalidFrame)?;
    let text = message
        .get("message")
        .and_then(Value::as_str)
        .filter(|text| text.len() <= MAX_TEXT_BYTES)
        .ok_or(Error::InvalidFrame)?;
    Ok(Incoming {
        id: IncomingId {
            account: account.into(),
            sender_aci: sender.to_string(),
            timestamp,
        },
        text: Zeroizing::new(text.into()),
    })
}

/// Recipient receipt from an authenticated account-owned daemon stream.
/// It is evidence only after correlation with that account's native journal.
pub struct Receipt {
    pub account: String,
    pub sender_aci: String,
    pub timestamps: Vec<u64>,
    pub evidence: crate::journal::RecipientEvidence,
}

/// Decode direct recipient evidence without normalizing names or phone hints.
/// # Errors
/// Refuses another account, invalid/ambiguous types, viewed-only messages,
/// unknown receipt fields and unbounded/invalid timestamp lists.
pub fn receipt(frame: &[u8], account: &str) -> Result<Receipt, Error> {
    let value = decode(frame)?;
    if value.get("method").and_then(Value::as_str) != Some("receive") {
        return Err(Error::Unsupported);
    }
    let params = value.get("params").ok_or(Error::InvalidFrame)?;
    let received = params.get("result").unwrap_or(params);
    if received.get("account").and_then(Value::as_str) != Some(account) {
        return Err(Error::WrongAccount);
    }
    let envelope = received.get("envelope").ok_or(Error::InvalidFrame)?;
    let sender = envelope
        .get("sourceUuid")
        .and_then(Value::as_str)
        .ok_or(Error::InvalidIdentity)?;
    let aci = uuid::Uuid::parse_str(sender).map_err(|_| Error::InvalidIdentity)?;
    if aci.to_string() != sender {
        return Err(Error::InvalidIdentity);
    }
    let receipt = envelope
        .get("receiptMessage")
        .and_then(Value::as_object)
        .ok_or(Error::Unsupported)?;
    if receipt.keys().any(|field| {
        !["when", "isDelivery", "isRead", "isViewed", "timestamps"].contains(&field.as_str())
    }) {
        return Err(Error::Unsupported);
    }
    if receipt
        .get("when")
        .and_then(Value::as_u64)
        .is_none_or(|time| time == 0)
    {
        return Err(Error::InvalidFrame);
    }
    let delivery = receipt
        .get("isDelivery")
        .and_then(Value::as_bool)
        .ok_or(Error::InvalidFrame)?;
    let read = receipt
        .get("isRead")
        .and_then(Value::as_bool)
        .ok_or(Error::InvalidFrame)?;
    let viewed = receipt
        .get("isViewed")
        .map_or(Some(false), Value::as_bool)
        .ok_or(Error::InvalidFrame)?;
    let evidence = match (delivery, read, viewed) {
        (true, false, false) => crate::journal::RecipientEvidence::Delivered,
        (false, true, false) => crate::journal::RecipientEvidence::Read,
        _ => return Err(Error::Unsupported),
    };
    let times = receipt
        .get("timestamps")
        .and_then(Value::as_array)
        .filter(|times| !times.is_empty() && times.len() <= 512)
        .ok_or(Error::InvalidFrame)?;
    let timestamps = times
        .iter()
        .map(|time| {
            time.as_u64()
                .filter(|time| *time != 0)
                .ok_or(Error::InvalidFrame)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Receipt {
        account: account.into(),
        sender_aci: sender.into(),
        timestamps,
        evidence,
    })
}

fn decode(frame: &[u8]) -> Result<Value, Error> {
    if frame.len() > MAX_FRAME_BYTES {
        return Err(Error::FrameTooLarge);
    }
    let value: Value = serde_json::from_slice(frame).map_err(|_| Error::InvalidFrame)?;
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(Error::InvalidFrame);
    }
    Ok(value)
}
