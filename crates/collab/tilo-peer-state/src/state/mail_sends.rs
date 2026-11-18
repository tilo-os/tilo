//! How a workspace's mail outbox is sent once: one key holder takes a
//! message, sends it, and keeps that it left.
//!
//! A workspace's outgoing mail is workspace state, so every key holder of it
//! reads the same outbox, and any of them may be the one that hands a message
//! to its account's server. A side effect is not state: two key holders that
//! send the same message send it twice. So each message is taken under a
//! lease of the message's own ([`MAIL_SEND_LEASE`], one id per outbox entry),
//! and the sender keeps what left in the workspace ([`MAIL_SENDS_KEY`]).
//!
//! What is kept is per device, like a lease claim: each device writes the row of
//! the message it sent, rows merge by union, and the earliest row is the send
//! every device reads. A device that takes a message another one already sent
//! answers the send already kept instead of sending it again, so a takeover — a
//! machine restarted, a lease taken over after its holder stopped — never sends
//! one twice. A send that failed leaves no row, so the next holder tries
//! again and nothing is dropped.
//!
//! Neither is a server's: a group of desktops with no server between them
//! holds the same lease and keeps the same rows.

use super::SystemTime;
use super::facades::{PeerWorkspace, PeerWorkspaceMut};
use super::{ROOT, now_ns};
use automerge::transaction::Transactable as _;
use automerge::{ObjType, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};

/// Key of the messages a workspace's key holders sent, in its own document: a
/// map from each message's id to a map from each sending device key (hex) to
/// what it sent.
pub const MAIL_SENDS_KEY: &str = "mail_sends_v1";

/// The lease a message is sent under, named by the outbox entry the message
/// is: one message, one lease, so two key holders never work on two messages
/// through one place.
#[must_use]
pub fn mail_send_lease(message: &str) -> String {
    format!("mail-send/{message}")
}

/// What one device recorded of a message it sent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MailSent {
    /// The device that handed it to its account's server.
    pub device: [u8; 32],
    /// When it left, ms since the epoch, on that device's clock.
    pub sent_at_ms: u64,
    /// What the send answered: the account's Sent folder's verdict, as the
    /// sender answered the caller.
    pub answer: serde_json::Value,
}

/// How a send is stored: the JSON every device reads.
#[derive(Serialize, Deserialize)]
struct StoredSent {
    sent_at_ms: u64,
    answer: serde_json::Value,
}

/// The rows of the message `message` in `doc`.
fn rows_of(doc: Option<&automerge::AutoCommit>, message: &str) -> Vec<MailSent> {
    let Some(doc) = doc else {
        return Vec::new();
    };
    let Ok(Some((Value::Object(ObjType::Map), sends))) = doc.get(ROOT, MAIL_SENDS_KEY) else {
        return Vec::new();
    };
    let Ok(Some((Value::Object(ObjType::Map), devices))) = doc.get(&sends, message) else {
        return Vec::new();
    };
    let mut sent = Vec::new();
    for device in doc.keys(&devices) {
        let Ok(Some((Value::Scalar(value), _))) = doc.get(&devices, device.as_str()) else {
            continue;
        };
        let ScalarValue::Str(json) = value.as_ref() else {
            continue;
        };
        let (Ok(stored), Some(key)) =
            (serde_json::from_str::<StoredSent>(json), device_of(&device))
        else {
            continue;
        };
        sent.push(MailSent {
            device: key,
            sent_at_ms: stored.sent_at_ms,
            answer: stored.answer,
        });
    }
    sent
}

/// The send `rows` say: the earliest one, so a message two key holders
/// sent while neither could see the other answers as the one that left first,
/// alike on every device.
fn earliest(rows: Vec<MailSent>) -> Option<MailSent> {
    rows.into_iter()
        .min_by(|left, right| (left.sent_at_ms, left.device).cmp(&(right.sent_at_ms, right.device)))
}

/// The key a device row's hex names, if it is one.
fn device_of(device: &str) -> Option<[u8; 32]> {
    let key = tilo_failure::ignored!(
        "a device row another build wrote is not a key this one can name",
        hex::decode(device).ok()
    )?;
    <[u8; 32]>::try_from(key.as_slice()).ok()
}

impl PeerWorkspace<'_> {
    /// What a key holder recorded of the message `message` having left, if
    /// one did.
    #[must_use]
    pub fn mail_sent(self, message: &str) -> Option<MailSent> {
        earliest(rows_of(self.peer.core.workspace_doc.as_deref(), message))
    }
}

