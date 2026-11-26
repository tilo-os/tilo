//! One key signing its own admission in two places at once, both on the governance they were
//! sent and the same record of what the key signed before: a daemon letting one agent key in for
//! two connections that open together, or a server and a session of one device. However the two
//! signings meet, the key never writes two changes under one number.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};

/// A key that, the first time it signs, waits until the other signer has started signing too:
/// both read what the key signed before while neither has kept anything yet.
struct MeetsTheOther {
    key: SigningKey,
    met: Arc<Barrier>,
    waited: AtomicBool,
}

impl collab_types::signer::DeviceSigner for MeetsTheOther {
    fn device_public_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    fn sign(&self, message: Vec<u8>) -> collab_types::signer::SignFuture<'_> {
        if !self.waited.swap(true, Ordering::SeqCst) {
            self.met.wait();
        }
        collab_types::signer::DeviceSigner::sign(&self.key, message)
    }
}

/// An invitation the owner (`authority`) issued for `guest`'s person, its own nonce `n`, as
/// redeemed by `guest`'s device.
fn invited(
    peer: &StatePeer,
    authority: &SigningKey,
    guest: &DeviceCert,
    n: u8,
    now: SystemTime,
) -> crate::join::VerifiedRedemption {
    let signed = crate::fixture::invitation(peer, authority, now, |grant| {
        grant.nonce = [n; 16];
        grant.recipient_user_public_key = Some(guest.user_public_key);
    });
    crate::join::verify_connection_redemption(
        &signed,
        guest,
        [n; 32],
        &RevocationList::new(),
        crate::fixture::ns(now),
    )
    .unwrap()
}

#[test]
fn one_key_signing_in_two_places_at_once_never_takes_one_number_twice() {
    let (peer, authority_key, _, _, now) = seeded();
    let (key, guest) = crate::fixture::device(171, 173);
    let workspace = peer.workspace_id().to_owned();
    let governance = vec![postcard::to_stdvec(&peer.workspace_message()).unwrap()];
    let met = Arc::new(Barrier::new(2));

    // Each place redeems an invitation of its own for the key, as a host invites a key again for
    // every connection that asks to bring it in.
    let signed: Vec<Vec<Vec<u8>>> = std::thread::scope(|scope| {
        [181, 182]
            .map(|n| {
                let redemption = invited(&peer, &authority_key, &guest, n, now);
                let signer = MeetsTheOther {
                    key: key.clone(),
                    met: Arc::clone(&met),
                    waited: AtomicBool::new(false),
                };
                let (workspace, governance) = (&workspace, &governance);
                scope.spawn(move || {
                    crate::own_admission::own_admission(
                        workspace,
                        governance,
                        &redemption,
                        None,
                        &signer,
                        now,
                    )
                    .unwrap()
                })
            })
            .map(|signing| signing.join().unwrap())
            .into_iter()
            .collect()
    });

    let mut numbers = std::collections::BTreeMap::new();
    for bytes in signed.iter().flatten() {
        let change = automerge::Change::try_from(bytes.as_slice()).unwrap();
        let number = (change.actor_id().to_hex_string(), change.seq());
        if let Some(other) = numbers.insert(number.clone(), change.hash()) {
            assert_eq!(
                other,
                change.hash(),
                "two changes of one writer under one number: {number:?}"
            );
        }
    }

    // Every copy takes both places' admissions in, one after the other.
    let mut receiver = peer.clone();
    for changes in &signed {
        assert!(receiver.admit_own(&guest, changes.clone(), now).unwrap());
    }

    // And the key goes on signing there: its next admission builds on both.
    let again = invited(&peer, &authority_key, &guest, 183, now);
    crate::own_admission::own_admission(&workspace, &governance, &again, None, &key, now).unwrap();
}
