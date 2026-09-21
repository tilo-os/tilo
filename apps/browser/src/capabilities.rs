//! Device-local permission and download surfaces backed by engine outcomes.
use std::collections::BTreeMap;
use ui::webview::{SharedEngine, WebViewCommand, WebViewEvent};
use ui::widget::{Button, Column, Container, Row, Text, Widget};

pub(crate) struct Capabilities {
    permissions: BTreeMap<u32, (String, String)>,
    // Native events are drained on the UI thread. Tabs share this UI model;
    // background download workers communicate through the engine protocol.
    downloads: std::rc::Rc<std::cell::RefCell<DownloadStore>>,
    observed_download_version: u64,
}
impl Default for Capabilities {
    fn default() -> Self {
        Self {
            permissions: BTreeMap::new(),
            downloads: std::rc::Rc::default(),
            observed_download_version: 0,
        }
    }
}

struct DownloadStore {
    prompts: BTreeMap<u32, (String, String)>,
    records: BTreeMap<u32, Download>,
    admitted: u32,
    detached: u32,
    accepting: bool,
    runtime_known: bool,
    terminal_notice: String,
    version: u64,
}

impl Default for DownloadStore {
    fn default() -> Self {
        Self {
            prompts: BTreeMap::new(),
            records: BTreeMap::new(),
            admitted: 0,
            detached: 0,
            accepting: true,
            runtime_known: false,
            terminal_notice: String::new(),
            version: 0,
        }
    }
}

