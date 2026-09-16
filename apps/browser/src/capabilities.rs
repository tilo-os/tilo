//! Device-local permission and download surfaces backed by engine outcomes.
use std::collections::BTreeMap;
use ui::webview::{SharedEngine, WebViewCommand, WebViewEvent};
use ui::widget::{Button, Column, Container, Row, Text, Widget};

pub(crate) struct Capabilities {
    permissions: BTreeMap<u32, (String, String)>,
    downloads: std::sync::Arc<std::sync::Mutex<DownloadStore>>,
    observed_download_version: u64,
    notice: ui::LocalState<String>,
}
impl Default for Capabilities {
    fn default() -> Self {
        Self {
            permissions: BTreeMap::new(),
            downloads: Default::default(),
            observed_download_version: 0,
            notice: ui::LocalState::new(String::new()),
        }
    }
}

#[derive(Default)]
struct DownloadStore {
    prompts: BTreeMap<u32, (String, String)>,
    records: BTreeMap<u32, Download>,
    version: u64,
}

fn open_download(path: &std::path::Path, reveal: bool) -> Result<(), String> {
    if !path.is_absolute() || !path.is_file() {
        return Err("The downloaded file is no longer available.".into());
    }
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("/usr/bin/open")
    } else {
        std::process::Command::new("xdg-open")
    };
    if cfg!(target_os = "macos") {
        if reveal {
            command.arg("-R");
        }
        command.arg("--").arg(path);
    } else {
        command.arg(if reveal {
            path.parent().unwrap_or(path)
        } else {
            path
        });
    }
    let status = command.status().map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("Could not open the download. Try revealing its folder.".into())
    }
}

