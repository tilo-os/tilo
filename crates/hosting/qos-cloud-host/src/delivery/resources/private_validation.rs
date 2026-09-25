//! Retained private history and exact original Store binding checks.
use super::{CodecBudget, HeldCloudState};
use qos_peer_state::{
    PeerWork,
    owner_commit::{AgentRuntimeFact, PrivateAgentState},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrivateStateFailure {
    Capacity,
    Denied,
}
impl HeldCloudState {
    pub(crate) fn validate_private(&self, work: &PeerWork<'_>) -> Result<(), PrivateStateFailure> {
        let state = self.state();
        // Exact State::private_schema_version match without constructing anyhow.
        let selected = match (state.version, &state.delivery) {
            (4 | 5, None) => state.version,
            (6, Some(delivery)) if matches!(delivery.prior_private_schema, 4 | 5) => {
                delivery.prior_private_schema
            }
            _ => return Err(PrivateStateFailure::Denied),
        };
        let ledger = match (selected, &state.private_agent_ledger) {
            (4, None) => return Ok(()),
            (5, Some(ledger)) => ledger,
            _ => return Err(PrivateStateFailure::Denied),
        };
        ledger
            .validate_paid(&state.room, state.generation, work)
            .map_err(|failure| {
                if failure.is_capacity() {
                    PrivateStateFailure::Capacity
                } else {
                    PrivateStateFailure::Denied
                }
            })?;
        if let PrivateAgentState::V2(ledger) = ledger {
            // Legacy open requires bound even when the selected V2 ledger has
            // no facts. V1 deliberately gains no new retained-owner restriction.
            let bound = state.bound.as_ref().ok_or(PrivateStateFailure::Denied)?;
            for receipt in ledger.facts.values() {
                if let AgentRuntimeFact::Admitted { intent, .. } = &receipt.change.fact {
                    if intent.scope.machine != state.machine_id
                        || intent.scope.host != state.room
                        || intent.scope.caller.user_public_key != bound.root
                    {
                        return Err(PrivateStateFailure::Denied);
                    }
                    // room_workspace_id now formats its UUID on stack. u64's
                    // specialized ToString allocates at most20 decimal bytes.
                    // String is declared after, and dies before, its reservation.
                    let budget = CodecBudget::peer(work);
                    let _format = budget
                        .reserve(20)
                        .map_err(|_| PrivateStateFailure::Capacity)?;
                    let expected = qos_relay::room_workspace_id(&state.room)
                        .map_err(|_| PrivateStateFailure::Denied)?
                        .to_string();
                    if intent.route.app_peer != expected {
                        return Err(PrivateStateFailure::Denied);
                    }
                }
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests;