#[derive(Default)]
struct Download {
    name: String,
    url: String,
    retryable: bool,
    state: String,
    path: String,
    received: u64,
    total: u64,
    error: String,
}
impl Capabilities {
    pub fn share_downloads(&mut self, other: &Self) {
        self.downloads = other.downloads.clone();
    }
    pub fn downloads_changed(&mut self) -> bool {
        let version = self.downloads.borrow().version;
        let changed = version != self.observed_download_version;
        self.observed_download_version = version;
        changed
    }
    pub fn toolbar_status(&self) -> Option<String> {
        let downloads = self.downloads.borrow();
        let active: Vec<_> = downloads
            .records
            .values()
            .filter(|download| download.state == "progress")
            .collect();
        let count = active.len() + downloads.prompts.len();
        if count > 0 {
            let determinate =
                downloads.prompts.is_empty() && active.iter().all(|download| download.total > 0);
            if determinate {
                let received = active
                    .iter()
                    .map(|download| u128::from(download.received))
                    .sum::<u128>();
                let total = active
                    .iter()
                    .map(|download| u128::from(download.total))
                    .sum::<u128>();
                let percent = received
                    .saturating_mul(100)
                    .checked_div(total)
                    .unwrap_or(0)
                    .min(100);
                return Some(format!(
                    "{count} {} · {percent}%",
                    if count == 1 { "download" } else { "downloads" }
                ));
            }
            return Some(format!(
                "{count} active {}",
                if count == 1 { "download" } else { "downloads" }
            ));
        }
        (!downloads.terminal_notice.is_empty()).then(|| downloads.terminal_notice.clone())
    }
    pub fn exit_requires_choice(&self) -> bool {
        let downloads = self.downloads.borrow();
        downloads.admitted > 0
            || downloads.detached > 0
            || !downloads.prompts.is_empty()
            || downloads
                .records
                .values()
                .any(|download| download.state == "progress")
    }
    pub fn exit_cleanup_complete(&self) -> bool {
        let downloads = self.downloads.borrow();
        downloads.runtime_known
            && !downloads.accepting
            && downloads.admitted == 0
            && downloads.detached == 0
            && downloads.prompts.is_empty()
            && downloads
                .records
                .values()
                .all(|download| download.state != "progress")
    }
    pub fn cancel_all(engine: &SharedEngine) {
        engine.send(WebViewCommand::CancelAllDownloads);
    }
    pub fn apply(&mut self, event: WebViewEvent) {
        match event {
            WebViewEvent::PermissionRequested {
                id,
                origin,
                permissions,
            } => {
                if self.permissions.len() < 32 {
                    self.permissions.insert(id, (origin, permissions));
                }
            }
            WebViewEvent::PermissionClosed { id } => {
                self.permissions.remove(&id);
            }
            WebViewEvent::NativeDownloadRequested { id, url, name } => {
                self.request_download(id, url, name);
            }
            WebViewEvent::DownloadUpdated {
                id,
                state,
                path,
                name,
                url,
                retryable,
                received,
                total,
                error,
            } => {
                let mut downloads = self.downloads.borrow_mut();
                downloads.version = downloads.version.wrapping_add(1);
                let prompt = downloads.prompts.get(&id).cloned();
                if !path.is_empty() || state != "progress" {
                    downloads.prompts.remove(&id);
                }
                if downloads.records.len() >= 100
                    && !downloads.records.contains_key(&id)
                    && let Some(old) = downloads
                        .records
                        .iter()
                        .find(|(_, d)| d.state != "progress")
                        .map(|(id, _)| *id)
                {
                    downloads.records.remove(&old);
                }
                if downloads.records.len() < 100 || downloads.records.contains_key(&id) {
                    {
                        let record = downloads.records.entry(id).or_default();
                        if !name.is_empty() {
                            record.name = name;
                        } else if record.name.is_empty() {
                            record.name = prompt
                                .as_ref()
                                .map(|(_, name)| name.clone())
                                .unwrap_or_default();
                        }
                        if !url.is_empty() {
                            record.url = url;
                        } else if record.url.is_empty() {
                            record.url = prompt
                                .as_ref()
                                .map(|(url, _)| url.clone())
                                .unwrap_or_default();
                        }
                        record.retryable |= retryable;
                        if !path.is_empty() {
                            record.path = path;
                        }
                        if received > 0 || record.received == 0 {
                            record.received = received;
                        }
                        if total > 0 || record.total == 0 {
                            record.total = total;
                        }
                        if !error.is_empty() {
                            record.error = error;
                        }
                        record.state.clone_from(&state);
                    }
                    if state != "progress" {
                        downloads.terminal_notice = match state.as_str() {
                            "complete" => "Download complete".into(),
                            "failed" => "Download failed".into(),
                            "cancelled" => "Download cancelled".into(),
                            _ => String::new(),
                        };
                    }
                }
            }
            WebViewEvent::DownloadRuntime {
                admitted,
                detached,
                accepting,
                error,
            } => {
                self.apply_runtime(admitted, detached, accepting, &error);
            }
            _ => {}
        }
    }
    fn request_download(&self, id: u32, url: String, name: String) {
        let mut downloads = self.downloads.borrow_mut();
        downloads.version = downloads.version.wrapping_add(1);
        if downloads.prompts.len() < 32 {
            downloads.prompts.insert(id, (url, name));
        }
    }
    fn apply_runtime(&self, admitted: u32, detached: u32, accepting: bool, error: &str) {
        let mut downloads = self.downloads.borrow_mut();
        downloads.version = downloads.version.wrapping_add(1);
        downloads.admitted = admitted;
        downloads.detached = detached;
        downloads.accepting = accepting;
        downloads.runtime_known = true;
        if !error.is_empty() {
            downloads.prompts.clear();
            for record in downloads.records.values_mut() {
                if record.state == "progress" {
                    record.state = "failed".into();
                    error.clone_into(&mut record.error);
                }
            }
            downloads.terminal_notice = "Download failed".into();
        }
    }
    pub fn prompt(&self, engine: &SharedEngine) -> Option<Box<dyn Widget>> {
        let downloads = self.downloads.borrow();
        let (id, title, permission) =
            if let Some((&id, (origin, rights))) = self.permissions.first_key_value() {
                (
                    id,
                    format!("{origin} requests {rights}. Expires in 60 seconds."),
                    true,
                )
            } else if let Some((&id, (url, name))) = downloads.prompts.first_key_value() {
                (
                    id,
                    format!("Download {name} from {url}? Save in a new folder in Downloads."),
                    false,
                )
            } else {
                return None;
            };
        drop(downloads);
        let mut row = Row::new();
        for (label, allow) in [
            (if permission { "Allow" } else { "Save" }, true),
            ("Deny", false),
        ] {
            let engine = engine.clone();
            row = row.push(Box::new(
                Button::new(
                    &format!("browser.decision.{permission}.{id}.{allow}"),
                    label,
                    None,
                )
                .with_size(80.0, 36.0)
                .on_effect(move || {
                    engine.send(if permission {
                        WebViewCommand::PermissionDecision { id, allow }
                    } else {
                        WebViewCommand::DownloadDecision { id, allow }
                    });
                }),
            ));
        }
        Some(Box::new(
            Container::new().with_background().with_child(Box::new(
                Column::new()
                    .with_intrinsic_height()
                    .push(Box::new(Text::new(&title).with_size(13.0)))
                    .push(Box::new(row)),
            )),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_records_survive_origin_close_and_permissions_stay_local() {
        let first_engine: SharedEngine = ui::webview::test_engine::TestEngine::new();
        let second_engine: SharedEngine = ui::webview::test_engine::TestEngine::new();
        let mut first = Capabilities::default();
        let mut second = Capabilities::default();
        second.share_downloads(&first);
        first.apply(WebViewEvent::PermissionRequested {
            id: 1,
            origin: "https://site.test".into(),
            permissions: "camera".into(),
        });
        assert!(first.prompt(&first_engine).is_some());
        assert!(second.prompt(&second_engine).is_none());
        first.apply(WebViewEvent::DownloadUpdated {
            id: 7,
            state: "progress".into(),
            path: "/tmp/file".into(),
            name: "file".into(),
            url: "https://downloads.test/file".into(),
            retryable: true,
            received: 10,
            total: 20,
            error: String::new(),
        });
        assert!(second.downloads_changed());
        drop(first);
        second.apply(WebViewEvent::DownloadUpdated {
            id: 7,
            state: "complete".into(),
            path: "/tmp/file".into(),
            name: "file".into(),
            url: "https://downloads.test/file".into(),
            retryable: true,
            received: 20,
            total: 20,
            error: String::new(),
        });
        for id in 8..150 {
            second.apply(WebViewEvent::DownloadUpdated {
                id,
                state: "complete".into(),
                path: "/tmp/file".into(),
                name: "file".into(),
                url: "https://downloads.test/file".into(),
                retryable: true,
                received: 20,
                total: 20,
                error: String::new(),
            });
        }
        assert_eq!(second.downloads.borrow().records.len(), 100);
    }

    #[test]
    fn aggregate_progress_handles_large_valid_native_byte_counts() {
        let mut capabilities = Capabilities::default();
        for id in 1..=4 {
            capabilities.apply(WebViewEvent::DownloadUpdated {
                id,
                state: "progress".into(),
                path: "/tmp/large-download".into(),
                name: "large".into(),
                url: "https://downloads.test/large".into(),
                retryable: true,
                received: 1_u64 << 61,
                total: 1_u64 << 62,
                error: String::new(),
            });
        }
        assert_eq!(
            capabilities.toolbar_status().as_deref(),
            Some("4 downloads · 50%")
        );
    }

    #[test]
    fn sparse_terminal_updates_preserve_retry_metadata_and_aggregate_status() {
        let mut capabilities = Capabilities::default();
        capabilities.apply(WebViewEvent::DownloadUpdated {
            id: 41,
            state: "progress".into(),
            path: "/tmp/archive.zip".into(),
            name: "archive.zip".into(),
            url: "https://downloads.test/archive.zip".into(),
            retryable: true,
            received: 256,
            total: 1024,
            error: String::new(),
        });
        assert_eq!(
            capabilities.toolbar_status().as_deref(),
            Some("1 download · 25%")
        );

        capabilities.apply(WebViewEvent::DownloadUpdated {
            id: 41,
            state: "failed".into(),
            path: String::new(),
            name: String::new(),
            url: String::new(),
            retryable: false,
            received: 0,
            total: 0,
            error: "connection reset".into(),
        });
        assert!(
            capabilities
                .downloads
                .borrow()
                .records
                .get(&41)
                .is_some_and(|download| download.retryable)
        );
        assert_eq!(
            capabilities.toolbar_status().as_deref(),
            Some("Download failed")
        );
    }

    #[test]
    fn runtime_disconnect_fails_in_progress_records_and_exit_waits_for_detached_owner() {
        let mut capabilities = Capabilities::default();
        capabilities.apply(WebViewEvent::DownloadUpdated {
            id: 7,
            state: "progress".into(),
            path: "/tmp/file".into(),
            name: "file".into(),
            url: "https://downloads.test/file".into(),
            retryable: true,
            received: 10,
            total: 20,
            error: String::new(),
        });
        capabilities.apply(WebViewEvent::DownloadRuntime {
            admitted: 1,
            detached: 1,
            accepting: true,
            error: String::new(),
        });
        assert!(capabilities.exit_requires_choice());
        capabilities.apply(WebViewEvent::DownloadRuntime {
            admitted: 0,
            detached: 1,
            accepting: false,
            error: String::new(),
        });
        assert!(!capabilities.exit_cleanup_complete());
        capabilities.apply(WebViewEvent::DownloadRuntime {
            admitted: 0,
            detached: 0,
            accepting: false,
            error: "Chromium engine disconnected".into(),
        });
        assert!(capabilities.exit_cleanup_complete());
        assert_eq!(
            capabilities.downloads.borrow().terminal_notice,
            "Download failed"
        );
    }
}
