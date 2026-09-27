//! Ordering fence only: a current authenticated native poll must validate a
//! restored cursor before an uncertain ACK is retried. Empty poll is not a cut.
#[derive(Default)]
pub(crate) struct CursorRecovery {
    epoch: u64,
    validated: bool,
}
impl CursorRecovery {
    pub fn reset(&mut self) -> Result<(), String> {
        self.epoch = self
            .epoch
            .checked_add(1)
            .ok_or("cursor recovery generation exhausted")?;
        self.validated = false;
        Ok(())
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn accept_poll(&mut self, epoch: u64) -> Result<(), String> {
        if epoch != self.epoch {
            return Err("stale cursor validation".into());
        }
        self.validated = true;
        Ok(())
    }
    pub fn ack_allowed(&self) -> bool {
        self.validated
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restored_ack_waits_for_current_poll_and_replacement_fences_old_success() {
        let mut gate = CursorRecovery::default();
        gate.reset().unwrap();
        let first = gate.epoch();
        assert!(
            !gate.ack_allowed(),
            "restored cursor is not current native acceptance"
        );
        gate.accept_poll(first).unwrap();
        assert!(gate.ack_allowed());
        gate.reset().unwrap();
        assert!(!gate.ack_allowed());
        assert!(gate.accept_poll(first).is_err());
        assert!(!gate.ack_allowed());
        gate.accept_poll(gate.epoch()).unwrap();
        assert!(gate.ack_allowed());
    }
}
