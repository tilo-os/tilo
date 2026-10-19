//! Lazy, credential-free descriptions of mounted cloud entries.
#[cfg(test)]
#[path = "mounted_cloud_tests.rs"]
mod tests;
#[allow(
    clippy::wildcard_imports,
    reason = "this file continues the parent module's widget impl; it and the tests under it use the parent's items as the parent does, under every cfg"
)]
use super::*;
use std::sync::mpsc::{Receiver, channel};
use tilo_cloud_files::cloud::{CloudClient, FileDescription, FileReply, FileRequest};

type Descriptions = Vec<(PathBuf, Option<FileDescription>)>;

#[derive(Default)]
pub(super) struct MountedCloud {
    descriptions: HashMap<PathBuf, Option<FileDescription>>,
    snapshot: HashMap<PathBuf, (u64, Option<i64>)>,
    pending: Option<Receiver<Descriptions>>,
    chooser: Option<(String, ui::portal::PortalChannel)>,
}

impl MountedCloud {
    pub(super) fn is_pending(&self) -> bool {
        self.pending.is_some() || self.chooser.is_some()
    }
}

impl FileExplorerWidget {
    pub(super) fn converted_description(&self, path: &Path) -> Option<String> {
        let description = self.mounted_cloud.descriptions.get(path)?.as_ref()?;
        if !description.converted || description.content_writable {
            return None;
        }
        let source = match description.source_mime.as_deref()? {
            "application/vnd.google-apps.document" => "Google Docs",
            "application/vnd.google-apps.spreadsheet" => "Google Sheets",
            "application/vnd.google-apps.presentation" => "Google Slides",
            _ => return Some(t("files-mounted-converted")),
        };
        Some(t_args("files-mounted-source", &[arg("source", source)]))
    }

    pub(super) fn choose_mounted_import(&mut self, entry: String) -> Result<(), String> {
        let path = self
            .visible_entry_path(&entry)
            .ok_or_else(|| t("files-selection-unavailable"))?;
        if self.converted_description(&path).is_none() || !self.importable_cloud_entry(&path) {
            return Err(t("files-import-source-unavailable"));
        }
        if self.mounted_cloud.chooser.is_some() {
            return Err(t("files-cloud-busy"));
        }
        self.mounted_cloud.chooser = Some((
            entry,
            ui::portal::PickRequest::writable_directory(
                &t("files-cloud-destination"),
                &t("files-mounted-copy"),
            )
            .send(),
        ));
        Ok(())
    }

    fn refresh_mounted_snapshot(&mut self) -> bool {
        let snapshot: HashMap<_, _> = self
            .displayed_entries()
            .iter()
            .map(|entry| {
                (
                    entry.path.clone(),
                    (
                        entry.size,
                        entry.modified.map(|time| time.timestamp_millis()),
                    ),
                )
            })
            .collect();
        if snapshot == self.mounted_cloud.snapshot {
            return false;
        }
        self.mounted_cloud.snapshot = snapshot;
        self.mounted_cloud.descriptions.clear();
        // A reply for a previous listing cannot label a replacement item.
        self.mounted_cloud.pending = None;
        true
    }

    pub(super) fn poll_mounted_cloud(&mut self) -> bool {
        let mut changed = self.refresh_mounted_snapshot();
        if let Some(reply) = self
            .mounted_cloud
            .pending
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.mounted_cloud.pending = None;
            self.mounted_cloud.descriptions.extend(reply);
            changed = true;
        }
        if let Some((entry, reply)) = self
            .mounted_cloud
            .chooser
            .as_ref()
            .and_then(|(entry, rx)| rx.try_recv().ok().map(|reply| (entry.clone(), reply)))
        {
            self.mounted_cloud.chooser = None;
            match reply {
                ui::portal::PortalOutcome::Opened(path) => {
                    if let Err(error) = self
                        .visible_entry_path(&entry)
                        .filter(|source| self.converted_description(source).is_some())
                        .ok_or_else(|| t("files-import-source-unavailable"))
                        .and_then(|_| self.import_once(&entry, &path.to_string_lossy()))
                    {
                        self.status = Some(error);
                    }
                }
                ui::portal::PortalOutcome::Failed(error) => self.status = Some(error),
                ui::portal::PortalOutcome::Cancelled => {}
            }
            changed = true;
        }
        let entries = self.displayed_entries();
        let paths: std::collections::HashSet<_> = entries.iter().map(|e| e.path.clone()).collect();
        self.mounted_cloud
            .descriptions
            .retain(|path, _| paths.contains(path));
        if self.mounted_cloud.pending.is_none() {
            let accounts = self.remote_account_snapshot();
            let work: Vec<_> = entries
                .iter()
                .filter(|entry| {
                    !entry.is_dir && !self.mounted_cloud.descriptions.contains_key(&entry.path)
                })
                .filter_map(|entry| {
                    accounts.iter().filter(|a| !a.is_import()).find_map(|a| {
                        let relative = entry.path.strip_prefix(a.mount.as_ref()?).ok()?;
                        Some((
                            entry.path.clone(),
                            a.id.to_string(),
                            relative.to_string_lossy().into_owned(),
                        ))
                    })
                })
                .take(32)
                .collect();
            if !work.is_empty() {
                let (tx, rx) = channel();
                self.mounted_cloud.pending = Some(rx);
                std::thread::spawn(move || {
                    let mut client = tilo_cloud_files::cloud::default_files_socket()
                        .and_then(|socket| CloudClient::connect(socket).ok());
                    let replies = work
                        .into_iter()
                        .map(|(path, account, relative)| {
                            let description = client
                                .as_mut()
                                .and_then(|c| {
                                    c.file(&account, FileRequest::Describe { path: relative })
                                        .ok()
                                })
                                .and_then(|reply| match reply {
                                    FileReply::Description(description) => Some(description),
                                    _ => None,
                                });
                            (path, description)
                        })
                        .collect();
                    let _ = tx.send(replies);
                });
            }
        }
        if changed {
            self.structure_dirty = true;
        }
        changed
    }
}
