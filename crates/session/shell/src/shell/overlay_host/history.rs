//! Presentation and navigation helpers for the clipboard and resource history.

use std::path::PathBuf;

use super::DeviceFabricView;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum HistoryTargetAddress {
    File(PathBuf),
    Uri(String),
    App,
}

pub(super) fn clipboard_history_relative_time(created_unix_ms: u64, now_unix_ms: u64) -> String {
    let seconds = now_unix_ms.saturating_sub(created_unix_ms) / 1_000;
    ui::format::relative(i64::try_from(seconds).unwrap_or(i64::MAX))
}

pub(super) fn clipboard_history_source_label(
    item: &crate::compositor::selection::ClipboardHistorySnapshot,
    fabric: Option<&DeviceFabricView>,
) -> String {
    let Some(source) = item.source_device.as_ref() else {
        return crate::shell::i18n::t("history-this-device");
    };
    fabric
        .and_then(|fabric| fabric.devices.iter().find(|device| &device.id == source))
        .map_or_else(
            || source.as_str().to_string(),
            |device| device.label.clone(),
        )
}

pub(super) fn clipboard_history_matches_query(
    item: &crate::compositor::selection::ClipboardHistorySnapshot,
    query: &str,
    fabric: Option<&DeviceFabricView>,
) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    let source = clipboard_history_source_label(item, fabric);
    let mut searchable = source.to_lowercase();
    for mime in &item.mime_types {
        searchable.push(' ');
        searchable.push_str(&mime.to_lowercase());
    }
    if let Some(preview) = item.text_preview.as_deref() {
        searchable.push(' ');
        searchable.push_str(&preview.to_lowercase());
    }
    query
        .split_whitespace()
        .all(|term| searchable.contains(term))
}

/// Resolve only addresses the action explicitly declared. Semantic resource
/// ids such as `notes.page:abc` are labels within an app, not record ids or
/// URIs; treating them as either would make History's Open control navigate
/// somewhere other than the selected action.
pub(super) fn history_target_address(target: &ui::HistoryResourceTarget) -> HistoryTargetAddress {
    if let Some(path) = target.id.strip_prefix("file:") {
        return HistoryTargetAddress::File(PathBuf::from(path));
    }
    if target.id.contains("://") && crate::shell::uri_open::resource_for_uri(&target.id).is_ok() {
        return HistoryTargetAddress::Uri(target.id.clone());
    }
    HistoryTargetAddress::App
}
