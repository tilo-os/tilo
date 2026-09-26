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
        let len = self.members.len();
        let next = idx.saturating_add(1);
        self.members.get(if next < len { next } else { 0 }).copied()
    }

    /// The id Shift+Tab should move to from `id`, wrapping past the start back
    /// to the end. Returns `None` only when the trap is empty.
    pub fn prev_from(&self, id: u64) -> Option<u64> {
        if self.members.is_empty() {
            return None;
        }
        let idx = self.members.iter().position(|&x| x == id).unwrap_or(0);
        let len = self.members.len();
        let prev = if idx == 0 {
            len.saturating_sub(1)
        } else {
            idx.saturating_sub(1)
        };
        self.members.get(prev).copied()
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
    fn tab_and_shift_tab_wrap_inside_the_trap() {
        let t = FocusTrap::new(vec![1, 2, 3]);
        // (case, got, want)
        let cases = [
            ("next inside", t.next_from(1), Some(2)),
            // Tab from last member returns to first.
            ("next wraps at the end", t.next_from(3), Some(1)),
            ("prev inside", t.prev_from(2), Some(1)),
            // Shift+Tab from first wraps to last.
            ("prev wraps at the start", t.prev_from(1), Some(3)),
            // An unknown id starts from the first member.
            (
                "unknown id",
                FocusTrap::new(vec![10, 20]).next_from(999),
                Some(20),
            ),
        ];
        for (case, got, want) in cases {
            assert_eq!(got, want, "case {case}");
        }

        // An empty trap yields None rather than panicking.
        let t = FocusTrap::new(vec![]);
        assert!(t.is_empty());
        assert_eq!(t.next_from(1), None);
        assert_eq!(t.prev_from(1), None);
    }
}
