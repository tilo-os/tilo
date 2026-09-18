//! State and presentation helpers for the file-history panel.
//!
//! This module owns the bounded text-preview boundary and line-diff policy;
//! the main widget only coordinates loading, selection, and painting.

use crate::i18n::t;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HistoryPanel {
    pub(crate) record_id: String,
    pub(crate) path: PathBuf,
    /// Newest first. The filesystem projection is bounded, so this remains
    /// cheap to paint and safe to expose to accessibility clients.
    pub(crate) versions: Vec<crate::model::HistoryVersion>,
    pub(crate) heads: Vec<String>,
    pub(crate) selected: usize,
    pub(crate) compare: bool,
    pub(crate) selected_preview: Option<String>,
    pub(crate) current_preview: Option<String>,
    pub(crate) preview_loading: bool,
}

impl HistoryPanel {
    pub(crate) fn selected_version(&self) -> Option<&crate::model::HistoryVersion> {
        self.versions.get(self.selected)
    }

    pub(crate) fn current_version(&self) -> Option<&crate::model::HistoryVersion> {
        self.versions
            .iter()
            .find(|version| self.heads.contains(&version.id))
    }

    pub(crate) fn selected_is_current(&self) -> bool {
        self.selected_version()
            .is_some_and(|version| self.heads.contains(&version.id))
    }
}

pub(crate) fn format_timestamp(timestamp_ms: u64) -> String {
    i64::try_from(timestamp_ms)
        .ok()
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
        .map_or_else(
            || t("files-time-unknown"),
            |timestamp| crate::i18n::format_datetime(timestamp.with_timezone(&chrono::Local)),
        )
}

pub(crate) fn read_current_text_preview(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() > collab_types::MAX_HISTORY_PREVIEW_BYTES as u64 {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    (!text.chars().any(|character| character == '\0')).then_some(text)
}

#[allow(
    clippy::indexing_slicing,
    reason = "the dynamic-programming table is sized one sentinel row/column larger and every index is guarded by the corresponding source length"
)]
pub(crate) fn text_diff_lines(before: &str, after: &str) -> Vec<(char, String)> {
    let before: Vec<&str> = before.lines().take(120).collect();
    let after: Vec<&str> = after.lines().take(120).collect();
    let mut lengths =
        vec![vec![0_usize; after.len().saturating_add(1)]; before.len().saturating_add(1)];
    for left in (0..before.len()).rev() {
        for right in (0..after.len()).rev() {
            lengths[left][right] = if before[left] == after[right] {
                lengths[left + 1][right + 1].saturating_add(1)
            } else {
                lengths[left + 1][right].max(lengths[left][right + 1])
            };
        }
    }
    let (mut left, mut right) = (0, 0);
    let mut output = Vec::new();
    while left < before.len() || right < after.len() {
        if left < before.len() && right < after.len() && before[left] == after[right] {
            output.push((' ', before[left].to_string()));
            left += 1;
            right += 1;
        } else if right < after.len()
            && (left == before.len() || lengths[left][right + 1] > lengths[left + 1][right])
        {
            output.push(('+', after[right].to_string()));
            right += 1;
        } else if left < before.len() {
            output.push(('-', before[left].to_string()));
            left += 1;
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::text_diff_lines;

    #[test]
    fn replacements_read_as_removed_then_added() {
        assert_eq!(
            text_diff_lines("Ship: Friday\n", "Ship: Monday\n"),
            vec![
                ('-', "Ship: Friday".to_string()),
                ('+', "Ship: Monday".to_string())
            ]
        );
    }

    #[test]
    fn previews_are_bounded_to_the_visible_diff_budget() {
        let before = (0..130).map(|line| format!("old {line}"));
        let after = (0..130).map(|line| format!("new {line}"));
        let diff = text_diff_lines(
            &before.collect::<Vec<_>>().join("\n"),
            &after.collect::<Vec<_>>().join("\n"),
        );
        assert_eq!(diff.len(), 240);
        assert!(diff.iter().all(|(_, line)| !line.ends_with(" 120")));
    }
}
