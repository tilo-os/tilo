//! Work a workspace's key holders do once: one key holder takes it under a
//! lease, does it, and keeps that it is done.
//!
//! A workspace's state is every key holder's, so any of them may be the one
//! that sends a message from its outbox or asks the person about a page
//! submission. A side effect is not state: two key holders that do the same
//! work do it twice, and a lease ([`super::leases`]) only says who does it
//! while it is held. So the one that did it keeps what it answered in the
//! workspace ([`DONE_KEY`]), under the id of the lease it was done under.
//!
//! What is kept is per device, like a lease claim: each device writes the row of
//! the work it did, rows merge by union, and the earliest row is the answer
//! every device reads. A device that takes work another one already did
//! answers what was kept instead of doing it again, so a takeover — a machine
//! restarted, a lease taken over after its holder stopped, the same work
//! arriving later at another holder — never does it twice. Work that failed
//! leaves no row, so the next holder tries again and nothing is dropped.
//!
//! Neither is a server's: a group of desktops with no server between them
//! holds the same lease and keeps the same rows.

use super::SystemTime;
use super::facades::{PeerWorkspace, PeerWorkspaceMut};
use super::{device_rows, now_ns};
use serde::{Deserialize, Serialize};
/// Key of the work a workspace's key holders did, in its own document: a map,
/// there from the workspace's founding, from `<lease id>/<device key hex>` of
/// each device that did it to what it answered ([`device_rows`]).
pub const DONE_KEY: &str = "done_v1";

/// The lease a message is sent under, named by the outbox entry the message
/// is: one message, one lease, so two key holders never work on two messages
/// through one place.
#[must_use]
pub fn mail_send_lease(message: &str) -> String {
    format!("mail-send/{message}")
}

/// What one device kept of work it did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Done {
    /// The device that did it.
    pub device: [u8; 32],
    /// When it was done, ns since the epoch, on that device's clock.
    pub done_at_ns: u64,
    /// What the work answered, as the device that did it answered its caller.
    pub answer: serde_json::Value,
}

/// How a row is stored: the JSON every device reads.
#[derive(Serialize, Deserialize)]
struct StoredDone {
    done_at_ns: u64,
    answer: serde_json::Value,
}

/// The rows of the work done under `id` in `doc`.
fn rows_of(doc: Option<&automerge::AutoCommit>, id: &str) -> Vec<Done> {
    device_rows::rows_of(doc, DONE_KEY, id)
        .into_iter()
        .filter_map(|(device, json)| {
            let stored = serde_json::from_str::<StoredDone>(&json).ok()?;
            Some(Done {
                device,
                done_at_ns: stored.done_at_ns,
                answer: stored.answer,
            })
        })
        .collect()
}

/// The work `rows` say was done: the earliest row, so work two key holders
/// did while neither could see the other answers as the one done first,
/// alike on every device.
fn earliest(rows: Vec<Done>) -> Option<Done> {
    rows.into_iter()
        .min_by(|left, right| (left.done_at_ns, left.device).cmp(&(right.done_at_ns, right.device)))
}

impl PeerWorkspace<'_> {
    /// What a key holder kept of the work done under the lease `id`, if one
    /// did it.
    #[must_use]
    pub fn done(self, id: &str) -> Option<Done> {
        earliest(rows_of(self.peer.core.workspace_doc.as_deref(), id))
    }

    /// What was kept of the work done under every lease whose id starts with `prefix`, by lease
    /// id, each as [`Self::done`] reads it.
    #[must_use]
    pub fn done_under(self, prefix: &str) -> std::collections::BTreeMap<String, Done> {
        let mut rows = std::collections::BTreeMap::<String, Vec<Done>>::new();
        for (id, device, json) in
            device_rows::rows_under(self.peer.core.workspace_doc.as_deref(), DONE_KEY, prefix)
        {
            let Some(stored) = tilo_failure::ignored!(
                "a done row another build wrote is one this build cannot read, as `rows_of` reads it",
                serde_json::from_str::<StoredDone>(&json).ok()
            ) else {
                continue;
            };
            rows.entry(id).or_default().push(Done {
                device,
                done_at_ns: stored.done_at_ns,
                answer: stored.answer,
            });
        }
        rows.into_iter()
            .filter_map(|(id, rows)| Some((id, earliest(rows)?)))
            .collect()
    }
}

