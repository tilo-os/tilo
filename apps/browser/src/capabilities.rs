//! Device-local permission and download surfaces backed by engine outcomes.
use std::collections::BTreeMap;
use ui::webview::{SharedEngine, WebViewCommand, WebViewEvent};
use ui::widget::{Button, Column, Container, Row, Text, Widget};

#[derive(Default)]
pub(crate) struct Capabilities {
    permissions: BTreeMap<u32, (String, String)>,
    prompts: BTreeMap<u32, (String, String)>,
    downloads: BTreeMap<u32, Download>,
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
                if self.prompts.len() < 32 {
                    self.prompts.insert(id, (url, name));
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
                if !path.is_empty() || state != "progress" {
                    self.prompts.remove(&id);
                }
                if self.downloads.len() >= 100 && !self.downloads.contains_key(&id) {
                    if let Some(old) = self
                        .downloads
                        .iter()
                        .find(|(_, d)| d.state != "progress")
                        .map(|(id, _)| *id)
                    {
                        self.downloads.remove(&old);
                    }
                }
                if self.downloads.len() < 100 || self.downloads.contains_key(&id) {
                    self.downloads.insert(
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
        let (id, title, permission) =
            if let Some((&id, (origin, rights))) = self.permissions.first_key_value() {
                (
                    id,
                    format!("{origin} requests {rights}. Expires in 60 seconds."),
                    true,
                )
            } else if let Some((&id, (url, name))) = self.prompts.first_key_value() {
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
        let mut rows = Column::new().with_no_padding().with_intrinsic_height();
        if self.downloads.is_empty() {
            rows = rows.push(Box::new(Text::new("No downloads this session.")));
        }
        for (&id, d) in self.downloads.iter().rev() {
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
            rows = rows.push(Box::new(Container::new().with_child(Box::new(row))));
        }
        Box::new(rows)
    }
}
