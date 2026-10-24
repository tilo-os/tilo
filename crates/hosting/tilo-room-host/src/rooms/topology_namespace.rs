//! Namespace registration, a member's own business in the room.
use super::{CollaborationError, GovernanceCompletion, Rooms, system_time};
use tilo_user_identity::DeviceCert;

impl Rooms {
    /// Durably register an already admitted writer's own topology namespace.
    /// The caller is the device the room certified, as the relay session it
    /// asks on (`actions::member`) authenticated it; membership
    /// and current workspace Write are checked again under the room lock.
    ///
    /// # Errors
    /// Refuses invalid/revoked certificates, stale membership, denied Write,
    /// conflicting namespaces and failed persistence or signed publication.
    pub async fn claim_topology_namespace(
        &self,
        room: &str,
        caller: &DeviceCert,
        salt: u16,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        tilo_user_identity::verify_device_cert_at(caller, now)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        if self.revocations().is_revoked(&caller.device_public_key) {
            return Err(CollaborationError::new("device revoked"));
        }
        let state = self.room(room).await?;
        let mut room = state.lock().await;
        let mut candidate = room.peer().clone();
        let changed = candidate.governance_mut().claim_topology_namespace(
            caller,
            salt,
            &self.inner.host.key,
            system_time(now),
        )?;
        let collaboration = room.collaboration().clone();
        let generation = if changed {
            room.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
                Ok(collaboration)
            })?
        } else {
            room.record.generation
        };
        self.settle(&mut room).await?;
        Ok(GovernanceCompletion { generation })
    }
}

impl Rooms {
    /// Count `room` refusing a member's windows that build on `missing`
    /// changes it no longer has, and say so: the member makes them again on
    /// the room's current base once that reaches it.
    pub(super) fn unbuildable_windows(&self, room: &str, missing: usize) -> String {
        let count = *self
            .inner
            .unbuildable_windows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(room.to_owned())
            .and_modify(|count| *count = count.saturating_add(1))
            .or_insert(1);
        format!(
            "member's windows build on {missing} changes the room no longer has \
             (refused {count} times in this room)"
        )
    }
}

/// How many changes `changes`, a member's windows, build on that neither
/// the room's document `room` holds nor `changes` bring: the history the
/// member had before the governor started the room over from a base.
/// Taking them in would leave them waiting for that history for good.
///
/// # Errors
/// A change does not decode.
pub(super) fn missing_history(
    room: Option<&automerge::AutoCommit>,
    changes: &[Vec<u8>],
) -> Result<usize, String> {
    use automerge::ReadDoc as _;
    let changes = changes
        .iter()
        .map(|bytes| automerge::Change::from_bytes(bytes.clone()).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let brought: std::collections::HashSet<_> =
        changes.iter().map(automerge::Change::hash).collect();
    Ok(changes
        .iter()
        .flat_map(automerge::Change::deps)
        .filter(|dep| {
            !brought.contains(*dep) && room.is_none_or(|doc| doc.get_change_by_hash(dep).is_none())
        })
        .count())
}

#[cfg(test)]
mod tests {
    use automerge::transaction::Transactable as _;

    /// Windows built on the room's own history are taken in; those built on
    /// a history the room started over from are found out, not left waiting.
    #[test]
    fn windows_built_on_a_history_the_room_no_longer_has_are_found_out() {
        let mut old = automerge::AutoCommit::new();
        old.put(automerge::ROOT, "name", "Projects").unwrap();
        old.commit();
        let mut member = old.fork();
        let heads = member.get_heads();
        member.put(automerge::ROOT, "contents", "windows").unwrap();
        member.commit();
        let windows: Vec<Vec<u8>> = member
            .get_changes(&heads)
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        assert_eq!(super::missing_history(Some(&old), &windows), Ok(0));

        let mut based = automerge::AutoCommit::new();
        based.put(automerge::ROOT, "name", "Projects").unwrap();
        based.commit();
        assert_eq!(super::missing_history(Some(&based), &windows), Ok(1));
    }
}