impl PeerWorkspaceMut<'_> {
    /// Keep that `device`, a member's device whose exact current
    /// certificate this workspace holds and who may change it, did the work
    /// of the lease `id`, and what it answered. A row already kept for that
    /// device stays: one device did it once. Answers whether the workspace
    /// changed.
    ///
    /// # Errors
    ///
    /// `device` is not allowed to change this workspace, or what is kept cannot
    /// be written.
    pub fn keep_done(
        &mut self,
        device: &tilo_user_identity::DeviceCert,
        id: &str,
        answer: serde_json::Value,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.check_writer(device, now)?;
        if rows_of(self.peer.core.workspace_doc.as_deref(), id)
            .iter()
            .any(|done| done.device == device.device_public_key)
        {
            return Ok(false);
        }
        let json = serde_json::to_string(&StoredDone {
            done_at_ns: now_ns(now),
            answer,
        })
        .map_err(|error| error.to_string())?;
        let key = device_rows::row_key(id, &device.device_public_key);
        self.write_row(DONE_KEY, &key, &device.device_public_key, Some(&json), now)?;
        Ok(true)
    }
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "test arithmetic moves a deadline"
)]
mod tests {
    use super::*;
    use crate::fixture::member_cert;
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
        let now = crate::fixture::at();
        let left_cert = cert_of(&left);
        let right_cert = cert_of(&right);
        let message = "outbox-1";

        let claim = take(&mut left, message, now, 60).expect("the first taker holds it");
        assert_eq!(claim.device, left_cert.device_public_key);
        assert!(
            left.workspace().done(&mail_send_lease(message)).is_none(),
            "nothing sent yet"
        );
        assert!(
            left.workspace_mut()
                .keep_done(&left_cert, &mail_send_lease(message), sent(""), now)
                .unwrap()
        );
        let left_over = left
            .workspace()
            .done(&mail_send_lease(message))
            .expect("the send is kept");
        assert_eq!(left_over.device, left_cert.device_public_key);
        assert_eq!(left_over.answer, sent(""));

        // The other device reads the same send from the merged workspace, and
        // takes no lease for it: it is done.
        sync(&left, &mut right);
        assert_eq!(
            right.workspace().done(&mail_send_lease(message)),
            Some(left_over.clone())
        );
        assert!(
            right
                .workspace_mut()
                .keep_done(&right_cert, &mail_send_lease(message), sent("again"), now)
                .unwrap(),
            "a row of its own is written"
        );
        assert_eq!(
            right.workspace().done(&mail_send_lease(message)),
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
        let now = crate::fixture::at();
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
        assert!(right.workspace().done(&mail_send_lease(message)).is_none());
        let taken = take(&mut right, message, over, 60).expect("the over lease is taken");
        assert_eq!(taken.device, right_cert.device_public_key);
        assert!(
            right
                .workspace_mut()
                .keep_done(&right_cert, &mail_send_lease(message), sent(""), over)
                .unwrap()
        );
        sync(&right, &mut left);
        assert_eq!(
            left.workspace()
                .done(&mail_send_lease(message))
                .map(|sent| sent.device),
            Some(right_cert.device_public_key),
            "the takeover's send is the one kept"
        );
    }

    #[test]
    fn two_senders_after_a_partition_merge_on_one_winner_and_send_no_more() {
        let (mut left, _left_key, mut right, _right_key) = desktops();
        let now = crate::fixture::at();
        let left_cert = cert_of(&left);
        let right_cert = cert_of(&right);
        assert!(
            left_cert.device_public_key < right_cert.device_public_key,
            "the fixture's left device has the lesser key"
        );
        let message = "outbox-3";
        // The partition: neither sees the other, so both take the message.
        let left_claim =
            take(&mut left, message, now, 60).expect("alone, the left device takes it");
        let right_claim =
            take(&mut right, message, now, 60).expect("alone, the right device takes it too");
        assert_eq!(left_claim.term, right_claim.term);

        // The partition ends. The claims rank the same on both devices, so
        // the lesser device key holds; only it sends, and the loser stops and
        // withdraws its claim.
        sync(&left, &mut right);
        sync(&right, &mut left);
        let winner = right.workspace().lease(&mail_send_lease(message), now);
        assert_eq!(winner.holder, Some(left_claim), "the tie goes one way");
        assert_eq!(winner.claims.len(), 2, "both claims merge");
        assert_eq!(
            winner,
            left.workspace().lease(&mail_send_lease(message), now)
        );
        assert!(
            take(&mut right, message, now, 60).is_none(),
            "the loser does not send, and withdraws its claim"
        );
        sync(&right, &mut left);
        assert_eq!(
            left.workspace()
                .lease(&mail_send_lease(message), now)
                .claims,
            vec![left_claim],
            "one claim is left"
        );
        assert!(right.workspace().done(&mail_send_lease(message)).is_none());
        assert!(left.workspace().done(&mail_send_lease(message)).is_none());
    }

    #[test]
    fn a_device_that_may_not_change_the_workspace_keeps_no_send() {
        let (mut left, _left_key, _, _right_key) = desktops();
        let now = crate::fixture::at();
        let stranger_root = ed25519_dalek::SigningKey::from_bytes(&[77; 32]);
        let stranger_key = ed25519_dalek::SigningKey::from_bytes(&[78; 32]);
        let stranger = member_cert(&stranger_root, &stranger_key, "stranger");
        assert!(
            left.workspace_mut()
                .keep_done(&stranger, &mail_send_lease("outbox-4"), sent(""), now)
                .is_err()
        );
        assert!(
            left.workspace()
                .done(&mail_send_lease("outbox-4"))
                .is_none()
        );
    }
}
