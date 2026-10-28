//! Ownership guard only. No state grants source rights or proves native stop.
use super::AtomicSavePreparation;

pub struct AtomicSaveReservation<T> {
    state: State<T>,
}
enum State<T> {
    Unknown,
    Prepared(T),
    Claimed,
    Released,
    CleanupSubmitted,
}
impl<T> Default for AtomicSaveReservation<T> {
    fn default() -> Self {
        Self {
            state: State::Unknown,
        }
    }
}
impl<T> AtomicSaveReservation<T> {
    /// Store the actual returned owner before any fallible caller validation.
    pub fn capture(&mut self, outcome: AtomicSavePreparation<T>) -> Result<(), String> {
        match outcome {
            AtomicSavePreparation::Prepared(owner) => {
                self.state = State::Prepared(owner);
                Ok(())
            }
            AtomicSavePreparation::Refused(error) => {
                self.state = State::Released;
                Err(error)
            }
            AtomicSavePreparation::Unknown(error) => {
                self.state = State::Unknown;
                Err(error)
            }
        }
    }
    /// # Errors
    /// Refuses reservations that are not currently prepared.
    pub fn prepared(&self) -> Result<&T, String> {
        match &self.state {
            State::Prepared(owner) => Ok(owner),
            _ => Err("AtomicSave reservation unavailable".into()),
        }
    }
    /// Failure retains the actual reservation; cancellation remains possible.
    pub fn validate(&self, check: impl FnOnce(&T) -> Result<(), String>) -> Result<(), String> {
        check(self.prepared()?)
    }
    /// # Errors
    /// Refuses reservations that are not currently prepared.
    pub fn claim(&mut self) -> Result<T, String>
    where
        T: Clone,
    {
        let owner = self.prepared()?.clone();
        self.state = State::Claimed;
        Ok(owner)
    }
    /// Only actual cancel ACK releases an unsubmitted reservation. Failed or
    /// poisoned cancellation keeps its owner; unknown/claimed effects refuse.
    /// # Errors
    /// Propagates cancellation failures or refuses unresolved reservation custody.
    pub fn release_with(
        &mut self,
        cancel: impl FnOnce(&T) -> Result<(), String>,
    ) -> Result<(), String> {
        match &self.state {
            State::Prepared(owner) => {
                cancel(owner)?;
                self.state = State::Released;
                Ok(())
            }
            State::Released => Ok(()),
            _ => Err("AtomicSave reservation custody unresolved".into()),
        }
    }
    /// Called after an actual creator cleanup job is returned. This transition
    /// cannot prove Finished, release a slot, or authorize any namespace effect.
    /// # Errors
    /// Refuses cleanup submission until the reservation has been released.
    pub fn cleanup_submitted(&mut self) -> Result<(), String> {
        if !matches!(self.state, State::Released) {
            return Err("AtomicSave cleanup not released".into());
        }
        self.state = State::CleanupSubmitted;
        Ok(())
    }
}
