//! Provider-backed browsing is deliberately not a synthetic filesystem mount.
#[cfg(test)]
#[path = "cloud_cancel_tests.rs"]
mod cancel_tests;
#[cfg(test)]
#[path = "cloud_job_window_tests.rs"]
mod job_window_tests;
#[cfg(test)]
#[path = "cloud_browse_tests.rs"]
mod tests;
use super::*;
use crate::effects::FilesEffect;
use std::collections::HashSet;
use std::sync::mpsc::{Receiver, channel};
use tilo_cloud_files::cloud::{CloudClient, ImportEntry, ImportKind, ImportReply, ImportRequest};

#[derive(Default)]
pub(super) struct CloudView {
    pub(super) account: String,
    pub(super) job_id: Option<String>,
    folder: Option<String>,
    pub(super) entries: Vec<ImportEntry>,
    folder_label: Option<String>,
    header_shown: bool,
    selected: HashSet<String>,
    next: Option<u32>,
    pub(super) pending: Option<Receiver<Result<ImportReply, String>>>,
    pub(super) chooser: Option<ui::portal::PortalChannel>,
    proposal: Vec<ImportEntry>,
    pub(super) transfer: Option<(String, Receiver<crate::file_ops::Outcome>)>,
    retry: Option<crate::file_ops::FileOp>,
    destination: Option<PathBuf>,
    progress: crate::file_ops::TransferProgress,
    error: Option<String>,
    pub(super) active: bool,
}

impl FileExplorerWidget {
    pub(super) fn browse_cloud(
        &mut self,
        account: String,
        folder: Option<String>,
        offset: u32,
    ) -> Result<(), String> {
        let descriptor = self
            .remote_account(&account)
            .ok_or("account is no longer available")?;
        if !descriptor.is_import()
            || descriptor.health == crate::remote_accounts::RemoteAccountHealth::Disabled
        {
            return Err(t("files-import-source-unavailable"));
        }
        if offset != 0 {
            if !self.cloud.active
                || self.cloud.account != account
                || self.cloud.folder != folder
                || self.cloud.next != Some(offset)
            {
                return Err("cloud page is no longer current".into());
            }
            if self.cloud.pending.is_some() {
                return Err(t("files-cloud-busy"));
            }
        } else {
            self.cloud.folder_label = folder.as_ref().and_then(|id| {
                self.cloud
                    .entries
                    .iter()
                    .find(|entry| &entry.id == id)
                    .map(|entry| entry.name.clone())
            });
            // Dropping the old receiver detaches any in-flight page. Its worker
            // cannot publish into the new folder or account when it finishes.
            self.leave_cloud_browse();
        }
        self.cloud.account.clone_from(&account);
        self.cloud.folder.clone_from(&folder);
        self.cloud.active = true;
        self.cloud.error = None;
        self.entries.clear();
        self.error = None;
        self.is_loading = false;
        self.close_collab_panel();
        let (tx, rx) = channel();
        self.cloud.pending = Some(rx);
        std::thread::spawn(move || {
            let result = (|| {
                let socket = tilo_cloud_files::cloud::default_files_socket()
                    .ok_or("cloud service unavailable")?;
                CloudClient::connect(socket)
                    .and_then(|mut client| {
                        client.import(&account, ImportRequest::Browse { folder, offset })
                    })
                    .map_err(|e| e.to_string())
            })();
            let _ = tx.send(result);
        });
        self.structure_dirty = true;
        Ok(())
    }

    pub(super) fn leave_cloud_browse(&mut self) {
        self.cloud.active = false;
        self.cloud.pending = None;
        self.cloud.entries.clear();
        self.cloud.selected.clear();
        self.cloud.next = None;
        self.cloud.chooser = None;
        self.cloud.proposal.clear();
    }

    pub(super) fn cloud_import_pending(&self) -> bool {
        self.cloud.transfer.is_some()
            || self
                .awaiting_ops
                .iter()
                .any(|(op, _)| matches!(op, crate::file_ops::FileOp::CloudImport { .. }))
    }

