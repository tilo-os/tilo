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
///
/// # Errors
/// Refuses another service, invalid operation IDs and empty/oversized text.
pub fn send(route: &Route, operation: &str, text: &str) -> Result<Zeroizing<Vec<u8>>, Error> {
    send_on(route, route.account(), operation, text)
}

/// Encode with the host's verified signal-cli account. Tilo's account registry
/// ID is distinct from a Signal phone number/ACI and stays in the frozen route.
///
/// # Errors
/// Refuses another service, invalid identities/operation IDs and invalid text.
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

/// Decode one matching send response without interpreting it as delivery.
///
/// # Errors
/// Refuses oversized/malformed frames, mismatched IDs and remote errors.
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

/// Account-scoped incoming identity. Do not deduplicate globally by timestamp.
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

/// Decode a direct text event bound to the broker's chosen account.
///
/// # Errors
/// Refuses wrong-account, malformed, oversized and unsupported events.
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

#[cfg(test)]
mod tests {
    use super::*;

    const ACI: &str = "11111111-1111-4111-8111-111111111111";

    fn notification(account: &str) -> Value {
        json!({"jsonrpc":"2.0","method":"receive","params":{"account":account,
            "envelope":{"sourceUuid":ACI,"dataMessage":{"timestamp":42,"message":"hello",
                "attachments":[],"contacts":[],"mentions":[],"viewOnce":false,"expiresInSeconds":0,
                "isExpirationUpdate":false,"isEndSession":false,"isProfileKeyUpdate":false,"hasProfileKey":true}}}})
    }

    #[test]
    fn plain_text_never_flattens_another_envelope_kind() {
        for kind in [
            "editMessage",
            "storyMessage",
            "syncMessage",
            "callMessage",
            "receiptMessage",
            "typingMessage",
        ] {
            for value in [json!({}), json!([]), json!(false), json!("unexpected")] {
                let mut event = notification("account-a");
                event["params"]["envelope"][kind] = value;
                assert!(matches!(
                    incoming(&serde_json::to_vec(&event).unwrap(), "account-a"),
                    Err(Error::Unsupported)
                ));
            }
            let mut event = notification("account-a");
            event["params"]["envelope"][kind] = Value::Null;
            assert!(incoming(&serde_json::to_vec(&event).unwrap(), "account-a").is_ok());
        }
    }

    #[test]
    fn numberless_accounts_and_json_escaping_preserve_exact_destination() {
        let route = Route::new(Service::Signal, ACI.into(), "u:person.000".into()).unwrap();
        let wire = send(&route, "op-1", "hello\n\"recipient\":\"other\"").unwrap();
        assert_eq!(wire.last(), Some(&b'\n'));
        let value: Value = serde_json::from_slice(&wire).unwrap();
        assert_eq!(value["params"]["account"], ACI);
        assert_eq!(value["params"]["recipient"], json!(["u:person.000"]));
        assert_eq!(value["params"]["message"], "hello\n\"recipient\":\"other\"");
        assert!(!wire.strip_suffix(b"\n").unwrap().contains(&b'\n'));
    }

