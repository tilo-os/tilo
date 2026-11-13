//! The workspace's invitation ledger: the receipt of each redemption, under
//! the invitation it redeemed, and the parts of signed invitations that many
//! receipts share.
//!
//! A receipt carries the signed invitation it redeemed, so that every device
//! checks the admission itself, whoever wrote it. The invitations a public
//! workspace's door hands out differ only in whom they name and when: each
//! carries the same issuer certificate and the same signed transport
//! details, about two kilobytes, which a workspace of hundreds would hold
//! hundreds of times. A receipt therefore keeps the bytes of its invitation
//! that are its own, and names each shared run of them by its digest; the
//! runs are kept once, under that digest, beside the receipts
//! ([`PARTS_KEY`]).
//!
//! Nothing is weaker for it: the invitation a receipt names is put back
//! together byte for byte, and is the invitation only when its digest is the
//! invitation id the receipt is kept under. Its signature and everything
//! else is then checked on those bytes, as before.
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::RedemptionReceipt;

/// The document key holding the receipts, by invitation id (hex).
pub(crate) const RECEIPTS_KEY: &str = "invitation_receipts_v1";
/// The document key holding the shared parts of invitations, by the hex
/// BLAKE3 digest of their bytes.
pub(crate) const PARTS_KEY: &str = "invitation_parts_v1";

/// A shared run is worth naming once it is longer than its name.
const SHARED_AT_LEAST: usize = 64;

/// One redemption as the ledger keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LedgerReceipt {
    pub(crate) receipt: RedemptionReceipt,
    /// The signed invitation redeemed, in order: its own bytes, and the
    /// shared runs named by their digest.
    pub(crate) invitation: Vec<Piece>,
}

/// A run of a signed invitation's bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Piece {
    /// Bytes of this invitation alone.
    Own(#[serde(with = "serde_bytes")] Vec<u8>),
    /// A run kept once in the ledger's parts, by the BLAKE3 digest of its
    /// bytes.
    Shared([u8; 32]),
}

impl LedgerReceipt {
    /// The receipt of `receipt`, redeeming `signed`, with each of `shared`
    /// that `signed` holds named rather than copied, and the parts it names.
    pub(crate) fn new(
        receipt: RedemptionReceipt,
        signed: &[u8],
        shared: &[&[u8]],
    ) -> (Self, Vec<Vec<u8>>) {
        let mut invitation = Vec::new();
        let mut parts = Vec::new();
        // Where in `signed` the bytes not yet taken begin.
        let mut rest = signed;
        for run in shared {
            if run.len() < SHARED_AT_LEAST {
                continue;
            }
            let Some(found) = rest.windows(run.len()).position(|window| window == *run) else {
                continue;
            };
            let (own, from) = rest.split_at(found);
            let Some(after) = from.get(run.len()..) else {
                continue;
            };
            if !own.is_empty() {
                invitation.push(Piece::Own(own.to_vec()));
            }
            invitation.push(Piece::Shared(*blake3::hash(run).as_bytes()));
            parts.push(run.to_vec());
            rest = after;
        }
        if !rest.is_empty() {
            invitation.push(Piece::Own(rest.to_vec()));
        }
        (
            Self {
                receipt,
                invitation,
            },
            parts,
        )
    }

    /// The receipt `bytes` hold.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }

    /// The bytes the ledger keeps for this receipt.
    pub(crate) fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_stdvec(self)
    }

    /// The signed invitation this receipt redeemed, each shared run read
    /// with `part`: exactly the bytes whose digest is the receipt's
    /// invitation id, or `None`.
    pub(crate) fn invitation<'a>(
        &self,
        part: impl Fn(&[u8; 32]) -> Option<&'a [u8]>,
    ) -> Option<Vec<u8>> {
        let mut signed = Vec::new();
        for piece in &self.invitation {
            match piece {
                Piece::Own(bytes) => signed.extend_from_slice(bytes),
                Piece::Shared(digest) => {
                    let bytes = part(digest)?;
                    if blake3::hash(bytes).as_bytes() != digest {
                        return None;
                    }
                    signed.extend_from_slice(bytes);
                }
            }
        }
        (*blake3::hash(&signed).as_bytes() == self.receipt.invite_id).then_some(signed)
    }

    /// The parts this receipt names.
    pub(crate) fn shared(&self) -> impl Iterator<Item = &[u8; 32]> {
        self.invitation.iter().filter_map(|piece| match piece {
            Piece::Shared(digest) => Some(digest),
            Piece::Own(_) => None,
        })
    }
}

/// The key a part is kept under: the hex BLAKE3 digest of its bytes.
pub(crate) fn part_key(digest: &[u8; 32]) -> String {
    hex::encode(digest)
}

/// The invitation parts `doc` keeps, by key. A part two devices wrote at
/// once is the same bytes under the same key, kept once.
pub(crate) fn parts(doc: &AutoCommit) -> Result<BTreeMap<String, Vec<Vec<u8>>>, String> {
    let mut parts: BTreeMap<String, Vec<Vec<u8>>> = BTreeMap::new();
    for (value, object) in doc.get_all(ROOT, PARTS_KEY).map_err(err)? {
        if !matches!(value, Value::Object(automerge::ObjType::Map)) {
            return Err("governed invitation parts are not a map".into());
        }
        for key in doc.keys(&object) {
            for (value, _) in doc.get_all(&object, &key).map_err(err)? {
                let Value::Scalar(value) = value else {
                    return Err("governed invitation part is not bytes".into());
                };
                let ScalarValue::Bytes(bytes) = value.as_ref() else {
                    return Err("governed invitation part is not bytes".into());
                };
                parts.entry(key.clone()).or_default().push(bytes.clone());
            }
        }
    }
    Ok(parts)
}

