/// A ring of focusable ids that keyboard focus cycles within while a modal is
/// up. `next_from`/`prev_from` wrap at the ends so Tab from the last member
/// returns to the first (and Shift+Tab from the first wraps to the last),
/// keeping focus inside the modal rather than escaping to the content behind.
#[derive(Clone, Debug)]
pub struct FocusTrap {
    members: Vec<u64>,
}

impl FocusTrap {
    pub fn new(members: Vec<u64>) -> Self {
        Self { members }
    }

    /// The id Tab should move to from `id`, wrapping past the end back to the
    /// start. Returns `None` only when the trap is empty (nothing to focus).
    pub fn next_from(&self, id: u64) -> Option<u64> {
        if self.members.is_empty() {
            return None;
        }
        let idx = self.members.iter().position(|&x| x == id).unwrap_or(0);
        Some(self.members[(idx + 1) % self.members.len()])
    }

    /// The id Shift+Tab should move to from `id`, wrapping past the start back
    /// to the end. Returns `None` only when the trap is empty.
    pub fn prev_from(&self, id: u64) -> Option<u64> {
        if self.members.is_empty() {
            return None;
        }
        let idx = self.members.iter().position(|&x| x == id).unwrap_or(0);
        let len = self.members.len();
        Some(self.members[(idx + len - 1) % len])
    }

    pub fn contains(&self, id: u64) -> bool {
        self.members.contains(&id)
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_wraps_at_the_end() {
        let t = FocusTrap::new(vec![1, 2, 3]);
        assert_eq!(t.next_from(1), Some(2));
        assert_eq!(
            t.next_from(3),
            Some(1),
            "Tab from last member returns to first"
        );
    }

    #[test]
    fn prev_wraps_at_the_start() {
        let t = FocusTrap::new(vec![1, 2, 3]);
        assert_eq!(t.prev_from(2), Some(1));
        assert_eq!(
            t.prev_from(1),
            Some(3),
            "Shift+Tab from first wraps to last"
        );
    }

    #[test]
    fn unknown_id_starts_from_the_first_member() {
        let t = FocusTrap::new(vec![10, 20]);
        assert_eq!(t.next_from(999), Some(20));
    }

    #[test]
    fn empty_trap_yields_none_not_panic() {
        let t = FocusTrap::new(vec![]);
        assert!(t.is_empty());
        assert_eq!(t.next_from(1), None);
        assert_eq!(t.prev_from(1), None);
    }
}
