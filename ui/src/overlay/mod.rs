pub mod focus_trap;
pub mod modal;
pub mod popover;
pub mod scrim;

pub use focus_trap::FocusTrap;
pub use modal::Modal;
pub use popover::Popover;
pub use scrim::Scrim;

pub type OverlayId = u64;

#[derive(Debug)]
pub enum OverlayEntry {
    Modal(Modal),
    Popover(Popover),
}

pub struct OverlayStack {
    /// `None` once every possible ID has been issued. A dismissed ID must
    /// never be recycled: retained callbacks can still carry it and would
    /// otherwise dismiss a different, newer overlay.
    next_id: Option<u64>,
    entries: Vec<(OverlayId, OverlayEntry)>,
}

impl OverlayStack {
    pub fn new() -> Self {
        Self {
            next_id: Some(1),
            entries: Vec::new(),
        }
    }
    /// Push a modal and return its unique ID, or `None` after the ID namespace
    /// is exhausted. On exhaustion neither the entry list nor an existing ID
    /// changes.
    pub fn push_modal(&mut self, m: Modal) -> Option<OverlayId> {
        let id = self.take_next_id()?;
        self.entries.push((id, OverlayEntry::Modal(m)));
        Some(id)
    }
    /// Push a popover and return its unique ID, or `None` after exhaustion.
    pub fn push_popover(&mut self, p: Popover) -> Option<OverlayId> {
        let id = self.take_next_id()?;
        self.entries.push((id, OverlayEntry::Popover(p)));
        Some(id)
    }

    fn take_next_id(&mut self) -> Option<OverlayId> {
        let id = self.next_id?;
        self.next_id = id.checked_add(1);
        Some(id)
    }
    pub fn dismiss(&mut self, id: OverlayId) {
        self.entries.retain(|(x, _)| *x != id);
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn top_id(&self) -> Option<OverlayId> {
        self.entries.last().map(|(id, _)| *id)
    }
    pub fn iter(&self) -> impl Iterator<Item = &(OverlayId, OverlayEntry)> {
        self.entries.iter()
    }
}

impl Default for OverlayStack {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_id_exhaustion_never_reuses_or_partially_pushes() {
        let mut stack = OverlayStack::new();
        stack.next_id = Some(u64::MAX);

        let last = stack
            .push_modal(Modal::new("last"))
            .expect("last ID is usable");
        assert_eq!(last, u64::MAX);
        assert_eq!(stack.len(), 1);
        assert_eq!(stack.top_id(), Some(u64::MAX));

        assert!(
            stack
                .push_popover(Popover::new("after", vello::kurbo::Point::ZERO))
                .is_none()
        );
        assert_eq!(stack.len(), 1);
        assert_eq!(stack.top_id(), Some(u64::MAX));
    }
}