impl PeerWorkspaceMut<'_> {
    /// Keep that `device`, a member's device whose exact current
    /// certificate this workspace holds and who may change it, sent the
    /// message `message`, and what the send answered. A row already kept for
    /// that device stays: one device sent it once. Answers whether the
    /// workspace changed.
    ///
    /// # Errors
    ///
    /// `device` is not allowed to change this workspace, or what is kept cannot
    /// be written.
    pub fn keep_mail_sent(
        &mut self,
        device: &tilo_user_identity::DeviceCert,
        message: &str,
        answer: serde_json::Value,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.check_writer(device, now)?;
        let row = hex::encode(device.device_public_key);
        let kept = {
            let doc = self.peer.core.workspace_doc.as_deref();
            doc.and_then(|doc| {
                let (Value::Object(ObjType::Map), sends) = doc.get(ROOT, MAIL_SENDS_KEY).ok()??
                else {
                    return None;
                };
                let (Value::Object(ObjType::Map), devices) = doc.get(&sends, message).ok()?? else {
                    return None;
                };
                doc.get(&devices, row.as_str()).ok()?
            })
            .is_some()
        };
        if kept {
            return Ok(false);
        }
        let json = serde_json::to_string(&StoredSent {
            sent_at_ms: now_ns(now),
            answer,
        })
        .map_err(|error| error.to_string())?;
        let actor = self.peer.workspace_actor_for(&device.device_public_key);
        let workspace = self
            .peer
            .core
            .workspace_doc
            .as_deref()
            .ok_or("mail sends workspace is absent")?;
        let mut candidate = workspace.clone();
        candidate.set_actor(actor);
        let sends = match candidate.get(ROOT, MAIL_SENDS_KEY).map_err(error_text)? {
            Some((Value::Object(ObjType::Map), sends)) => sends,
            Some(_) => return Err("the workspace's mail sends are malformed".into()),
            None => candidate
                .put_object(ROOT, MAIL_SENDS_KEY, ObjType::Map)
                .map_err(error_text)?,
        };
        let devices = match candidate.get(&sends, message).map_err(error_text)? {
            Some((Value::Object(ObjType::Map), devices)) => devices,
            Some(_) => return Err(format!("message `{message}`'s sends are malformed")),
            None => candidate
                .put_object(&sends, message, ObjType::Map)
                .map_err(error_text)?,
        };
        candidate
            .put(&devices, row.as_str(), json.as_str())
            .map_err(error_text)?;
        super::commit_governance(
            &mut candidate,
            Some(&device.user_public_key),
            super::now_secs(now),
        );
        self.peer.core.workspace_doc = Some(candidate.into());
        Ok(true)
    }
}

/// A document operation's error as text.
fn error_text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "test arithmetic moves a deadline"
)]
mod tests {
    use super::*;
    use crate::owner::testing::now;
    use crate::state::leases::{CLOCK_SKEW_NS, unranked};
    use crate::state::test_desktops::{cert_of, desktops, sync};
    use web_time::Duration;

    fn at(now: SystemTime, seconds: u64) -> SystemTime {
        now + Duration::from_secs(seconds)
    }

    fn sent(answer: &str) -> serde_json::Value {
        serde_json::json!({ "unfiled": answer })
    }

    /// Take the message's lease as `left`, answering whether it holds it.
    fn take(
        peer: &mut crate::StatePeer,
        message: &str,
        now: SystemTime,
        ttl: u64,
    ) -> Option<crate::LeaseClaim> {
        let cert = cert_of(peer);
        peer.workspace_mut()
            .take_lease(
                &cert,
                &mail_send_lease(message),
                Duration::from_secs(ttl),
                unranked(),
                now,
            )
            .unwrap()
    }

    #[test]
    fn one_key_holder_sends_a_message_and_the_other_finds_it_sent() {
        let (mut left, _left_key, mut right, _right_key) = desktops();
        let now = now();
        let left_cert = cert_of(&left);
        let right_cert = cert_of(&right);
        let message = "outbox-1";

        let claim = take(&mut left, message, now, 60).expect("the first taker holds it");
        assert_eq!(claim.device, left_cert.device_public_key);
        assert!(
            left.workspace().mail_sent(message).is_none(),
            "nothing sent yet"
        );
        assert!(
            left.workspace_mut()
                .keep_mail_sent(&left_cert, message, sent(""), now)
                .unwrap()
        );
        let left_over = left
            .workspace()
            .mail_sent(message)
            .expect("the send is kept");
        assert_eq!(left_over.device, left_cert.device_public_key);
        assert_eq!(left_over.answer, sent(""));

        // The other device reads the same send from the merged workspace, and
        // takes no lease for it: it is done.
        sync(&left, &mut right);
        assert_eq!(
            right.workspace().mail_sent(message),
            Some(left_over.clone())
        );
        assert!(
            right
                .workspace_mut()
                .keep_mail_sent(&right_cert, message, sent("again"), now)
                .unwrap(),
            "a row of its own is written"
        );
        assert_eq!(
            right.workspace().mail_sent(message),
            Some(left_over),
            "the earliest send is the one every device answers, so nothing sends again"
        );

        // The lease is released when the send is done, so the next message —
        // or the same id sent again by a person — is free to take.
        left.workspace_mut()
            .release_lease(&left_cert, &mail_send_lease(message), now)
            .unwrap();
        assert!(
            left.workspace()
                .lease(&mail_send_lease(message), now)
                .is_free()
        );
    }

