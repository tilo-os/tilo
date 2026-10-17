//! Private orchestration for explicit target grants. No transfer starts merely
//! because the person advances a setup page.
use std::path::PathBuf;

use imager_backend::journey_transfer;

#[derive(Default)]
pub(super) struct Transfer {
    review: Option<journey_transfer::TransferReview>,
    reviewing: Option<std::sync::mpsc::Receiver<Result<journey_transfer::TransferReview, String>>>,
    preparing_plan: Option<std::sync::mpsc::Receiver<Result<serde_json::Value, String>>>,
    worker: Option<journey_transfer::TransferWorker>,
    prepared_accounts: Option<std::sync::Arc<super::accounts::PreparedAccounts>>,
    merging_accounts: Option<std::sync::mpsc::Receiver<Result<usize, String>>>,
    accounts_invalidated: bool,
    categories: [bool; 3],
    state: &'static str,
    result: serde_json::Value,
}

impl Transfer {
    pub fn review(
        &mut self,
        source: PathBuf,
        destination: PathBuf,
        journals: PathBuf,
        categories: [bool; 3],
        refs: Vec<super::super::flatpak_transfer::InstalledRef>,
        prepared_accounts: Option<super::accounts::PreparedAccounts>,
    ) -> Result<(), &'static str> {
        journey_transfer::ensure_supported()?;
        if self.busy() {
            return Err("installer-preview-transfer-busy");
        }
        if categories[2] && prepared_accounts.is_none() {
            return Err("installer-preview-accounts-not-prepared");
        }
        self.categories = categories;
        self.accounts_invalidated = false;
        self.prepared_accounts = prepared_accounts.map(std::sync::Arc::new);
        let request = journey_transfer::TransferRequest {
            source_home: source,
            target_home: destination,
            private_journals: journals,
            copy_home: categories[0],
            copy_flatpak: categories[1],
            system_refs: if categories[1] { refs } else { Vec::new() },
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.review = None;
        self.reviewing = Some(rx);
        self.state = "reviewing";
        self.result = serde_json::Value::Null;
        std::thread::spawn(move || {
            let _ = tx.send(journey_transfer::review(request));
        });
        Ok(())
    }

