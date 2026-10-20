//! Bounded, device-local download history.
//!
//! Download bytes and destination capabilities belong to one device, so this
//! model is intentionally not copied into the collaborative Automerge doc: it
//! is the app's device-local setting, never synced.
//! Stable engine-issued IDs still make every row addressable by global search,
//! action history, agents, and the Files hand-off on this device.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const MAX_DOWNLOADS: usize = 500;
/// What the stored list may take of the app's device-local settings; the
/// oldest rows give way first.
const MAX_STORED_BYTES: usize = 192 * 1024;
const MAX_TEXT_BYTES: usize = 8 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DownloadStatus {
    AwaitingDestination,
    Downloading,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DownloadEntry {
    pub id: String,
    pub source_url: String,
    pub suggested_name: String,
    pub destination: Option<PathBuf>,
    pub received_bytes: u64,
    pub total_bytes: Option<u64>,
    pub progress: f64,
    pub status: DownloadStatus,
    pub error: Option<String>,
}

impl DownloadEntry {
    pub fn resource_id(&self) -> String {
        format!("{}/{}", crate::actions::DOWNLOADS_SCOPE, self.id)
    }

    pub fn display_name(&self) -> &str {
        self.destination
            .as_deref()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.suggested_name)
    }
}

/// The download list as stored in the app's device-local settings, kept as
/// raw rows [`Downloads::load`] validates one by one.
#[derive(Default, Serialize, Deserialize)]
pub struct Stored {
    #[serde(default)]
    downloads: Vec<serde_json::Value>,
}

impl ui::storage::SettingsSchema for Stored {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::DeviceLocal;
}

#[derive(Clone, Debug, Default)]
pub struct Downloads {
    /// Where the list is kept. `None` keeps it in memory only: a private
    /// window, a screenshot.
    store: Option<ui::storage::Settings<Stored>>,
    entries: Vec<DownloadEntry>,
    revision: u64,
}

impl Downloads {
    /// The list kept in `store`, which [`Self::save`] writes back to.
    pub fn load(store: ui::storage::Settings<Stored>) -> Self {
        let stored = Self::from_stored(store.get());
        Self {
            store: Some(store),
            ..stored
        }
    }

    /// Checkpoint the bounded download history in the device's settings.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the settings could not be written.
    pub fn save(&self) -> std::io::Result<()> {
        match &self.store {
            Some(store) => store.set(&self.stored()),
            None => Ok(()),
        }
    }

    /// The rows to keep, newest last, within [`MAX_STORED_BYTES`].
    fn stored(&self) -> Stored {
        let mut rows: Vec<serde_json::Value> = self
            .entries
            .iter()
            .filter_map(|entry| serde_json::to_value(entry).ok())
            .collect();
        let mut size: usize = rows.iter().map(|row| row.to_string().len() + 1).sum();
        while size > MAX_STORED_BYTES && !rows.is_empty() {
            size -= rows.remove(0).to_string().len() + 1;
        }
        Stored { downloads: rows }
    }