    #[test]
    fn a_message_its_sender_stopped_on_is_taken_over_once_and_sent_once() {
        let (mut left, _left_key, mut right, _right_key) = desktops();
        let now = now();
        let right_cert = cert_of(&right);
        let message = "outbox-2";

        assert!(take(&mut left, message, now, 60).is_some());
        sync(&left, &mut right);
        assert!(
            take(&mut right, message, now, 60).is_none(),
            "a live lease is not taken over"
        );
        // The sender stops without recording anything. Only once its claim is
        // over by the clock-skew bound does the other device take it, and it
        // sends the message the first one never did: nothing is dropped.
        let over = at(now, 62 + CLOCK_SKEW_NS / 1_000_000_000);
        assert!(right.workspace().mail_sent(message).is_none());
        let taken = take(&mut right, message, over, 60).expect("the over lease is taken");
        assert_eq!(taken.device, right_cert.device_public_key);
        assert!(
            right
                .workspace_mut()
                .keep_mail_sent(&right_cert, message, sent(""), over)
                .unwrap()
        );
        sync(&right, &mut left);
        assert_eq!(
            left.workspace().mail_sent(message).map(|sent| sent.device),
            Some(right_cert.device_public_key),
            "the takeover's send is the one kept"
        );
    }

    #[test]
    fn two_senders_after_a_partition_merge_on_one_winner_and_send_no_more() {
        let (mut left, _left_key, mut right, _right_key) = desktops();
        let now = now();
        let right_cert = cert_of(&right);
        let message = "outbox-3";
        // The partition: neither sees the other, so both take the message.
        let left_claim =
            take(&mut left, message, now, 60).expect("alone, the left device takes it");
        let right_claim =
            take(&mut right, message, now, 60).expect("alone, the right device takes it too");
        assert_eq!(left_claim.term, right_claim.term);

        // The partition ends. The claims rank the same on both devices, and
        // only the winner sends; the loser stops and withdraws its claim.
        sync(&left, &mut right);
        sync(&right, &mut left);
        let winner = left.workspace().lease(&mail_send_lease(message), now);
        let holder = winner.holder.expect("one claim wins").device;
        assert_eq!(holder, right_cert.device_public_key, "the better rank wins");
        assert_eq!(
            winner,
            right.workspace().lease(&mail_send_lease(message), now)
        );
        assert!(
            take(&mut left, message, now, 60).is_none(),
            "the loser does not send, and withdraws its claim"
        );
        sync(&left, &mut right);
        assert_eq!(
            right
                .workspace()
                .lease(&mail_send_lease(message), now)
                .claims
                .iter()
                .map(|claim| claim.device)
                .collect::<Vec<_>>(),
            vec![right_cert.device_public_key],
            "one claim is left"
        );
        assert!(right.workspace().mail_sent(message).is_none());
        assert!(left.workspace().mail_sent(message).is_none());
    }

    #[test]
    fn a_device_that_may_not_change_the_workspace_keeps_no_send() {
        let (mut left, _left_key, _, _right_key) = desktops();
        let now = now();
        let stranger_root = ed25519_dalek::SigningKey::from_bytes(&[77; 32]);
        let stranger_key = ed25519_dalek::SigningKey::from_bytes(&[78; 32]);
        let mut stranger = tilo_user_identity::DeviceCert {
            user_public_key: stranger_root.verifying_key().to_bytes(),
            device_public_key: stranger_key.verifying_key().to_bytes(),
            device_label: "stranger".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut stranger, &stranger_root).unwrap();
        assert!(
            left.workspace_mut()
                .keep_mail_sent(&stranger, "outbox-4", sent(""), now)
                .is_err()
        );
        assert!(left.workspace().mail_sent("outbox-4").is_none());
    }
}
