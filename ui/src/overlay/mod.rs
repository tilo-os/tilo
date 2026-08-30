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
    next_id: u64,
    entries: Vec<(OverlayId, OverlayEntry)>,
}

impl OverlayStack {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            entries: Vec::new(),
        }
    }
    pub fn push_modal(&mut self, m: Modal) -> OverlayId {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push((id, OverlayEntry::Modal(m)));
        id
    }
    pub fn push_popover(&mut self, p: Popover) -> OverlayId {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push((id, OverlayEntry::Popover(p)));
        id
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