    pub fn entries(&self) -> &[DownloadEntry] {
        &self.entries
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn get(&self, id: &str) -> Option<&DownloadEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    pub fn requested(&mut self, id: String, source_url: String, suggested_name: &str) -> bool {
        if !valid_id(&id) || !valid_source(&source_url) || self.get(&id).is_some() {
            return false;
        }
        if self.entries.len() >= MAX_DOWNLOADS {
            self.entries.remove(0);
        }
        self.entries.push(DownloadEntry {
            id,
            source_url: bounded(source_url),
            suggested_name: safe_name(suggested_name),
            destination: None,
            received_bytes: 0,
            total_bytes: None,
            progress: 0.0,
            status: DownloadStatus::AwaitingDestination,
            error: None,
        });
        self.bump();
        true
    }

    pub fn destination(&mut self, id: &str, path: PathBuf) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
            if !path.is_absolute() {
                entry.status = DownloadStatus::Failed;
                entry.error = Some("The save destination was not an absolute path".into());
                self.bump();
                return;
            }
            entry.destination = Some(path);
            entry.status = DownloadStatus::Downloading;
            entry.error = None;
            self.bump();
        }
    }

    pub fn progress(&mut self, id: &str, received: u64, total: Option<u64>, progress: f64) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
            entry.received_bytes = received;
            entry.total_bytes = total;
            entry.progress = progress.clamp(0.0, 1.0);
            entry.status = DownloadStatus::Downloading;
            self.bump();
        }
    }

    pub fn finished(&mut self, id: &str, path: PathBuf) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
            if !path.is_absolute() {
                entry.status = DownloadStatus::Failed;
                entry.error = Some("The completed destination was invalid".into());
                self.bump();
                return;
            }
            entry.destination = Some(path);
            entry.progress = 1.0;
            entry.status = DownloadStatus::Completed;
            entry.error = None;
            self.bump();
        }
    }

    pub fn failed(&mut self, id: &str, error: String, cancelled: bool) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
            // A portal/setup failure is recorded before cancelling WebKit's
            // paused object. Its resulting cancellation signal must not erase
            // the actionable reason the user needs to see.
            if cancelled && entry.status == DownloadStatus::Failed && entry.error.is_some() {
                return;
            }
            entry.status = if cancelled {
                DownloadStatus::Cancelled
            } else {
                DownloadStatus::Failed
            };
            entry.error = (!cancelled).then(|| bounded(error));
            self.bump();
        }
    }

    fn from_stored(stored: Stored) -> Self {
        let mut entries: Vec<DownloadEntry> = stored
            .downloads
            .into_iter()
            .filter_map(|row| serde_json::from_value(row).ok())
            .filter(|entry: &DownloadEntry| valid_id(&entry.id) && valid_source(&entry.source_url))
            .collect();
        entries.truncate(MAX_DOWNLOADS);
        for entry in &mut entries {
            entry.source_url = bounded(std::mem::take(&mut entry.source_url));
            entry.suggested_name = safe_name(&entry.suggested_name);
            entry.error = entry.error.take().map(bounded);
            entry.progress = entry.progress.clamp(0.0, 1.0);
            if entry
                .destination
                .as_deref()
                .is_some_and(|path| !path.is_absolute())
            {
                entry.destination = None;
                entry.status = DownloadStatus::Failed;
                entry.error = Some("The stored download destination was invalid".into());
            }
            if matches!(
                entry.status,
                DownloadStatus::AwaitingDestination | DownloadStatus::Downloading
            ) {
                entry.status = DownloadStatus::Failed;
                entry.error = Some("Browser closed before this download completed".into());
            }
        }
        Self {
            store: None,
            entries,
            revision: 0,
        }
    }

    fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_source(source: &str) -> bool {
    url::Url::parse(source)
        .ok()
        .is_some_and(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some())
}

fn bounded(mut value: String) -> String {
    if value.len() <= MAX_TEXT_BYTES {
        return value;
    }
    let mut end = MAX_TEXT_BYTES;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value.truncate(end);
    value
}

