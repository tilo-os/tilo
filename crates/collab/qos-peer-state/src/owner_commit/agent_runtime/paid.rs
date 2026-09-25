//! Closed resource custody for the unchanged retained private validator.
use super::*;
mod cost;
use automerge::resource_limits::NativeReservation;

#[derive(Debug)]
pub struct PrivateValidationFailure {
    reason: Failure,
}
#[derive(Debug)]
enum Failure {
    Capacity,
    Invalid {
        message: String,
        _reservation: NativeReservation,
    },
}
impl PrivateValidationFailure {
    pub fn is_capacity(&self) -> bool {
        matches!(self.reason, Failure::Capacity)
    }
    pub fn diagnostic(&self) -> Option<&str> {
        match &self.reason {
            Failure::Capacity => None,
            Failure::Invalid { message, .. } => Some(message),
        }
    }
    fn capacity() -> Self {
        Self {
            reason: Failure::Capacity,
        }
    }
}
impl PrivateAgentState {
    pub fn validate_paid(
        &self,
        room: &str,
        generation: u64,
        work: &crate::PeerWork<'_>,
    ) -> Result<(), PrivateValidationFailure> {
        let bytes =
            cost::validation(self, room).map_err(|_| PrivateValidationFailure::capacity())?;
        let reservation = work
            .reserve_output(bytes)
            .map_err(|_| PrivateValidationFailure::capacity())?;
        match self.validate(room, generation) {
            Ok(()) => Ok(()),
            Err(message) => Err(PrivateValidationFailure {
                reason: Failure::Invalid {
                    message,
                    _reservation: reservation,
                },
            }),
        }
    }
}
#[cfg(test)]
mod tests;
