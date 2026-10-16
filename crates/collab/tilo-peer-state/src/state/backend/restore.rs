//! Closed composition of authenticated durable input into bounded peer storage.
use super::super::bounded::{BoundedPeerError, BoundedPeerInvalid, BoundedStatePeer, PeerLimits};
use super::{
    BTreeMap, BoundedNative, ChannelState, DeviceCert, Ed25519PublicKey, NativeCheckpoint,
    PeerCore, PeerDocument, RevocationList, Route, SystemTime, durable, governed, now_ns,
    revocations, roster,
};
use crate::coverage::read::{BoundedCoverage, CoverageMatchError, governed_snapshot_matches};
use crate::resources::json::JsonLimits;
use crate::resources::native::NativeBatch;
use crate::room::media::{
    self,
    read::{BoundedDecode, PolicyValue},
};

fn invalid(value: BoundedPeerInvalid) -> BoundedPeerError {
    BoundedPeerError::Invalid(value)
}
fn policy_error(value: media::read::Error) -> BoundedPeerError {
    match value {
        media::read::Error::Capacity => BoundedPeerError::Capacity,
        media::read::Error::Decode(error) => BoundedPeerError::Decode(error),
        media::read::Error::Policy(_) => invalid(BoundedPeerInvalid::Media),
    }
}
fn roster_error(_: roster::RosterError) -> BoundedPeerError {
    invalid(BoundedPeerInvalid::Certificate)
}
fn revocation_error(value: &revocations::RestoreRevocationsError) -> BoundedPeerError {
    match value {
        revocations::RestoreRevocationsError::AuthorityRevoked => {
            invalid(BoundedPeerInvalid::AuthorityRevoked)
        }
        _ => invalid(BoundedPeerInvalid::Revocation),
    }
}
fn governance_error(value: &governed::ImportError) -> BoundedPeerError {
    match value {
        governed::ImportError::Capacity => BoundedPeerError::Capacity,
        governed::ImportError::Decode(error) => BoundedPeerError::Decode(*error),
        _ => invalid(BoundedPeerInvalid::Governance),
    }
}
#[expect(
    clippy::too_many_lines,
    reason = "one durable restore whose steps share its limits"
)]
pub(in crate::state) fn restore(
    bytes: &[u8],
    room: &str,
    authority: &DeviceCert,
    roster: &[DeviceCert],
    revocations: &RevocationList,
    now: SystemTime,
    limits: &PeerLimits,
) -> Result<BoundedStatePeer, BoundedPeerError> {
    let decoded =
        durable::DecodedDurable::decode(bytes, limits.durable, limits.exact).map_err(|error| {
            match error {
                durable::DurableDecodeError::Resource(error) => BoundedPeerError::Decode(error),
                durable::DurableDecodeError::UnsupportedVersion => {
                    invalid(BoundedPeerInvalid::Version)
                }
                durable::DurableDecodeError::Envelope => invalid(BoundedPeerInvalid::Envelope),
                durable::DurableDecodeError::Pending(_) => invalid(BoundedPeerInvalid::Pending),
            }
        })?;
    let durable::DecodedDurable {
        wire,
        exact,
        version,
        sealed,
    } = decoded;
    let stored_room = wire.base.room;
    let stored_authority = wire.base.authority;
    let room_bytes = wire.base.room_bytes;
    let documents = wire.base.documents;
    let receipts = wire.base.receipts;
    let certificates = wire.base.certificates;
    let stored_revocations = wire.base.revocations;
    let pending = wire.pending;
    let pending_bytes = wire.pending_bytes;
    if stored_room != room || &stored_authority != authority {
        return Err(invalid(BoundedPeerInvalid::Namespace));
    }
    let uuid = uuid::Uuid::parse_str(room).map_err(|_| invalid(BoundedPeerInvalid::Namespace))?;
    let mut metadata = Metadata {
        remaining: limits.metadata_bytes,
    };
    let mut remaining_document_bytes = limits.normalized_bytes;
    let mut batch = NativeBatch::continuing(&mut remaining_document_bytes);
    let mut buffer = [0; 36];
    let room = metadata.text(uuid.hyphenated().encode_lower(&mut buffer))?;
    let certificates =
        roster::BoundedRoster::restore(&certificates, roster, authority, now_ns(now))
            .map_err(roster_error)?;
    let revocations = revocations::BoundedRevocations::restore(
        &stored_revocations,
        revocations,
        &certificates,
        authority.device_public_key,
    )
    .map_err(|error| revocation_error(&error))?;
    let room_doc = room_bytes
        .as_deref()
        .map(|bytes| batch.load(bytes))
        .transpose()?;
    let mut admitted_documents = BTreeMap::new();
    for document in documents {
        let read = room_doc
            .as_ref()
            .map(crate::resources::native::NativeDocument::read);
        governed_snapshot_matches(
            read.as_ref(),
            &document.route.app,
            &document.route.record,
            document.covered_snapshot.as_deref(),
            &BoundedCoverage {
                limits: limits.policy,
            },
        )
        .map_err(|error| match error {
            CoverageMatchError::Capacity => BoundedPeerError::Capacity,
            CoverageMatchError::Decode(error) => BoundedPeerError::Decode(error),
            CoverageMatchError::NoMatch => invalid(BoundedPeerInvalid::Coverage),
        })?;
        metadata.watermarks(document.watermarks.len())?;
        let mut channel = ChannelState::default();
        for (key, sequence) in document.watermarks {
            channel
                .last_seq_per_author
                .insert(Ed25519PublicKey::from_bytes(key), sequence);
        }
        let doc = batch.load(&document.bytes)?;
        metadata.tree::<Route, PeerDocument<BoundedNative>>()?;
        admitted_documents.insert(
            document.route,
            PeerDocument {
                doc,
                channel,
                encryption_required: document.encryption_required,
                covered: document.covered,
                covered_snapshot: document
                    .covered_snapshot
                    .map(|original| NativeCheckpoint { original }),
                sealed: super::SealedLog::default(),
            },
        );
    }
    let mut core = PeerCore::<BoundedNative> {
        room,
        authority: stored_authority,
        certificates,
        revocations,
        receipts,
        room_doc,
        documents: admitted_documents,
        pending,
        pending_bytes,
        checkpoint_times: BTreeMap::new(),
        exact_admissions: exact,
        admission_v5: version >= 5,
        native_cut_publisher: None,
        _retention: (),
    };
    let media_policy = if let Some(doc) = &core.room_doc {
        let read = doc.read();
        let policy = media::read::parse(
            &read,
            &BoundedDecode {
                limits: limits.policy,
            },
        )
        .map_err(policy_error)?;
        media::validate_policy_transition(
            None,
            policy.as_ref().map(PolicyValue::policy),
            media::TransitionMode::Restore,
        )
        .map_err(|_| invalid(BoundedPeerInvalid::Media))?;
        if let Some(policy) = &policy {
            let policy = policy.policy();
            let json = JsonLimits {
                allocation_bytes: limits.governance_json.allocation_bytes,
                collection_entries: limits.governance_json.collection_entries,
            };
            if doc.has_missing_dependencies()
                || policy.authorization_basis
                    != media::basis::bounded(
                        &read,
                        &[],
                        media::basis::BasisLimits {
                            collection_bytes: limits.governance_collection_bytes,
                            json,
                        },
                    )
                    .map_err(policy_error)?
            {
                return Err(invalid(BoundedPeerInvalid::Media));
            }
            let imported = governed::bounded(
                &read,
                &core.certificates,
                &core.revocations,
                &core.authority,
                now,
                governed::ImportLimits {
                    collection_bytes: limits.governance_collection_bytes,
                    certificate_bytes: limits.governance_collection_bytes,
                    json,
                },
            )
            .map_err(|error| governance_error(&error))?;
            core.certificates = imported.certificates;
            core.revocations = imported.revocations;
            if core
                .revocations
                .is_revoked(&core.authority.device_public_key)
            {
                return Err(invalid(BoundedPeerInvalid::AuthorityRevoked));
            }
            // Shared partial-binding semantics preserve data restore when an
            // exact plaintext Chat has not arrived yet.
            super::media::bindings(policy, false, |app, record| {
                super::media::native_binding(&core, policy, app, record)
            })
            .map_err(|error| match error {
                super::media::ReadinessError::Source(error) => policy_error(error),
                _ => invalid(BoundedPeerInvalid::Media),
            })?;
        }
        policy
    } else {
        None
    };
    if let Some(exact) = &core.exact_admissions {
        for (key, _) in exact.iter() {
            let floor = core
                .documents
                .iter()
                .find(|(route, _)| route.app == key.app && route.record == key.record)
                .and_then(|(_, document)| {
                    document
                        .channel
                        .last_seq_per_author
                        .get(&Ed25519PublicKey::from_bytes(key.author))
                })
                .copied();
            if key.room != core.room || floor.is_none_or(|floor| key.sequence > floor) {
                return Err(invalid(BoundedPeerInvalid::ExactFloor));
            }
        }
    }
    for (route, ops) in sealed {
        super::super::sealed_history::restore(&mut core, &route, ops)
            .map_err(|_| invalid(BoundedPeerInvalid::Sealed))?;
    }
    let media = super::media::cache(&core, media_policy, limits)?;
    Ok(BoundedStatePeer::from_core(core, media, *limits))
}
/// Bytes left for restored metadata, from `metadata_bytes`.
struct Metadata {
    remaining: usize,
}
impl Metadata {
    fn debit(&mut self, bytes: Option<usize>) -> Result<(), BoundedPeerError> {
        self.remaining = bytes
            .and_then(|bytes| self.remaining.checked_sub(bytes))
            .ok_or(BoundedPeerError::Capacity)?;
        Ok(())
    }
    fn text(&mut self, value: &str) -> Result<String, BoundedPeerError> {
        self.debit(Some(value.len()))?;
        Ok(value.to_owned())
    }
    fn tree<K, V>(&mut self) -> Result<(), BoundedPeerError> {
        // An estimate of a BTreeMap node's share per insertion.
        self.debit(
            size_of::<K>()
                .checked_add(size_of::<V>())
                .and_then(|n| n.checked_mul(33))
                .and_then(|n| n.checked_add(768)),
        )
    }
    fn watermarks(&mut self, count: usize) -> Result<(), BoundedPeerError> {
        self.debit(count.checked_mul(size_of::<(Ed25519PublicKey, u64)>().saturating_mul(2)))
    }
}
#[cfg(test)]
pub(in crate::state) mod tests;