    pub fn prepare_migration(
        &mut self,
        source: PathBuf,
        directory: PathBuf,
        operation: String,
        categories: [bool; 3],
        refs: Vec<super::super::flatpak_transfer::InstalledRef>,
    ) -> Result<(), &'static str> {
        journey_transfer::ensure_supported()?;
        if self.busy() {
            return Err("installer-preview-transfer-busy");
        }
        let (tx, receiver) = std::sync::mpsc::channel();
        self.preparing_plan = Some(receiver);
        self.review = None;
        self.state = "preparing-migration";
        self.result = serde_json::Value::Null;
        std::thread::spawn(move || {
            let result = journey_transfer::prepare_migration(
                &source, &directory, &operation, categories, refs,
            );
            let _ = tx.send(result);
        });
        Ok(())
    }

    pub fn review_installed(
        &mut self,
        directory: PathBuf,
        operation: String,
        destination: PathBuf,
        journals: PathBuf,
        categories: [bool; 3],
        accounts: Option<super::accounts::PreparedAccounts>,
        flatpak_inventory: Option<PathBuf>,
    ) -> Result<(), &'static str> {
        journey_transfer::ensure_supported()?;
        if self.busy() {
            return Err("installer-preview-transfer-busy");
        }
        if categories[2] && accounts.is_none() {
            return Err("installer-preview-accounts-not-prepared");
        }
        self.categories = categories;
        self.accounts_invalidated = false;
        self.prepared_accounts = accounts.map(std::sync::Arc::new);
        self.review = None;
        self.state = "reviewing";
        self.result = serde_json::Value::Null;
        let (tx, receiver) = std::sync::mpsc::channel();
        self.reviewing = Some(receiver);
        std::thread::spawn(move || {
            let result = journey_transfer::review_installed(
                &directory,
                &operation,
                &destination,
                &journals,
                categories,
                flatpak_inventory,
            );
            let _ = tx.send(result);
        });
        Ok(())
    }

    pub fn start(&mut self, retry: bool, categories: [bool; 3]) -> Result<(), &'static str> {
        journey_transfer::ensure_supported()?;
        if self.busy() {
            return Err("installer-preview-transfer-busy");
        }
        if self.categories != categories || self.accounts_invalidated {
            return Err("installer-preview-transfer-needs-review");
        }
        let review = self
            .review
            .as_ref()
            .ok_or("installer-preview-transfer-needs-review")?;
        self.worker = Some(
            review
                .start(retry || self.state == "reviewed" && review.recovered())
                .map_err(|_| "installer-preview-transfer-start-failed")?,
        );
        self.state = "copying";
        Ok(())
    }

    pub fn invalidate_accounts(&mut self) {
        self.accounts_invalidated = true;
    }

    pub fn cancel(&mut self) -> Result<(), &'static str> {
        if self.worker.as_ref().is_some_and(|worker| worker.cancel()) {
            self.state = "cancelling";
            return Ok(());
        }
        Err("installer-preview-transfer-not-running")
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(receiver) = &self.preparing_plan {
            let result = match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Some(Err("Migration preparation stopped unexpectedly.".into()))
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
            };
            if let Some(result) = result {
                self.preparing_plan = None;
                self.state = if result.is_ok() {
                    "migration-prepared"
                } else {
                    "failed"
                };
                self.result = result.unwrap_or_else(|error| serde_json::json!({"error":error}));
                changed = true;
            }
        }
        if let Some(receiver) = &self.reviewing {
            match receiver.try_recv() {
                Ok(Ok(review)) => {
                    self.result = review.projection();
                    self.review = Some(review);
                    self.reviewing = None;
                    self.state = "reviewed";
                    changed = true;
                }
                Ok(Err(error)) => {
                    self.result = serde_json::json!({"error":error});
                    self.reviewing = None;
                    self.state = "failed";
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.reviewing = None;
                    self.state = "failed";
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        while let Some(event) = self.worker.as_mut().and_then(|worker| worker.try_next()) {
            use journey_transfer::TransferEvent;
            changed = true;
            match event {
                TransferEvent::CopyProgress(progress) => {
                    self.result = progress;
                }
                TransferEvent::RestoringApplications => self.state = "restoring-applications",
                TransferEvent::Finished { state, result } => {
                    self.state = state;
                    self.result = result;
                    self.worker = None;
                    if self.state == "complete" && self.categories[2] {
                        if let (Some(accounts), Some(review)) =
                            (&self.prepared_accounts, &self.review)
                        {
                            let accounts = std::sync::Arc::clone(accounts);
                            let target = review.target_home().to_path_buf();
                            let (tx, receiver) = std::sync::mpsc::channel();
                            self.merging_accounts = Some(receiver);
                            self.state = "connecting-accounts";
                            std::thread::spawn(move || {
                                let result = accounts.merge_into(&target).map(|()| accounts.count())
                                    .map_err(|_| "Prepared accounts could not be delivered to the destination.".to_string());
                                let _ = tx.send(result);
                            });
                        }
                    }
                }
            }
        }
        if let Some(receiver) = &self.merging_accounts {
            let outcome = match receiver.try_recv() {
                Ok(outcome) => Some(outcome),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Some(Err("Account delivery stopped unexpectedly.".into()))
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
            };
            if let Some(outcome) = outcome {
                self.merging_accounts = None;
                self.state = if outcome.is_ok() {
                    "complete"
                } else {
                    "incomplete"
                };
                self.result["accounts"] = match outcome {
                    Ok(count) => serde_json::json!({"connected":count}),
                    Err(error) => serde_json::json!({"error":error}),
                };
                changed = true;
            }
        }
        changed
    }

    pub fn busy(&self) -> bool {
        self.preparing_plan.is_some()
            || self.reviewing.is_some()
            || self.worker.is_some()
            || self.merging_accounts.is_some()
    }

    pub fn completed_home(&self) -> Option<journey_transfer::TransferReview> {
        (self.state == "complete" && self.categories[0])
            .then(|| self.review.clone())
            .flatten()
    }

    pub fn status(&self) -> serde_json::Value {
        serde_json::json!({"state": if self.state.is_empty() { "idle" } else { self.state }, "result":self.result})
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{Transfer, journey_transfer};
    use std::{
        ffi::{CString, OsStr},
        os::unix::{ffi::OsStrExt, fs::PermissionsExt},
        time::{Duration, Instant},
    };

    fn wait(transfer: &mut Transfer) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while transfer.busy() {
            assert!(Instant::now() < deadline, "{}", transfer.status());
            transfer.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn requested_accounts_without_custody_refuse_both_public_review_effects() {
        use super::super::{InstallerPreview, InstallerPreviewEffect};

        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let original = source.join("document");
        std::fs::write(&original, b"original remains unchanged").unwrap();
        let target = temp.path().join("target");
        let journals = temp.path().join("journals");
        let plans = temp.path().join("plans");
        let mut preview = InstallerPreview {
            live: true,
            ..Default::default()
        };
        preview.journey.migrate = Some(true);
        preview.journey.categories = [true, false, true];

        let effects = [
            InstallerPreviewEffect::ReviewUserTransfer {
                source: source.to_str().unwrap().into(),
                destination: target.to_str().unwrap().into(),
                journals: journals.to_str().unwrap().into(),
            },
            InstallerPreviewEffect::ReviewInstalledTransfer {
                private_plans: plans.to_str().unwrap().into(),
                operation: "requested-account-custody".into(),
                destination: target.to_str().unwrap().into(),
                journals: journals.to_str().unwrap().into(),
                flatpak_inventory: None,
            },
        ];
        for effect in effects {
            assert_eq!(
                preview.apply(effect),
                Err("installer-preview-accounts-not-prepared")
            );
            assert!(!preview.user_transfer.busy());
            assert_eq!(preview.user_transfer.status()["state"], "idle");
            assert_eq!(
                preview.apply(InstallerPreviewEffect::StartUserTransfer),
                Err("installer-preview-transfer-needs-review")
            );
            assert!(!target.exists());
            assert!(!journals.exists());
            assert!(!plans.exists());
            assert_eq!(
                std::fs::read(&original).unwrap(),
                b"original remains unchanged"
            );
        }
    }

    #[test]
    fn actual_transfer_projects_non_utf8_target_and_issue_paths_losslessly() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("source");
        let journals = root.join("journals");
        for path in [&source, &journals] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let name = OsStr::from_bytes(b"pipe-\xff");
        let fifo = source.join(name);
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        std::fs::write(source.join(OsStr::from_bytes(b"notes-\xfe")), b"notes").unwrap();
        let target = root.join(OsStr::from_bytes(b"target-\xfd"));
        let mut transfer = Transfer::default();
        transfer
            .review(
                source,
                target.clone(),
                journals,
                [true, false, false],
                vec![],
                None,
            )
            .unwrap();
        wait(&mut transfer);
        let reviewed = transfer.status();
        assert_eq!(reviewed["state"], "reviewed", "{reviewed}");
        let native: super::super::super::migration_store::NativePath =
            serde_json::from_value(reviewed["result"]["target"]["native"].clone()).unwrap();
        assert_eq!(native.decode().unwrap(), target);
        transfer.start(false, [true, false, false]).unwrap();
        wait(&mut transfer);
        let finished = transfer.status();
        assert_eq!(finished["state"], "incomplete", "{finished}");
        let skipped = finished["result"]["copies"][0]["skipped"]
            .as_array()
            .unwrap();
        assert_eq!(skipped.len(), 1, "{finished}");
        let native: super::super::super::migration_store::NativePath =
            serde_json::from_value(skipped[0]["path"]["native"].clone()).unwrap();
        assert_eq!(native.decode().unwrap(), std::path::Path::new(name));
        assert_eq!(
            std::fs::read(target.join(OsStr::from_bytes(b"notes-\xfe"))).unwrap(),
            b"notes"
        );
    }

    #[test]
    fn flatpak_inventory_reader_refuses_fifo_promptly_and_never_follows_links() {
        use std::os::unix::fs::{OpenOptionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let fifo = root.join("inventory.fifo");
        let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path inside an isolated fixture.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        // A regressed blocking open is released by a delayed fixture writer,
        // making the test fail its latency assertion instead of hanging Cargo.
        let writer_path = fifo.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(writer_path);
        });
        let started = Instant::now();
        assert!(journey_transfer::read_flatpak_inventory(&fifo).is_err());
        let elapsed = started.elapsed();
        writer.join().unwrap();
        assert!(
            elapsed < Duration::from_millis(250),
            "A FIFO must be rejected without waiting for a writer: {elapsed:?}"
        );
        let original = root.join("original");
        std::fs::write(&original, b"unchanged private metadata fixture").unwrap();
        std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = root.join("inventory-link");
        symlink(&original, &link).unwrap();
        assert!(journey_transfer::read_flatpak_inventory(&link).is_err());
        assert_eq!(
            std::fs::read(original).unwrap(),
            b"unchanged private metadata fixture"
        );
    }
}