/// The part kept under `digest` in `parts`, as [`parts`] reads them.
pub(crate) fn part_in<'a>(
    parts: &'a BTreeMap<String, Vec<Vec<u8>>>,
    digest: &[u8; 32],
) -> Option<&'a [u8]> {
    parts
        .get(&part_key(digest))?
        .iter()
        .find(|bytes| blake3::hash(bytes).as_bytes() == digest)
        .map(Vec::as_slice)
}

/// Keep `parts` in `doc`'s ledger, each one the ledger does not hold yet.
/// One it holds is not written again: a write over it would read to every
/// receiver as a value taken back.
pub(crate) fn put_parts(doc: &mut AutoCommit, parts: &[Vec<u8>]) -> Result<(), String> {
    if parts.is_empty() {
        return Ok(());
    }
    let map = match doc.get(ROOT, PARTS_KEY).map_err(err)? {
        Some((Value::Object(automerge::ObjType::Map), object)) => object,
        Some(_) => return Err("governed invitation parts are not a map".into()),
        None => doc
            .put_object(ROOT, PARTS_KEY, automerge::ObjType::Map)
            .map_err(err)?,
    };
    for part in parts {
        let key = part_key(blake3::hash(part).as_bytes());
        if doc.get(&map, &key).map_err(err)?.is_none() {
            doc.put(&map, &key, ScalarValue::Bytes(part.clone()))
                .map_err(err)?;
        }
    }
    Ok(())
}

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(signed: &[u8]) -> RedemptionReceipt {
        RedemptionReceipt {
            invite_id: *blake3::hash(signed).as_bytes(),
            user_public_key: [1; 32],
            device_public_key: [2; 32],
            request_id: [3; 32],
            role: crate::join::Role::Editor,
        }
    }

    fn signed() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let issuer = vec![7; 150];
        let transport = vec![9; 300];
        let mut signed = vec![1, 2, 3];
        signed.extend_from_slice(&issuer);
        signed.extend_from_slice(&[4; 40]);
        signed.extend_from_slice(&transport);
        signed.extend_from_slice(&[5; 64]);
        (signed, issuer, transport)
    }

    #[test]
    fn a_receipt_keeps_its_own_bytes_and_puts_its_invitation_back_together() {
        let (signed, issuer, transport) = signed();
        let (ledgered, parts) =
            LedgerReceipt::new(receipt(&signed), &signed, &[&issuer, &transport]);
        assert_eq!(parts, [issuer.clone(), transport.clone()]);
        let encoded = ledgered.encode().unwrap();
        assert!(
            encoded.len() < signed.len() - issuer.len() - transport.len() + 300,
            "the shared runs are named, not copied: {} B",
            encoded.len()
        );
        let read = LedgerReceipt::decode(&encoded).unwrap();
        let kept: BTreeMap<String, Vec<Vec<u8>>> = parts
            .iter()
            .map(|part| (part_key(blake3::hash(part).as_bytes()), vec![part.clone()]))
            .collect();
        assert_eq!(
            read.invitation(|digest| part_in(&kept, digest)),
            Some(signed)
        );
    }

    #[test]
    fn a_tampered_receipt_or_part_names_no_invitation() {
        let (signed, issuer, transport) = signed();
        let (ledgered, parts) =
            LedgerReceipt::new(receipt(&signed), &signed, &[&issuer, &transport]);
        let kept: BTreeMap<String, Vec<Vec<u8>>> = parts
            .iter()
            .map(|part| (part_key(blake3::hash(part).as_bytes()), vec![part.clone()]))
            .collect();
        // A byte of the receipt's own changed: the invitation it spells is
        // not the one it names.
        let mut own_changed = ledgered.clone();
        for piece in &mut own_changed.invitation {
            if let Piece::Own(bytes) = piece {
                bytes[0] ^= 1;
                break;
            }
        }
        assert_eq!(
            own_changed.invitation(|digest| part_in(&kept, digest)),
            None
        );
        // A part changed under its key.
        let mut forged = kept.clone();
        for values in forged.values_mut() {
            values[0][0] ^= 1;
        }
        assert_eq!(ledgered.invitation(|digest| part_in(&forged, digest)), None);
        // A part missing.
        assert_eq!(ledgered.invitation(|_| None), None);
        // Another part named in its place, itself well formed.
        let mut swapped = ledgered;
        let other = vec![8; 150];
        for piece in &mut swapped.invitation {
            if let Piece::Shared(digest) = piece {
                *digest = *blake3::hash(&other).as_bytes();
                break;
            }
        }
        let mut with_other = kept;
        with_other.insert(part_key(blake3::hash(&other).as_bytes()), vec![other]);
        assert_eq!(
            swapped.invitation(|digest| part_in(&with_other, digest)),
            None
        );
    }

    #[test]
    fn a_run_it_does_not_hold_or_too_short_to_name_stays_its_own() {
        let (signed, _, _) = signed();
        let (ledgered, parts) =
            LedgerReceipt::new(receipt(&signed), &signed, &[&[6; 100], &[7; 8]]);
        assert!(parts.is_empty());
        assert_eq!(ledgered.invitation, [Piece::Own(signed.clone())]);
        assert_eq!(ledgered.invitation(|_| None), Some(signed));
    }
}
