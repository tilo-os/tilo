//! A clean mount stays absent between preparation and an acknowledged metadata commit.
//! Preparation expires, but begin-commit durably pins the hold before acknowledging
//! the writer. Neither expiry, account watchers nor restart may release a pinned
//! hold. Completion verifies the saved mode; explicit recovery takes the writer
//! fence before restoring Sync or consuming an already saved Import change.
//! The service loop owns holds; stopping a FUSE process never blocks that loop.

use super::{
    AccountId, AccountStore, CloudResponse, CloudService, Duration, Instant, MountReply, Sender,
    Signal, ToMount, fs, providers,
};

const HOLD_LIFETIME: Duration = Duration::from_secs(120);

#[cfg(test)]
#[path = "import_commit_tests.rs"]
mod commit_tests;
#[cfg(test)]
#[path = "import_transition_tests.rs"]
mod tests;

pub(super) struct ImportHold {
    account: accounts::Account,
    token: String,
    request: u64,
    reply: Option<Sender<CloudResponse>>,
    phase: Phase,
    deadline: Instant,
    cancelled: bool,
    orphan_reported: bool,
}

#[derive(PartialEq, Eq)]
enum Phase {
    Freezing,
    Stopping,
    Ready,
    Committing,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CommitRecord {
    account: accounts::Account,
    token: String,
}

const JOURNAL: &str = "import-commits.json";

fn hold_deadline() -> Instant {
    let now = Instant::now();
    now.checked_add(HOLD_LIFETIME).unwrap_or(now)
}

fn generation(account: &accounts::Account) -> String {
    // Account config is explicitly public. The vault and handoff are never read.
    let bytes = serde_json::to_vec(account).unwrap_or_default();
    blake3::hash(&bytes).to_hex().to_string()
}

fn committed(before: &accounts::Account, after: &accounts::Account) -> bool {
    let mut expected = before.clone();
    accounts::cloud_types::set_files_mode(
        &mut expected.config,
        accounts::cloud_types::FilesMode::Import,
    );
    expected == *after
}

impl CloudService {
    pub(super) fn restore_import_commits(&mut self) {
        let result = (|| -> Result<Vec<CommitRecord>, String> {
            let bytes = match fs::read(self.config.state_root.join(JOURNAL)) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(error) => return Err(error.to_string()),
            };
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())
        })();
        match result {
            Ok(records) => {
                for record in records {
                    let _ = self.reporter(record.account.id.as_str()).failed();
                    tracing::warn!(account = %record.account.id, "interrupted cloud mode change requires explicit recovery");
                    self.import_holds.insert(
                        record.account.id.as_str().to_owned(),
                        ImportHold {
                            account: record.account,
                            token: record.token,
                            request: 0,
                            reply: None,
                            phase: Phase::Committing,
                            deadline: hold_deadline(),
                            cancelled: false,
                            orphan_reported: true,
                        },
                    );
                }
            }
            Err(error) => {
                tracing::error!(%error, "cannot recover cloud mode changes; writable mounts are blocked");
                self.transition_error =
                    Some(format!("Cannot read the mode-change journal: {error}"));
            }
        }
    }

    fn save_import_commits(&self, excluding: Option<&str>) -> Result<(), String> {
        let records: Vec<_> = self
            .import_holds
            .iter()
            .filter(|(id, hold)| Some(id.as_str()) != excluding && hold.phase == Phase::Committing)
            .map(|(_, hold)| CommitRecord {
                account: hold.account.clone(),
                token: hold.token.clone(),
            })
            .collect();
        let bytes = serde_json::to_vec(&records).map_err(|error| error.to_string())?;
        tilo_files::native::atomic::write_creating_parent_with_mode(
            &self.config.state_root.join(JOURNAL),
            &bytes,
            0o600,
        )
        .map_err(|error| format!("Cannot persist the mode-change journal: {error}"))
    }

    pub(super) fn begin_commit_import(&mut self, account: &str, token: &str) -> CloudResponse {
        self.expire_import_holds();
        let store = self.store();
        let Some(hold) = self.import_holds.get_mut(account) else {
            return CloudResponse::Error(
                "Import preparation expired; retry the mode change".into(),
            );
        };
        if hold.token != token
            || hold.phase != Phase::Ready
            || store
                .get(&AccountId::from(account))
                .is_none_or(|current| generation(current) != generation(&hold.account))
        {
            return CloudResponse::Error(
                "Import token or account configuration changed; retry".into(),
            );
        }
        hold.phase = Phase::Committing;
        if let Err(error) = self.save_import_commits(None) {
            // Publication may have succeeded before a durability error. Keep
            // the hold pinned until abort/recovery can durably clear it.
            return CloudResponse::Error(error);
        }
        CloudResponse::Done
    }

    pub(super) fn complete_import(&mut self, account: &str, token: &str) -> CloudResponse {
        let store = self.store();
        let valid = self.import_holds.get(account).is_some_and(|hold| {
            hold.token == token
                && hold.phase == Phase::Committing
                && store.get(&AccountId::from(account)).is_some_and(|current| {
                    current.type_id == hold.account.type_id
                        && accounts::cloud_types::files_mode(current)
                            == Some(accounts::cloud_types::FilesMode::Import)
                })
        });
        if !valid {
            return CloudResponse::Error("Import was not finalized: token or saved mode changed. Close this form and recover the mode change.".into());
        }
        if let Err(error) = self.save_import_commits(Some(account)) {
            return CloudResponse::Error(error);
        }
        self.import_holds.remove(account);
        let _ = self.reporter(account).succeeded();
        self.reconcile();
        CloudResponse::Done
    }

    pub(super) fn recover_import(&mut self, account: &str) -> CloudResponse {
        let root = self
            .config
            .config_root
            .clone()
            .or_else(accounts::config_root);
        let Some(root) = root else {
            return CloudResponse::Error("No account configuration directory".into());
        };
        let _writer = match runtime_fs::cloud::import_writer_lock(&root, account) {
            Ok(lock) => lock,
            Err(error) => {
                return CloudResponse::Error(format!("The account is still being saved: {error}"));
            }
        };
        let Some(hold) = self.import_holds.get(account) else {
            return CloudResponse::Error("No interrupted mode change for this account".into());
        };
        if hold.phase != Phase::Committing {
            return CloudResponse::Error(
                "Preparation is still active; cancel it or wait for expiry before recovery".into(),
            );
        }
        let token = hold.token.clone();
        let store = self.store();
        if store.get(&AccountId::from(account)).is_some_and(|current| {
            accounts::cloud_types::files_mode(current)
                == Some(accounts::cloud_types::FilesMode::Import)
        }) {
            self.complete_import(account, &token)
        } else {
            // Explicit recovery owns the writer fence. Even an external config
            // edit can now be reconciled safely: the old mount was clean and
            // its cache and upload queue are never deleted here.
            if let Err(error) = self.save_import_commits(Some(account)) {
                return CloudResponse::Error(error);
            }
            self.import_holds.remove(account);
            self.reconcile();
            CloudResponse::Done
        }
    }

    pub(super) fn prepare_import(&mut self, account: &str, reply: Sender<CloudResponse>) {
        self.expire_import_holds();
        if let Some(error) = &self.transition_error {
            let _ = reply.send(CloudResponse::Error(error.clone()));
            return;
        }
        if self.import_holds.contains_key(account) {
            let _ = reply.send(CloudResponse::Error(
                "an import preparation is already active; close the connection form and recover the interrupted mode change before retrying".into(),
            ));
            return;
        }
        let store = self.store();
        let Some(config) = store.get(&AccountId::from(account)).cloned() else {
            let _ = reply.send(CloudResponse::Error("no such account".into()));
            return;
        };
        if !self
            .mounts
            .get(account)
            .is_some_and(|mounted| providers::spec_for(&config).as_ref() == Some(&mounted.spec))
        {
            let _ = reply.send(CloudResponse::Error(
                "the sync account must be mounted before preparing import".into(),
            ));
            return;
        }
        let request = self.next_request;
        self.next_request = self.next_request.wrapping_add(1);
        let hold = ImportHold {
            token: format!("{}-{}", generation(&config), uuid::Uuid::new_v4().simple()),
            account: config,
            request,
            reply: Some(reply),
            phase: Phase::Freezing,
            deadline: hold_deadline(),
            cancelled: false,
            orphan_reported: false,
        };
        self.import_holds.insert(account.to_owned(), hold);
        if let Err(error) = self.send_to(account, &ToMount::PrepareImport { id: request }) {
            self.fail_import_hold(account, error);
        }
    }

    pub(super) fn fail_import_hold(&mut self, account: &str, error: String) {
        if let Some(mut hold) = self.import_holds.remove(account)
            && let Some(reply) = hold.reply.take()
        {
            let _ = reply.send(CloudResponse::Error(error));
        }
    }

    pub(super) fn import_freeze_reply(&mut self, account: &str, reply: &MountReply) -> bool {
        let Some(hold) = self.import_holds.get(account) else {
            return false;
        };
        if hold.request != reply.id || hold.phase != Phase::Freezing {
            return false;
        }
        if !matches!(
            &reply.result,
            Ok(runtime_fs::cloud::FileReply::ImportFrozen)
        ) {
            self.fail_import_hold(
                account,
                reply
                    .result
                    .as_ref()
                    .err()
                    .cloned()
                    .unwrap_or_else(|| "unexpected freeze response".into()),
            );
            self.reconcile();
            return true;
        }
        self.stop_import_mount(account);
        true
    }

    fn stop_import_mount(&mut self, account: &str) {
        let Some(mounted) = self.mounts.remove(account) else {
            self.fail_import_hold(account, "the mount stopped during preparation".into());
            return;
        };
        if let Some(hold) = self.import_holds.get_mut(account) {
            hold.phase = Phase::Stopping;
        }
        self.drop_pending(account);
        let signals = self.signals.clone();
        let id = account.to_owned();
        let directory = self.config.mounts_root.join(&mounted.dir_name);
        std::thread::spawn(move || {
            let result = mounted
                .running
                .stop()
                .and_then(|()| fs::remove_dir(directory))
                .map_err(|error| error.to_string());
            // Only remove the empty mount point; cache, queue and user data remain.
            let _ = signals.send(Signal::ImportStopped(id, result));
        });
    }

    pub(super) fn import_stopped(&mut self, account: &str, result: Result<(), String>) {
        if let Err(error) = result {
            self.fail_import_hold(
                account,
                format!("the mount could not stop cleanly: {error}"),
            );
            self.reconcile();
            return;
        }
        let store = self.store();
        let Some(hold) = self.import_holds.get_mut(account) else {
            return;
        };
        hold.phase = Phase::Ready;
        let current = store.get(&AccountId::from(account));
        let unchanged =
            current.is_some_and(|config| generation(config) == generation(&hold.account));
        if hold.cancelled || !unchanged || Instant::now() >= hold.deadline {
            self.fail_import_hold(
                account,
                "the account changed or preparation expired; try again".into(),
            );
            self.reconcile();
            return;
        }
        hold.deadline = hold_deadline();
        if let Some(reply) = hold.reply.take() {
            let _ = reply.send(CloudResponse::ImportPrepared {
                token: hold.token.clone(),
            });
        }
    }

    pub(super) fn abort_import(&mut self, account: &str, token: &str) -> CloudResponse {
        let store = self.store();
        let valid = self.import_holds.get(account).is_some_and(|hold| {
            (hold.phase == Phase::Committing
                || (hold.phase == Phase::Ready && Instant::now() < hold.deadline))
                && hold.token == token
                && store
                    .get(&AccountId::from(account))
                    .is_some_and(|config| generation(config) == generation(&hold.account))
        });
        if !valid {
            self.expire_import_holds();
            return CloudResponse::Error(
                "the import token is expired or does not match this account configuration".into(),
            );
        }
        if let Err(error) = self.save_import_commits(Some(account)) {
            return CloudResponse::Error(error);
        }
        self.import_holds.remove(account);
        // The persistent pause set was never changed, so restarting restores it.
        self.reconcile();
        CloudResponse::Done
    }

    /// Returns whether an unpinned ready hold was released (and may need a remount).
    pub(super) fn reconcile_import_holds(&mut self, store: &AccountStore) -> bool {
        let mut released = Vec::new();
        let mut stopping = Vec::new();
        for (id, hold) in &mut self.import_holds {
            if hold.phase == Phase::Committing {
                // Never infer completion or remount from a timer or watcher.
                continue;
            }
            let current = store.get(&AccountId::from(id.as_str()));
            let unchanged =
                current.is_some_and(|config| generation(config) == generation(&hold.account));
            let is_committed = current.is_some_and(|config| committed(&hold.account, config));
            if is_committed || !unchanged || Instant::now() >= hold.deadline {
                if hold.phase == Phase::Ready {
                    released.push(id.clone());
                } else {
                    // Never remount while an old child can still hold the cache lock.
                    hold.cancelled = true;
                    if hold.phase == Phase::Freezing {
                        stopping.push(id.clone());
                    }
                }
            }
        }
        for id in stopping {
            self.stop_import_mount(&id);
        }
        for id in &released {
            self.fail_import_hold(
                id,
                "import preparation expired or configuration changed".into(),
            );
        }
        !released.is_empty()
    }

    pub(super) fn expire_import_holds(&mut self) {
        // A vanished Settings writer is an actionable error, not permission to
        // remount. The account row exposes explicit recovery even before the
        // daemon restarts; a surviving writer keeps its fence across restart.
        if let Some(root) = self
            .config
            .config_root
            .clone()
            .or_else(accounts::config_root)
        {
            let mut interrupted = Vec::new();
            for (id, hold) in &mut self.import_holds {
                if hold.phase == Phase::Committing
                    && !hold.orphan_reported
                    && let Ok(_writer) = runtime_fs::cloud::import_writer_lock(&root, id)
                {
                    hold.orphan_reported = true;
                    interrupted.push(id.clone());
                }
            }
            for id in interrupted {
                let _ = self.reporter(&id).failed();
            }
        }
        let store = self.store();
        if self.reconcile_import_holds(&store) {
            self.reconcile();
        }
    }
}
