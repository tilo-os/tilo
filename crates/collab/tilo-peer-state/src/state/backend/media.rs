//! One ordering of current media prerequisites; no authority constructor.
use super::{BoundedNative, PeerCore, Route, StatePeer, err, room};
use crate::room::media::RoomMediaV1;
use crate::room::media::read::{BoundedPolicy, PolicyValue};
use crate::state::bounded::{BoundedPeerError, PeerLimits};

/// Ready owns the original guarded decode, never a copied policy credential.
pub(in crate::state) enum Cache {
    Disabled,
    Unavailable,
    Ready(BoundedPolicy),
}
pub(in crate::state) fn native_binding(
    core: &PeerCore<BoundedNative>,
    policy: &RoomMediaV1,
    app: &str,
    record: &str,
) -> Result<bool, room::media::read::Error> {
    #[cfg(test)]
    BINDINGS.with(|count| count.set(count.get().saturating_add(1)));
    match core
        .documents
        .iter()
        .find(|(route, _)| route.app == app && route.record == record)
    {
        Some((route, document)) if !document.encryption_required => {
            room::media::binding::validate_native(
                policy,
                &route.app,
                &route.record,
                &document.doc,
            )?;
            Ok(true)
        }
        _ => Ok(false),
    }
}
pub(in crate::state) fn cache(
    core: &PeerCore<BoundedNative>,
    policy: Option<BoundedPolicy>,
    limits: &PeerLimits,
) -> Result<Cache, BoundedPeerError> {
    let Some(policy) = policy else {
        return Ok(Cache::Disabled);
    };
    let Some(room) = &core.room_doc else {
        return Ok(Cache::Unavailable);
    };
    let read = room.read();
    let result = ready(
        policy.policy(),
        |app, record| native_binding(core, policy.policy(), app, record),
        || {
            room::media::basis::bounded_entries(
                &read,
                core.revocations.iter(),
                room::media::basis::BasisLimits {
                    collection_bytes: limits.governance_collection_bytes,
                    json: crate::resources::json::JsonLimits {
                        allocation_bytes: limits.governance_json.allocation_bytes,
                        collection_entries: limits.governance_json.collection_entries,
                    },
                },
            )
        },
    );
    match result {
        Ok(()) => Ok(Cache::Ready(policy)),
        Err(ReadinessError::Source(room::media::read::Error::Capacity)) => {
            Err(BoundedPeerError::Capacity)
        }
        Err(ReadinessError::Source(room::media::read::Error::Decode(error))) => {
            Err(BoundedPeerError::Decode(error))
        }
        Err(_) => Ok(Cache::Unavailable),
    }
}
#[cfg(test)]
pub(in crate::state) fn binding_checks() -> usize {
    BINDINGS.with(std::cell::Cell::get)
}
#[cfg(test)]
thread_local! { static BINDINGS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[derive(Debug, PartialEq, Eq)]
pub(in crate::state) enum ReadinessError<E> {
    Source(E),
    Incomplete,
    Unreconciled,
}
impl ReadinessError<String> {
    pub(in crate::state) fn legacy(self) -> String {
        match self {
            Self::Source(error) => error,
            Self::Incomplete => "media binding requires exact admitted Chat checkpoint".into(),
            Self::Unreconciled => {
                "media security basis requires governed revocation reconciliation".into()
            }
        }
    }
}
pub(in crate::state) fn bindings<E>(
    policy: &RoomMediaV1,
    require_complete: bool,
    mut check: impl FnMut(&str, &str) -> Result<bool, E>,
) -> Result<(), ReadinessError<E>> {
    for call in policy
        .calls
        .values()
        .filter(|call| call.phase == room::media::CallPhaseV1::Active)
    {
        let present =
            check(&call.binding.app_id, &call.binding.record_id).map_err(ReadinessError::Source)?;
        if !present && require_complete {
            return Err(ReadinessError::Incomplete);
        }
    }
    Ok(())
}
pub(in crate::state) fn ready<E>(
    policy: &RoomMediaV1,
    check: impl FnMut(&str, &str) -> Result<bool, E>,
    basis: impl FnOnce() -> Result<[u8; 32], E>,
) -> Result<(), ReadinessError<E>> {
    bindings(policy, true, check)?;
    if basis().map_err(ReadinessError::Source)? != policy.authorization_basis {
        return Err(ReadinessError::Unreconciled);
    }
    Ok(())
}
pub(in crate::state) fn legacy_binding(
    peer: &StatePeer,
    policy: &RoomMediaV1,
    app: &str,
    record: &str,
) -> Result<bool, String> {
    let route = Route {
        app: app.into(),
        record: record.into(),
    };
    match peer.core.documents.get(&route) {
        Some(document) if !document.encryption_required => {
            room::media::validate_bound_chat_change(
                policy,
                &route.app,
                &route.record,
                &*document.doc,
            )
            .map_err(err)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}
#[cfg(test)]
mod tests;