fn safe_name(name: &str) -> String {
    let name = Path::new(name)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("download");
    bounded(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Download store lifecycle, bounds, restore and destinations.
    #[test]
    fn download_rows_are_bounded_restart_safe_and_truthful() {
        // Phase `lifecycle_is_stable_bounded_and_restart_safe`.
        {
            let mut downloads = Downloads::default();
            downloads.requested("stable-1".into(), "https://example.test/a".into(), "a.bin");
            downloads.destination("stable-1", "/tmp/a.bin".into());
            downloads.progress("stable-1", 50, Some(100), 0.5);
            let restored = Downloads::from_stored(downloads.stored());
            let entry = restored.get("stable-1").unwrap();
            assert_eq!(entry.resource_id(), "browser.downloads/stable-1");
            assert_eq!(entry.status, DownloadStatus::Failed);
        }

        // Phase `hostile_ids_names_and_corpora_are_bounded`.
        {
            let mut downloads = Downloads::default();
            downloads.requested("../escape".into(), "https://bad.test".into(), "bad");
            assert!(downloads.entries().is_empty());
            for index in 0..MAX_DOWNLOADS + 20 {
                downloads.requested(
                    format!("id-{index}"),
                    format!("https://{index}.test"),
                    "../safe.bin",
                );
            }
            assert_eq!(downloads.entries().len(), MAX_DOWNLOADS);
            assert!(
                downloads
                    .entries()
                    .iter()
                    .all(|entry| entry.suggested_name == "safe.bin")
            );
        }

        // Phase `completed_failed_and_cancelled_rows_restore_truthfully`.
        {
            let mut downloads = Downloads::default();
            assert!(downloads.requested(
                "complete".into(),
                "https://example.test/complete".into(),
                "complete.bin",
            ));
            downloads.finished("complete", "/tmp/complete.bin".into());
            assert!(downloads.requested(
                "failed".into(),
                "https://example.test/failed".into(),
                "failed.bin",
            ));
            downloads.failed("failed", "network failed".repeat(2_000), false);
            assert!(downloads.requested(
                "cancelled".into(),
                "https://example.test/cancelled".into(),
                "cancelled.bin",
            ));
            downloads.failed("cancelled", "ignored".into(), true);

            let restored = Downloads::from_stored(downloads.stored());
            assert_eq!(
                restored.get("complete").unwrap().status,
                DownloadStatus::Completed
            );
            assert_eq!(
                restored.get("failed").unwrap().status,
                DownloadStatus::Failed
            );
            assert!(
                restored
                    .get("failed")
                    .unwrap()
                    .error
                    .as_ref()
                    .unwrap()
                    .len()
                    <= MAX_TEXT_BYTES
            );
            assert_eq!(
                restored.get("cancelled").unwrap().status,
                DownloadStatus::Cancelled
            );
            assert!(restored.get("cancelled").unwrap().error.is_none());

            let previous_error = restored.get("failed").unwrap().error.clone();
            let mut restored = restored;
            restored.failed("failed", "Cancelled by cleanup".into(), true);
            assert_eq!(
                restored.get("failed").unwrap().status,
                DownloadStatus::Failed
            );
            assert_eq!(restored.get("failed").unwrap().error, previous_error);
        }

        // Phase `relative_destinations_never_become_openable_resources`.
        {
            let mut downloads = Downloads::default();
            assert!(downloads.requested(
                "path".into(),
                "https://example.test/path".into(),
                "../safe.bin",
            ));
            downloads.finished("path", "../../escape.bin".into());
            let entry = downloads.get("path").unwrap();
            assert_eq!(entry.suggested_name, "safe.bin");
            assert_eq!(entry.status, DownloadStatus::Failed);
            assert!(entry.destination.is_none());
        }
    }

    #[test]
    fn the_list_is_a_device_local_setting_that_survives_a_restart_within_its_budget() {
        let dir = tempfile::tempdir().unwrap();
        let store = || {
            ui::storage::Settings::<Stored>::for_app("so.tilo.browser")
                .in_file(dir.path().join("settings.json"))
        };
        assert_eq!(store().scope(), ui::storage::Scope::DeviceLocal);

        let mut downloads = Downloads::load(store());
        assert!(downloads.requested(
            "kept".into(),
            "https://example.test/kept".into(),
            "kept.bin",
        ));
        downloads.finished("kept", "/tmp/kept.bin".into());
        downloads.save().unwrap();
        let restored = Downloads::load(store());
        assert_eq!(
            restored.get("kept").unwrap().status,
            DownloadStatus::Completed
        );

        // A list too large for the device's settings drops its oldest rows.
        let long = "x".repeat(MAX_TEXT_BYTES - 64);
        for index in 0..MAX_DOWNLOADS {
            downloads.requested(
                format!("big-{index}"),
                format!("https://example.test/{long}/{index}"),
                "big.bin",
            );
        }
        let stored = downloads.stored();
        assert!(serde_json::to_string(&stored).unwrap().len() <= MAX_STORED_BYTES);
        let kept = Downloads::from_stored(stored);
        assert!(kept.get("kept").is_none(), "the oldest row gives way first");
        assert!(kept.get(&format!("big-{}", MAX_DOWNLOADS - 1)).is_some());
    }
}
