//! Closed composition of authenticated durable input into retained peer storage.
use super::super::bounded::*;
use super::*;
use crate::coverage::read::{BoundedCoverage, WireVersionError, snapshot_wire_version};
use crate::resources::json::JsonLimits;
use crate::room::media::{
    self,
    read::{BoundedDecode, PolicyValue},
};
use crate::room::read::TurnCopy;
use automerge::resource_limits::NativeResourceError;

// This is retained with the Core, after all moved original DTO fields. The
// private constructor below is the only place their custody can be transferred.
pub(in crate::state) struct CoreRetention {
    _durable: NativeReservation,
    _metadata: NativeReservation,
}
enum Documents {
    V2(std::vec::IntoIter<DurableDocumentV2>),
    Current(std::vec::IntoIter<DurableDocument>),
}
impl Iterator for Documents {
    type Item = DurableDocument;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Current(values) => values.next(),
            Self::V2(values) => values.next().map(|value| DurableDocument {
                route: value.route,
                bytes: value.bytes,
                watermarks: value.watermarks,
                wire_version: None,
                encryption_required: value.encryption_required,
                covered: value.covered,
                covered_snapshot: value.covered_snapshot,
            }),
        }
    }
}
fn invalid(value: BoundedPeerInvalid) -> BoundedPeerError {
    BoundedPeerError::Invalid(value)
}
fn resource(value: NativeResourceError) -> BoundedPeerError {
    BoundedPeerError::Resource(value)
}
fn policy_error(value: media::read::Error) -> BoundedPeerError {
    match value {
        media::read::Error::Resource(error) => resource(error),
        media::read::Error::Decode(error) => BoundedPeerError::Decode(error),
        media::read::Error::Policy(_) => invalid(BoundedPeerInvalid::Media),
    }
}
fn roster_error(value: roster::RosterError) -> BoundedPeerError {
    match value {
        roster::RosterError::Resource(error) => resource(error),
        _ => invalid(BoundedPeerInvalid::Certificate),
    }
}
fn revocation_error(value: revocations::RestoreRevocationsError) -> BoundedPeerError {
    match value {
        revocations::RestoreRevocationsError::Resource(error) => resource(error),
        revocations::RestoreRevocationsError::AuthorityRevoked => {
            invalid(BoundedPeerInvalid::AuthorityRevoked)
        }
        _ => invalid(BoundedPeerInvalid::Revocation),
    }
}
fn governance_error(value: governed::ImportError) -> BoundedPeerError {
    match value {
        governed::ImportError::Resource(error) => resource(error),
        governed::ImportError::Decode(error) => BoundedPeerError::Decode(error),
        _ => invalid(BoundedPeerInvalid::Governance),
    }
}
pub(in crate::state) fn restore(
    bytes: &[u8],
    room: &str,
    authority: &DeviceCert,
    roster: &[DeviceCert],
    revocations: &RevocationList,
    now: SystemTime,
    work: &mut PeerWork<'_>,
) -> Result<BoundedStatePeer, BoundedPeerError> {
    let decoded =
        durable::DecodedDurable::decode(bytes, work.limits.durable, work.limits.exact, work.turn)
            .map_err(|error| match error {
            durable::DurableDecodeError::Resource(error) => BoundedPeerError::Decode(error),
            durable::DurableDecodeError::UnsupportedVersion => invalid(BoundedPeerInvalid::Version),
            durable::DurableDecodeError::Envelope => invalid(BoundedPeerInvalid::Envelope),
            durable::DurableDecodeError::Pending(_) => invalid(BoundedPeerInvalid::Pending),
        })?;
    let durable::DecodedDurable {
        wire,
        exact,
        version,
        retention,
    } = decoded;
    let (
        stored_room,
        stored_authority,
        room_bytes,
        documents,
        receipts,
        certificates,
        stored_revocations,
        pending,
        pending_bytes,
    ) = match wire {
        durable::DurableWire::V2(value) => (
            value.room,
            value.authority,
            value.room_bytes,
            Documents::V2(value.documents.into_iter()),
            value.receipts,
            value.certificates,
            value.revocations,
            VecDeque::new(),
            0,
        ),
        durable::DurableWire::V3(value) => (
            value.room,
            value.authority,
            value.room_bytes,
            Documents::Current(value.documents.into_iter()),
            value.receipts,
            value.certificates,
            value.revocations,
            VecDeque::new(),
            0,
        ),
        durable::DurableWire::V4(value) => (
            value.base.room,
            value.base.authority,
            value.base.room_bytes,
            Documents::Current(value.base.documents.into_iter()),
            value.base.receipts,
            value.base.certificates,
            value.base.revocations,
            value.pending,
            value.pending_bytes,
        ),
    };
    if stored_room != room || &stored_authority != authority {
        return Err(invalid(BoundedPeerInvalid::Namespace));
    }
    let uuid = uuid::Uuid::parse_str(room).map_err(|_| invalid(BoundedPeerInvalid::Namespace))?;
    let mut metadata = Metadata {
        remaining: work.limits.metadata_bytes,
        retention: Some(
            work.turn
                .reserve(work.limits.metadata_bytes)
                .map_err(resource)?,
        ),
    };
    let mut buffer = [0; 36];
    let room = metadata.text(uuid.hyphenated().encode_lower(&mut buffer))?;
    let certificates =
        roster::BoundedRoster::restore(&certificates, roster, authority, now_ns(now), work.turn)
            .map_err(roster_error)?;
    let revocations = revocations::BoundedRevocations::restore(
        &stored_revocations,
        revocations,
        &certificates,
        authority.device_public_key,
        work.turn,
    )
    .map_err(revocation_error)?;
    let room_doc = room_bytes
        .as_deref()
        .map(|bytes| work.batch.load(bytes).map_err(resource))
        .transpose()?;
    let mut admitted_documents = BTreeMap::new();
    for document in documents {
        let read = room_doc
            .as_ref()
            .map(|room| room.read(work.turn).map_err(resource))
            .transpose()?;
        let governed = snapshot_wire_version(
            read.as_ref(),
            &document.route.app,
            &document.route.record,
            document.covered_snapshot.as_deref(),
            &BoundedCoverage {
                turn: work.turn,
                limits: work.limits.policy,
            },
        )
        .map_err(|error| match error {
            WireVersionError::Resource(error) => resource(error),
            WireVersionError::Decode(error) => BoundedPeerError::Decode(error),
            _ => invalid(BoundedPeerInvalid::Coverage),
        })?;
        if document
            .wire_version
            .zip(governed)
            .is_some_and(|(stored, governed)| stored != governed)
        {
            return Err(invalid(BoundedPeerInvalid::WireVersion));
        }
        let wire_version = document.wire_version.or(governed);
        metadata.watermarks(document.watermarks.len())?;
        let mut channel = ChannelState::default();
        for (key, sequence) in document.watermarks {
            channel
                .last_seq_per_author
                .insert(Ed25519PublicKey::from_bytes(key), sequence);
        }
        let doc = work.batch.load(&document.bytes).map_err(resource)?;
        metadata.tree::<Route, PeerDocument<BoundedNative>>()?;
        admitted_documents.insert(
            document.route,
            PeerDocument {
                doc,
                channel,
                wire_version,
                encryption_required: document.encryption_required,
                covered: document.covered,
                covered_snapshot: document.covered_snapshot.map(|original| NativeCheckpoint {
                    original,
                    wire_version,
                }),
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
        admission_v5: version == 5,
        native_cut_publisher: None,
        retention: CoreRetention {
            _durable: retention,
            _metadata: metadata
                .retention
                .take()
                .ok_or_else(|| resource(NativeResourceError::Unaccounted))?,
        },
    };
    let mut media_policy = None;
    if let Some(doc) = &core.room_doc {
        let read = doc.read(work.turn).map_err(resource)?;
        let policy = media::read::parse(
            &read,
            &TurnCopy(work.turn),
            &BoundedDecode {
                turn: work.turn,
                limits: work.limits.policy,
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
                allocation_bytes: work.limits.governance_json.allocation_bytes,
                collection_entries: work.limits.governance_json.collection_entries,
            };
            if doc.has_missing_dependencies()
                || policy.authorization_basis
                    != media::basis::bounded(
                        &read,
                        &[],
                        work.turn,
                        media::basis::BasisLimits {
                            collection_bytes: work.limits.governance_collection_bytes,
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
                work.turn,
                governed::ImportLimits {
                    collection_bytes: work.limits.governance_collection_bytes,
                    certificate_bytes: work.limits.governance_collection_bytes,
                    json,
                },
            )
            .map_err(governance_error)?;
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
                super::media::native_binding(&core, policy, app, record, work.turn)
            })
            .map_err(|error| match error {
                super::media::ReadinessError::Source(error) => policy_error(error),
                _ => invalid(BoundedPeerInvalid::Media),
            })?;
        }
        media_policy = policy;
    }
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
    let media = super::media::cache(&core, media_policy, work)?;
    Ok(BoundedStatePeer::from_core(core, media, work))
}
struct Metadata {
    remaining: usize,
    retention: Option<NativeReservation>,
}
impl Metadata {
    fn debit(&mut self, bytes: Option<usize>) -> Result<(), BoundedPeerError> {
        self.remaining = self
            .remaining
            .checked_sub(bytes.ok_or_else(|| resource(NativeResourceError::WorkBytes))?)
            .ok_or_else(|| resource(NativeResourceError::WorkBytes))?;
        Ok(())
    }
    fn text(&mut self, value: &str) -> Result<String, BoundedPeerError> {
        self.debit(Some(value.len()))?;
        let mut out = String::new();
        out.try_reserve_exact(value.len())
            .map_err(|_| resource(NativeResourceError::WorkBytes))?;
        if out.capacity() > value.len() {
            return Err(resource(NativeResourceError::Unaccounted));
        }
        out.push_str(value);
        Ok(out)
    }
    fn tree<K, V>(&mut self) -> Result<(), BoundedPeerError> {
        // Same pinned11-slot/12-child std1.97.1 bound as the reviewed roster.
        if align_of::<K>() > 16 || align_of::<V>() > 16 {
            return Err(resource(NativeResourceError::Unaccounted));
        }
        self.debit(
            size_of::<K>()
                .checked_add(size_of::<V>())
                .and_then(|n| n.checked_mul(33))
                .and_then(|n| n.checked_add(768)),
        )
    }
    fn watermarks(&mut self, count: usize) -> Result<(), BoundedPeerError> {
        if count == 0 {
            return Ok(());
        }
        // Fresh std/hashbrown0.17.1 HashMap<(fixed key),u64>, no deletions:
        //4/8/16 initial buckets then7/8 load. Two maximum allocations cover
        // old/new rehash overlap; no uncharged Vec/collect is constructed.
        let capacity = || resource(NativeResourceError::WorkBytes);
        let buckets = if count < 4 {
            4
        } else if count < 8 {
            8
        } else if count < 15 {
            16
        } else {
            (count.checked_mul(8).ok_or_else(capacity)? / 7)
                .checked_next_power_of_two()
                .ok_or_else(capacity)?
        };
        type Entry = (Ed25519PublicKey, u64);
        let alignment = align_of::<Entry>().max(16);
        let data = size_of::<Entry>()
            .checked_mul(buckets)
            .ok_or_else(capacity)?;
        let padded = data.checked_add(alignment - 1).ok_or_else(capacity)? & !(alignment - 1);
        self.debit(
            padded
                .checked_add(buckets)
                .and_then(|n| n.checked_add(16))
                .and_then(|n| n.checked_mul(2)),
        )
    }
}
#[cfg(test)]
pub(in crate::state) mod tests;