#[derive(Default)]
struct Download {
    name: String,
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
        let version = self.downloads.lock().unwrap().version;
        let changed = version != self.observed_download_version;
        self.observed_download_version = version;
        changed
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
            WebViewEvent::DownloadRequested { id, url, name } => {
                let mut downloads = self.downloads.lock().unwrap();
                downloads.version = downloads.version.wrapping_add(1);
                if downloads.prompts.len() < 32 {
                    downloads.prompts.insert(id, (url, name));
                }
            }
            WebViewEvent::DownloadUpdated {
                id,
                state,
                path,
                name,
                received,
                total,
                error,
            } => {
                let mut downloads = self.downloads.lock().unwrap();
                downloads.version = downloads.version.wrapping_add(1);
                if !path.is_empty() || state != "progress" {
                    downloads.prompts.remove(&id);
                }
                if downloads.records.len() >= 100 && !downloads.records.contains_key(&id) {
                    if let Some(old) = downloads
                        .records
                        .iter()
                        .find(|(_, d)| d.state != "progress")
                        .map(|(id, _)| *id)
                    {
                        downloads.records.remove(&old);
                    }
                }
                if downloads.records.len() < 100 || downloads.records.contains_key(&id) {
                    downloads.records.insert(
                        id,
                        Download {
                            name,
                            state,
                            path,
                            received,
                            total,
                            error,
                        },
                    );
                }
            }
            _ => {}
        }
    }
    pub fn prompt(&self, engine: &SharedEngine) -> Option<Box<dyn Widget>> {
        let downloads = self.downloads.lock().unwrap();
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
    pub fn downloads(&self, engine: &SharedEngine) -> Box<dyn Widget> {
        let downloads = self.downloads.lock().unwrap();
        let mut rows = Column::new().with_no_padding().with_intrinsic_height();
        rows = rows.push(Box::new(
            Text::from_state(&self.notice, Clone::clone).with_size(13.0),
        ));
        if downloads.records.is_empty() {
            rows = rows.push(Box::new(Text::new("No downloads this session.")));
        }
        for (&id, d) in downloads.records.iter().rev() {
            let summary = format!(
                "{} · {} · {} / {} bytes\n{} {}",
                d.name, d.state, d.received, d.total, d.path, d.error
            );
            let mut row = Row::new().push(Box::new(Text::new(&summary).with_size(13.0)));
            if d.state == "progress" {
                let engine = engine.clone();
                row = row.push(Box::new(
                    Button::new(&format!("browser.download.cancel.{id}"), "Cancel", None)
                        .on_effect(move || engine.send(WebViewCommand::CancelDownload { id })),
                ));
            }
            if d.state == "complete" && !d.path.is_empty() {
                for (name, label, reveal) in [("open", "Open", false), ("reveal", "Reveal", true)] {
                    let path = std::path::PathBuf::from(&d.path);
                    let notice = self.notice.writer();
                    row = row.push(Box::new(
                        Button::new(&format!("browser.download.{name}.{id}"), label, None)
                            .on_effect(move || {
                                let path = path.clone();
                                let notice = notice.clone();
                                let _ = std::thread::Builder::new()
                                    .name("browser-open-download".into())
                                    .spawn(move || {
                                        notice.set(
                                            open_download(&path, reveal).err().unwrap_or_default(),
                                        );
                                    });
                            }),
                    ));
                }
            }
            rows = rows.push(Box::new(Container::new().with_child(Box::new(row))));
        }
        Box::new(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn contains(widget: &dyn Widget, id: &str) -> bool {
        widget.test_id() == Some(id) || widget.children().iter().any(|child| contains(*child, id))
    }
    #[test]
    fn download_records_survive_origin_close_and_permissions_stay_local() {
        let first_engine: SharedEngine = ui::webview::test_engine::TestEngine::new();
        let second_capture = ui::webview::test_engine::TestEngine::new();
        let second_engine: SharedEngine = second_capture.clone();
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
            received: 10,
            total: 20,
            error: String::new(),
        });
        assert!(second.downloads_changed());
        let mut rows = second.downloads(&second_engine);
        assert!(contains(rows.as_ref(), "browser.download.cancel.7"));
        let result = rows
            .start_resource_operation(
                ui::qos_actions::InvocationId::new(),
                ui::qos_actions::OperationKind::Effect,
                "qos.ui.control.invoke",
                &serde_json::to_vec("browser.download.cancel.7").unwrap(),
                &automerge::AutoCommit::new(),
                &automerge::ROOT,
            )
            .unwrap();
        assert!(result.is_some());
        assert!(
            second_capture
                .commands()
                .contains(&WebViewCommand::CancelDownload { id: 7 })
        );
        drop(first);
        second.apply(WebViewEvent::DownloadUpdated {
            id: 7,
            state: "complete".into(),
            path: "/tmp/file".into(),
            name: "file".into(),
            received: 20,
            total: 20,
            error: String::new(),
        });
        assert!(contains(
            second.downloads(&second_engine).as_ref(),
            "browser.download.open.7"
        ));
        for id in 8..150 {
            second.apply(WebViewEvent::DownloadUpdated {
                id,
                state: "complete".into(),
                path: "/tmp/file".into(),
                name: "file".into(),
                received: 20,
                total: 20,
                error: String::new(),
            });
        }
        assert_eq!(second.downloads.lock().unwrap().records.len(), 100);
    }

    #[test]
    fn only_completed_downloads_offer_explicit_open_and_reveal() {
        let engine: SharedEngine = ui::webview::test_engine::TestEngine::new();
        let mut capabilities = Capabilities::default();
        for state in ["progress", "failed", "complete"] {
            capabilities.apply(WebViewEvent::DownloadUpdated {
                id: 7,
                state: state.into(),
                path: "/tmp/test-download.txt".into(),
                name: "test".into(),
                received: 8,
                total: 8,
                error: String::new(),
            });
            let view = capabilities.downloads(&engine);
            assert_eq!(
                contains(view.as_ref(), "browser.download.open.7"),
                state == "complete"
            );
            assert_eq!(
                contains(view.as_ref(), "browser.download.reveal.7"),
                state == "complete"
            );
        }
    }
}
