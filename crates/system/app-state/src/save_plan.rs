//! When a document's save appends the changes since its last one and when it rewrites the
//! document whole.
//!
//! Appending keeps a save cheap and keeps a crash from losing more than the last append. Once
//! what was appended outgrows the document it is appended to, the next save rewrites the whole
//! document instead, so a long-lived document's file never grows with every change. Both the
//! per-app documents and the device's settings documents save this way, so both ask one plan.

/// What a save of a document writes: the changes since its last save, or the document whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Save {
    Append,
    Full,
}

/// A document's saves as they stand: the size of its last whole save, and the bytes appended
/// since. A plain value with no interior state, so any thread that keeps a document's writer can
/// keep one beside it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SavePlan {
    whole: usize,
    appended: usize,
}

impl SavePlan {
    /// Appends may reach this multiple of the whole save before the next save rewrites the
    /// document.
    pub const COMPACT_AFTER: usize = 2;

    /// No rewrite is worth it below this many bytes: a small document rewrites in microseconds,
    /// and the bookkeeping that tracks appends would dominate the save.
    pub const MIN_COMPACT_BYTES: usize = 64 * 1024;

    /// A document last saved whole at `whole` bytes, with `appended` bytes appended since.
    #[must_use]
    pub fn new(whole: usize, appended: usize) -> Self {
        Self { whole, appended }
    }

    /// What the next save writes.
    #[must_use]
    pub fn next(&self) -> Save {
        if self.whole == 0
            || self.appended
                >= self
                    .whole
                    .saturating_mul(Self::COMPACT_AFTER)
                    .max(Self::MIN_COMPACT_BYTES)
        {
            Save::Full
        } else {
            Save::Append
        }
    }

    /// A whole save of `bytes`: appends start over from its end.
    pub fn stored_whole(&mut self, bytes: usize) {
        self.whole = bytes;
        self.appended = 0;
    }

    /// `bytes` were appended after the last whole save.
    pub fn appended(&mut self, bytes: usize) {
        self.appended = self.appended.saturating_add(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_outgrow_a_large_document_after_its_multiple() {
        let whole = 1024 * 1024;
        let mut plan = SavePlan::new(whole, 0);
        plan.appended(whole * 2 - 1);
        assert_eq!(plan.next(), Save::Append);
        plan.appended(1);
        assert_eq!(plan.next(), Save::Full);
    }

    #[test]
    fn a_whole_save_starts_the_appends_over() {
        let mut plan = SavePlan::new(1024 * 1024, 3 * 1024 * 1024);
        assert_eq!(plan.next(), Save::Full);
        plan.stored_whole(2048);
        assert_eq!(plan, SavePlan::new(2048, 0));
        assert_eq!(plan.next(), Save::Append);
    }
}
