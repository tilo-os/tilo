//! Sparse render-only column witnesses for exceptionally long source rows.

use std::ops::Range;

#[derive(Default)]
pub(crate) struct RowViewportIndex {
    checkpoints: Vec<(usize, usize)>, // UTF-8 byte, scalar column; grapheme boundaries
}

impl RowViewportIndex {
    pub(crate) fn new(line: &str) -> Self {
        let mut checkpoints = vec![(0, 0)];
        let mut column = 0usize;
        let mut next = 256usize;
        for (byte, cluster) in ui::text_break::grapheme_indices(line) {
            if column >= next {
                checkpoints.push((byte, column));
                next = column.saturating_add(256);
            }
            column = column.saturating_add(cluster.chars().count());
        }
        checkpoints.push((line.len(), column));
        Self { checkpoints }
    }

    pub(crate) fn column(&self, line: &str, byte: usize) -> usize {
        let position = self.checkpoints.partition_point(|(at, _)| *at <= byte);
        let (start, column) = self
            .checkpoints
            .get(position.saturating_sub(1))
            .copied()
            .unwrap_or_default();
        column.saturating_add(
            line.get(start..byte)
                .map_or(0, |prefix| prefix.chars().count()),
        )
    }

    /// Overscan callers choose their column budget; both endpoints preserve
    /// complete graphemes rather than slicing combining marks or ZWJ emoji.
    pub(crate) fn window(&self, line: &str, columns: Range<usize>) -> (Range<usize>, usize) {
        let position = self
            .checkpoints
            .partition_point(|(_, column)| *column <= columns.start);
        let (byte, mut column) = self
            .checkpoints
            .get(position.saturating_sub(1))
            .copied()
            .unwrap_or_default();
        let mut start = byte;
        let mut start_column = column;
        let mut end = byte;
        for (relative, cluster) in
            ui::text_break::grapheme_indices(line.get(byte..).unwrap_or_default())
        {
            let at = byte.saturating_add(relative);
            let after = column.saturating_add(cluster.chars().count());
            if after <= columns.start {
                start = at.saturating_add(cluster.len());
                start_column = after;
            }
            end = at.saturating_add(cluster.len());
            if after >= columns.end {
                break;
            }
            column = after;
        }
        (start..end, start_column)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_windows_keep_unicode_clusters_tabs_and_scalar_columns() {
        let prefix = "a\t".repeat(10_000);
        let source = format!("{prefix}e\u{301}👩‍👩‍👧‍👦tail");
        let index = RowViewportIndex::new(&source);
        let column = prefix.chars().count();
        let (window, first) =
            index.window(&source, column.saturating_add(1)..column.saturating_add(4));
        assert_eq!(source.get(window), Some("e\u{301}👩‍👩‍👧‍👦"));
        assert_eq!(first, column);
        assert_eq!(index.column(&source, prefix.len()), column);
        assert!(
            index.checkpoints.len() < 100,
            "long rows retain sparse witnesses, not every character"
        );
    }
}
