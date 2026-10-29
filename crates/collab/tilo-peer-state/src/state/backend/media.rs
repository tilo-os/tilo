//! One ordering of current media prerequisites; no authority constructor.
use super::{Route, StatePeer, err, room};
use crate::room::media::RoomMediaV1;

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