    #[test]
    fn acknowledgment_matches_request_and_never_invents_delivery() {
        let wire = br#"{"jsonrpc":"2.0","id":"op-1","result":{"timestamp":42}}"#;
        assert_eq!(acknowledgment(wire, "op-1").unwrap().timestamp, 42);
        assert_eq!(acknowledgment(wire, "op-2"), Err(Error::WrongRequest));
        assert_eq!(
            acknowledgment(
                br#"{"jsonrpc":"2.0","id":"op-1","error":{"message":"secret"}}"#,
                "op-1"
            ),
            Err(Error::RemoteRejected)
        );
        assert_eq!(
            acknowledgment(br#"{"jsonrpc":"2.0","id":"op-1","result":{}}"#, "op-1"),
            Err(Error::InvalidFrame)
        );
    }

    #[test]
    fn subscription_and_direct_notifications_keep_account_and_sender_identity() {
        let direct = notification(ACI);
        let mut subscription = direct.clone();
        subscription["params"] = json!({"subscription":0,"result":direct["params"]});
        let a = incoming(&serde_json::to_vec(&direct).unwrap(), ACI).unwrap();
        let b = incoming(&serde_json::to_vec(&subscription).unwrap(), ACI).unwrap();
        assert!(a.id == b.id);
        assert_eq!(a.text.as_str(), "hello");
        assert!(matches!(
            incoming(&serde_json::to_vec(&direct).unwrap(), "other"),
            Err(Error::WrongAccount)
        ));
        let other = incoming(
            &serde_json::to_vec(&notification("other")).unwrap(),
            "other",
        )
        .unwrap();
        assert!(a.id != other.id);
        assert!(!format!("{a:?}").contains("hello"));
    }

    #[test]
    fn signal_cli_0148_plain_text_metadata_keeps_exact_incoming_identity() {
        // JsonDataMessage v0.14.8 serializes these Boolean flags even when
        // false; hasProfileKey is metadata, not a profile-key update or grant.
        for has_profile_key in [false, true] {
            let mut event = notification(ACI);
            event["params"]["envelope"]["dataMessage"] = json!({
                "timestamp":42, "message":"hello", "expiresInSeconds":0,
                "isExpirationUpdate":false, "viewOnce":false,
                "isEndSession":false, "isProfileKeyUpdate":false,
                "hasProfileKey":has_profile_key
            });
            let parsed = incoming(&serde_json::to_vec(&event).unwrap(), ACI).unwrap();
            assert_eq!(parsed.text.as_str(), "hello");
            assert_eq!(parsed.id.account, ACI);
            assert_eq!(parsed.id.sender_aci, ACI);
            assert_eq!(parsed.id.timestamp, 42);
            assert!(matches!(
                incoming(&serde_json::to_vec(&event).unwrap(), "other"),
                Err(Error::WrongAccount)
            ));
        }
    }

    #[test]
    fn plain_text_metadata_never_admits_control_events_or_malformed_flags() {
        for field in ["isExpirationUpdate", "isEndSession", "isProfileKeyUpdate"] {
            for value in [json!(true), json!(null), json!(0), json!("false")] {
                let mut event = notification(ACI);
                event["params"]["envelope"]["dataMessage"][field] = value;
                assert!(matches!(
                    incoming(&serde_json::to_vec(&event).unwrap(), ACI),
                    Err(Error::Unsupported)
                ));
            }
        }
        for value in [json!(null), json!(0), json!("true")] {
            let mut event = notification(ACI);
            event["params"]["envelope"]["dataMessage"]["hasProfileKey"] = value;
            assert!(matches!(
                incoming(&serde_json::to_vec(&event).unwrap(), ACI),
                Err(Error::Unsupported)
            ));
        }
    }

    #[test]
    fn unsupported_messages_are_not_silently_flattened_or_misrouted() {
        for (field, value) in [
            ("groupInfo", json!({"groupId":"group"})),
            ("viewOnce", json!(true)),
            ("expiresInSeconds", json!(60)),
            ("attachments", json!([{"id":"photo"}])),
        ] {
            let mut notification = notification(ACI);
            notification["params"]["envelope"]["dataMessage"][field] = value;
            assert!(matches!(
                incoming(&serde_json::to_vec(&notification).unwrap(), ACI),
                Err(Error::Unsupported)
            ));
        }
        let too_big = vec![b' '; MAX_FRAME_BYTES + 1];
        assert!(matches!(incoming(&too_big, ACI), Err(Error::FrameTooLarge)));
        assert!(matches!(incoming(b"{}\n{}", ACI), Err(Error::InvalidFrame)));
    }

    #[test]
    fn a_display_name_cannot_be_used_as_the_authenticated_sender_aci() {
        let mut event = notification(ACI);
        event["params"]["envelope"]["sourceUuid"] = json!("Alice");
        assert!(matches!(
            incoming(&serde_json::to_vec(&event).unwrap(), ACI),
            Err(Error::InvalidIdentity)
        ));
    }

    #[test]
    fn receipt_codec_preserves_exact_account_and_rejects_ambiguous_or_unbounded_evidence() {
        let event = json!({"jsonrpc":"2.0","method":"receive","params":{"account":ACI,
            "envelope":{"sourceUuid":ACI,"receiptMessage":{"when":44,"isDelivery":false,
            "isRead":true,"isViewed":false,"timestamps":[42,43]}}}});
        let decoded = receipt(&serde_json::to_vec(&event).unwrap(), ACI).unwrap();
        assert_eq!(decoded.account, ACI);
        assert_eq!(decoded.sender_aci, ACI);
        assert_eq!(decoded.timestamps, vec![42, 43]);
        assert_eq!(decoded.evidence, crate::journal::RecipientEvidence::Read);
        assert!(matches!(
            receipt(&serde_json::to_vec(&event).unwrap(), "other"),
            Err(Error::WrongAccount)
        ));
        for (field, value) in [
            ("isDelivery", json!(true)),
            ("isViewed", json!(true)),
            ("isRead", json!("true")),
            ("timestamps", json!([])),
            ("timestamps", json!([0])),
            ("timestamps", json!([-1])),
            ("timestamps", json!(vec![42; 513])),
            ("when", json!(0)),
            ("unknown", json!(true)),
        ] {
            let mut invalid = event.clone();
            invalid["params"]["envelope"]["receiptMessage"][field] = value;
            assert!(receipt(&serde_json::to_vec(&invalid).unwrap(), ACI).is_err());
        }
        let mut wrong_sender = event;
        wrong_sender["params"]["envelope"]["sourceUuid"] = json!("Alex");
        assert!(matches!(
            receipt(&serde_json::to_vec(&wrong_sender).unwrap(), ACI),
            Err(Error::InvalidIdentity)
        ));
    }
}