    pub(super) fn cloud_effect(
        &mut self,
        effect: FilesEffect,
    ) -> Result<Option<serde_json::Value>, String> {
        match effect {
            FilesEffect::BrowseCloud { account, folder } => {
                self.browse_cloud(account, folder, 0)?;
            }
            FilesEffect::MoreCloud => {
                let offset = self.cloud.next.ok_or("no further cloud page")?;
                self.browse_cloud(
                    self.cloud.account.clone(),
                    self.cloud.folder.clone(),
                    offset,
                )?;
            }
            FilesEffect::SelectCloud { entry } => {
                if !self.cloud.active || !self.cloud.entries.iter().any(|e| e.id == entry) {
                    return Err("cloud entry is no longer visible".into());
                }
                if !self.cloud.selected.remove(&entry) {
                    self.cloud.selected.insert(entry);
                }
            }
            FilesEffect::ImportCloud { folder } => {
                if !self.cloud.active {
                    return Err("cloud view is no longer active".into());
                }
                if self.cloud.chooser.is_some() || self.cloud_import_pending() {
                    return Err(t("files-cloud-busy"));
                }
                self.cloud.proposal = self
                    .cloud
                    .entries
                    .iter()
                    .filter(|e| self.cloud.selected.contains(&e.id))
                    .cloned()
                    .collect();
                if self.cloud.proposal.is_empty() {
                    return Err(t("files-selection-empty"));
                }
                if let Some(folder) = folder {
                    self.admit_cloud_destination(PathBuf::from(folder))?;
                } else {
                    self.cloud.chooser = Some(
                        ui::portal::PickRequest::writable_directory(
                            &t("files-cloud-destination"),
                            &t("files-cloud-import"),
                        )
                        .send(),
                    );
                }
            }
            FilesEffect::CloudProgress => {
                return Ok(Some(serde_json::json!({
                    "phase": self.cloud.progress.phase, "completed": self.cloud.progress.completed,
                    "bytes": self.cloud.progress.bytes, "failed": self.cloud.progress.failed,
                    "error": self.cloud.error,
                })));
            }
            FilesEffect::CancelCloud => {
                if let Some(id) = &self.cloud.job_id {
                    crate::cloud_jobs::cancel(id);
                }
            }
            FilesEffect::RetryCloud => {
                if self.cloud_import_pending() {
                    return Err(t("files-cloud-busy"));
                }
                let mut op = self.cloud.retry.clone().ok_or("no failed cloud entries")?;
                if let crate::file_ops::FileOp::CloudImport { job_id, .. } = &mut op {
                    *job_id = uuid::Uuid::new_v4().to_string();
                }
                self.cloud.retry = Some(op.clone());
                self.queue(
                    op,
                    OpFollowUp::Paste {
                        count: 1,
                        cut: false,
                    },
                );
            }
            FilesEffect::RevealCloud => {
                let path = self
                    .cloud
                    .destination
                    .clone()
                    .ok_or("no import destination")?;
                self.navigate(path);
            }
            _ => return Err("not a cloud browse effect".into()),
        }
        self.structure_dirty = true;
        Ok(None)
    }

    fn admit_cloud_destination(&mut self, path: PathBuf) -> Result<(), String> {
        if !path.is_absolute() || !path.is_dir() {
            return Err(t("files-import-local-destination"));
        }
        let canonical = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
        if self
            .remote_account_snapshot()
            .iter()
            .filter_map(|a| a.mount.as_ref())
            .any(|mount| std::fs::canonicalize(mount).is_ok_and(|m| canonical.starts_with(m)))
        {
            return Err(t("files-import-local-destination"));
        }
        let snapshot = self.remote_account_snapshot();
        let folder = path.display().to_string();
        let destination = crate::cloud_transfer::pin_destination(
            &folder,
            snapshot
                .iter()
                .filter_map(|account| account.mount.as_deref()),
        )?;
        let entries = std::mem::take(&mut self.cloud.proposal);
        let count = entries.len();
        let op = crate::file_ops::FileOp::CloudImport {
            job_id: uuid::Uuid::new_v4().to_string(),
            account: self.cloud.account.clone(),
            entries,
            folder,
            destination,
        };
        self.cloud.destination = Some(path);
        self.cloud.retry = Some(op.clone());
        self.cloud.progress = crate::file_ops::TransferProgress::default();
        self.queue(op, OpFollowUp::Paste { count, cut: false });
        Ok(())
    }

