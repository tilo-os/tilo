//! Exact retained admission facts; no authority or mutation surface.
use super::PeerRead;
impl PeerRead<'_> {
    /// Whether this actual restored peer selected the V5 admission format.
    pub fn admission_v5_selected(&self) -> bool {
        self.peer.core.admission_v5
    }
    /// Membership in the actual retained exact admission index. This is data,
    /// not a receipt, authorization or evidence of an outer durable barrier.
    pub fn contains_admitted_operation(&self, operation: &[u8; 32]) -> bool {
        self.peer
            .core
            .exact_admissions
            .as_ref()
            .is_some_and(|index| index.iter().any(|(_, admitted)| admitted == operation))
    }
}
#[cfg(test)]
mod tests;