    pub(super) fn poll_cloud(&mut self) -> bool {
        let mut changed = false;
        if let Some(reply) = self
            .cloud
            .pending
            .as_ref()
            .and_then(|rx| match rx.try_recv() {
                Ok(reply) => Some(reply),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Some(Err(t("files-cloud-invalid-reply")))
                }
            })
        {
            self.cloud.pending = None;
            match reply {
                Ok(ImportReply::Page {
                    entries,
                    next_offset,
                    ..
                }) => {
                    self.cloud.entries.extend(entries);
                    self.cloud.next = next_offset;
                }
                Ok(ImportReply::Error(error)) => self.cloud.error = Some(error.message),
                Ok(_) => self.cloud.error = Some(t("files-cloud-invalid-reply")),
                Err(error) => self.cloud.error = Some(error),
            }
            changed = true;
        }
        if let Some(reply) = self
            .cloud
            .chooser
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.cloud.chooser = None;
            if let ui::portal::PortalOutcome::Opened(path) = reply {
                if let Err(error) = self.admit_cloud_destination(path) {
                    self.cloud.error = Some(error);
                }
            } else if let ui::portal::PortalOutcome::Failed(error) = reply {
                self.cloud.error = Some(error);
            }
            changed = true;
        }
        let progress = self
            .cloud
            .job_id
            .as_deref()
            .map(crate::cloud_jobs::snapshot)
            .unwrap_or_default();
        if progress != self.cloud.progress {
            self.cloud.progress = progress;
            changed = true;
        }
        if let Some(outcome) = self
            .cloud
            .transfer
            .as_ref()
            .and_then(|(_, rx)| rx.try_recv().ok())
        {
            if let Some((id, _)) = self.cloud.transfer.take() {
                self.cloud_finished(&outcome);
                self.local_outcomes.insert(id.clone(), outcome.clone());
                self.settle_op(
                    &id,
                    OpFollowUp::Paste {
                        count: outcome.result().len(),
                        cut: false,
                    },
                    &outcome,
                );
            }
            changed = true;
        }
        self.structure_dirty |= changed;
        changed
    }

    pub(super) fn cloud_finished(&mut self, outcome: &crate::file_ops::Outcome) {
        self.cloud.error = outcome.reason().map(str::to_owned);
        if let Some(crate::file_ops::FileOp::CloudImport {
            account, entries, ..
        }) = self.cloud.retry.as_mut()
        {
            crate::cloud_transfer::retain_uncommitted(account, entries, outcome.result());
            if entries.is_empty() {
                self.cloud.retry = None;
            }
        }
    }

    pub(super) fn refresh_cloud_header(&mut self) {
        if self.cloud.active {
            let account = self
                .remote_account(&self.cloud.account)
                .map_or_else(|| t("files-cloud-title"), |account| account.label);
            let title = self
                .cloud
                .folder_label
                .as_ref()
                .map_or_else(|| account.clone(), |folder| format!("{account} / {folder}"));
            self.nav
                .set_content_header_leading(Some(ui::widget::Text::new(&title).into_widget()));
            self.nav.set_content_header("", vec![]);
            self.cloud.header_shown = true;
        } else if self.cloud.header_shown {
            let (leading, actions, toolbar) = crate::toolbar::FileToolbar::new(self.cmds.clone());
            self.toolbar = toolbar;
            self.update_toolbar();
            self.nav.set_content_header_leading(Some(Box::new(leading)));
            self.nav.set_content_header("", vec![Box::new(actions)]);
            self.cloud.header_shown = false;
        }
    }

    fn cloud_button(&self, id: &str, label: &str, effect: FilesEffect) -> Box<dyn Widget> {
        let sink = self.cmds.clone();
        Button::new(id, label)
            .on_effect(move || crate::command::push(&sink, FileCommand::Cloud(effect.clone())))
            .into_widget()
    }

    fn cloud_checkbox(&self, entry: &ImportEntry) -> Box<dyn Widget> {
        let checked = ui::LocalState::new(self.cloud.selected.contains(&entry.id));
        let sink = self.cmds.clone();
        let id = entry.id.clone();
        ui::widget::Checkbox::from_state(&checked)
            .with_label(&entry.name)
            .on_change(move |_| {
                crate::command::push(
                    &sink,
                    FileCommand::Cloud(FilesEffect::SelectCloud { entry: id.clone() }),
                );
            })
            .into_widget()
    }

    fn cloud_entries(&self, mut body: Column) -> Column {
        use ui::widget::Text;
        // The job's reason already summarizes entry failures; do not repeat
        // the same provider response both as an aggregate and as per-entry text.
        let mut seen = HashSet::new();
        let errors: Vec<&str> = if self.cloud.progress.failed.is_empty() {
            self.cloud.error.as_deref().into_iter().collect()
        } else {
            self.cloud
                .progress
                .failed
                .iter()
                .map(String::as_str)
                .collect()
        };
        for error in errors {
            let mut parts = error.split(": ").collect::<Vec<_>>();
            parts.dedup();
            let message = parts.join(": ");
            if seen.insert(message.clone()) {
                body = body.push(Text::new(&message).into_widget());
            }
        }

        if self.cloud.pending.is_some() {
            body = body.push(Text::new(&t("files-cloud-loading")).into_widget());
        }
        for entry in &self.cloud.entries {
            body = body.push(self.cloud_checkbox(entry));
            if entry.kind == ImportKind::NativeExport && !entry.content_writable {
                let hint = match Path::new(&entry.name)
                    .extension()
                    .and_then(|ext| ext.to_str())
                {
                    Some("xlsx") => "files-cloud-converted-sheet",
                    Some("pptx") => "files-cloud-converted-slides",
                    _ => "files-cloud-converted",
                };
                body = body.push(Text::new(&t(hint)).into_widget());
            }
            if entry.kind == ImportKind::Folder {
                body = body.push(self.cloud_button(
                    &format!("files.cloud.open.{}", hex::encode(&entry.id)),
                    &t("files-menu-open"),
                    FilesEffect::BrowseCloud {
                        account: self.cloud.account.clone(),
                        folder: Some(entry.id.clone()),
                    },
                ));
            }
        }
        body
    }

    pub(super) fn build_cloud_pane(&self, theme: &ui::Theme) -> Box<dyn Widget> {
        use ui::TextRole;
        use ui::widget::Text;
        let mut body = Column::new().with_gap(theme.tokens.spacing.s2);
        let label = self
            .remote_account(&self.cloud.account)
            .map_or_else(|| t("files-cloud-title"), |a| a.label);
        body = body
            .push(Text::new(&label).with_role(TextRole::Title).into_widget())
            .push(Text::new(&t("files-cloud-independent")).into_widget())
            .push(self.cloud_button(
                "files.cloud.root",
                &t("files-cloud-root"),
                FilesEffect::BrowseCloud {
                    account: self.cloud.account.clone(),
                    folder: None,
                },
            ));
        body = self.cloud_entries(body);
        if self.cloud.next.is_some() {
            body = body.push(self.cloud_button(
                "files.cloud.more",
                &t("files-cloud-more"),
                FilesEffect::MoreCloud,
            ));
        }
        body = body.push(self.cloud_button(
            "files.cloud.import",
            &t("files-cloud-import"),
            FilesEffect::ImportCloud { folder: None },
        ));
        if !self.cloud.progress.phase.is_empty() {
            body = body.push(
                Text::new(&t_args(
                    "files-cloud-progress",
                    &[
                        arg(
                            "phase",
                            t(match self.cloud.progress.phase.as_str() {
                                "enumerating" => "files-cloud-enumerating",
                                "exporting" => "files-cloud-exporting",
                                "copying" | "transferring" => "files-cloud-copying",
                                "done" => "files-cloud-done",
                                "cancelled" => "files-cloud-cancelled",
                                "failed" => "files-cloud-failed",
                                _ => "files-cloud-preparing",
                            }),
                        ),
                        arg(
                            "count",
                            i64::try_from(self.cloud.progress.completed).unwrap_or(i64::MAX),
                        ),
                        arg(
                            "bytes",
                            i64::try_from(self.cloud.progress.bytes).unwrap_or(i64::MAX),
                        ),
                    ],
                ))
                .into_widget(),
            );
        }
        if self.cloud_import_pending() {
            body = body
                .push(
                    ui::widget::ProgressBar::indeterminate()
                        .with_label(t("files-cloud-title"))
                        .into_widget(),
                )
                .push(self.cloud_button(
                    "files.cloud.cancel",
                    &t("files-cloud-cancel"),
                    FilesEffect::CancelCloud,
                ));
        }
        if !self.cloud_import_pending()
            && matches!(self.cloud.progress.phase.as_str(), "failed" | "cancelled")
            && self.cloud.retry.is_some()
        {
            body = body.push(self.cloud_button(
                "files.cloud.retry",
                &t("files-cloud-retry"),
                FilesEffect::RetryCloud,
            ));
        }
        if self.cloud.destination.is_some() {
            body = body.push(self.cloud_button(
                "files.cloud.reveal",
                &t("files-cloud-reveal"),
                FilesEffect::RevealCloud,
            ));
        }
        Scrollable::new(ScrollAxis::Vertical)
            .fill_parent_height()
            .with_child(body.into_widget())
            .into_widget()
    }

    pub(super) fn cloud_resources(&self) -> Vec<Resource> {
        if !self.cloud.active {
            return vec![];
        }
        self.cloud
            .entries
            .iter()
            .map(|entry| {
                let mut row = Resource::new(
                    format!("files.cloud-entry:{}", hex::encode(&entry.id)),
                    "files.cloud-entry",
                    entry.name.clone(),
                );
                row.actions.push(crate::effects::row_action(
                    &FilesEffect::SelectCloud {
                        entry: entry.id.clone(),
                    },
                    "select",
                    t("files-cloud-select"),
                ));
                if entry.kind == ImportKind::Folder {
                    row.actions.push(crate::effects::row_action(
                        &FilesEffect::BrowseCloud {
                            account: self.cloud.account.clone(),
                            folder: Some(entry.id.clone()),
                        },
                        "open",
                        t("files-menu-open"),
                    ));
                }
                row
            })
            .collect()
    }
}
