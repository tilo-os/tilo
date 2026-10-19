//! Device-local action backend. The journey supplies its reviewed sources and
//! chosen destination; this module never chooses a location or launches an app.
//! Only identifiers, public consent URLs and credential-free receipts leave it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use account_custody::{Authorization, Custodian, OAuthCloud};
use accounts::AccountId;
use serde::Serialize;

use super::{Candidate, ConnectionError, Connector, Inventory, SourceRoots};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionProvider {
    GoogleDrive,
    OneDrive,
    Dropbox,
    Spotify,
    SpotifyStreaming,
}

impl ConnectionProvider {
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "google-drive" => Some(Self::GoogleDrive),
            "onedrive" => Some(Self::OneDrive),
            "dropbox" => Some(Self::Dropbox),
            "spotify" => Some(Self::Spotify),
            "spotify-streaming" => Some(Self::SpotifyStreaming),
            _ => None,
        }
    }
    fn cloud(self) -> Option<OAuthCloud> {
        match self {
            Self::GoogleDrive => Some(OAuthCloud::GoogleDrive),
            Self::OneDrive => Some(OAuthCloud::OneDrive),
            Self::Dropbox => Some(OAuthCloud::Dropbox),
            Self::Spotify | Self::SpotifyStreaming => None,
        }
    }
    fn accepts(self, candidate: &Candidate) -> bool {
        candidate.provider
            == match self {
                Self::GoogleDrive => super::Provider::Google,
                Self::OneDrive => super::Provider::Microsoft,
                Self::Dropbox => super::Provider::Dropbox,
                Self::Spotify | Self::SpotifyStreaming => super::Provider::Spotify,
            }
    }
}

/// Private worker input; never serialized or formatted into receipts.
pub struct DavConnection {
    pub nextcloud: bool,
    pub label: String,
    pub server: String,
    pub username: String,
    pub password: zeroize::Zeroizing<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionError {
    UnknownSelection,
    WrongProvider,
    TargetUnavailable,
    TargetRedirected,
    TargetOverlapsSource,
    TargetNotPrivate,
    SourceChanged,
    Busy,
    UnknownConnection,
    ConnectionFailed,
    WorkerLost,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConsentLink {
    pub connection_id: String,
    pub url: String,
    pub needs_pasted_code: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum AccountJobState {
    Running,
    Completed { account_id: String },
    Failed { reason: ConnectionError },
}

#[derive(Clone, Debug, Serialize)]
pub struct AccountJob {
    pub id: String,
    pub state: AccountJobState,
}

struct PendingConnection {
    provider: ConnectionProvider,
    label: String,
    home: PathBuf,
    connector: Connector,
    authorization: Authorization,
}

struct Work {
    receipt: AccountJob,
    result: Option<Receiver<Result<AccountId, ConnectionError>>>,
    cancel: Option<WorkCancel>,
    cleanup: Option<Connector>,
}

struct WorkCancel {
    loopback: Option<account_custody::LoopbackCancel>,
    nextcloud: Option<std::sync::mpsc::Sender<()>>,
    requested: Arc<AtomicBool>,
}
impl WorkCancel {
    fn cancel(&self) {
        self.requested.store(true, Ordering::SeqCst);
        if let Some(cancel) = &self.nextcloud {
            let _ = cancel.send(());
        }
        if let Some(loopback) = &self.loopback {
            loopback.cancel();
        }
    }
}

/// Immutable source-bound destination custody for async prepared handoff.
#[derive(Clone)]
pub struct DestinationGuard {
    boundary: Option<SourceBoundary>,
    reviewed: bool,
}
impl DestinationGuard {
    pub fn validate_destination(&self, home: &Path) -> Result<PathBuf, ActionError> {
        let root = target_root(home)?;
        if self.reviewed {
            self.boundary
                .as_ref()
                .ok_or(ActionError::SourceChanged)?
                .check_target(home)?;
        }
        Ok(root)
    }
}

/// Owned by the installer's local surface, never its resource document.
#[derive(Default)]
pub struct AccountActions {
    sources: Option<(SourceRoots, Inventory)>,
    boundary: Option<SourceBoundary>,
    pending: BTreeMap<String, PendingConnection>,
    jobs: BTreeMap<String, Work>,
    nextcloud_urls: BTreeMap<String, Receiver<String>>,
    custodian: Option<Custodian>,
    #[cfg(any(test, feature = "test-support"))]
    spotify_api: Option<String>,
    staging_lease: Option<std::sync::Arc<tempfile::TempDir>>,
}

impl AccountActions {
    pub fn reviewed(roots: SourceRoots, inventory: Inventory) -> Self {
        let boundary = SourceBoundary::capture(&roots).ok();
        Self {
            sources: Some((roots, inventory)),
            boundary,
            pending: BTreeMap::new(),
            jobs: BTreeMap::new(),
            nextcloud_urls: BTreeMap::new(),
            custodian: None,
            #[cfg(any(test, feature = "test-support"))]
            spotify_api: None,
            staging_lease: None,
        }
    }

    /// Preserve the original pinned destination boundary for a retained
    /// handoff, without copying pending authorization or worker state.
    pub fn destination_snapshot(&self) -> DestinationGuard {
        DestinationGuard {
            boundary: self.boundary.clone(),
            reviewed: self.sources.is_some(),
        }
    }

    /// Provider injection retains the real PKCE/custody path in fixture tests.
    #[must_use]
    pub fn with_custodian(mut self, custodian: Custodian) -> Self {
        self.custodian = Some(custodian);
        self
    }

    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn with_spotify_api(mut self, endpoint: String) -> Self {
        self.spotify_api = Some(endpoint);
        self
    }

    /// Create and retain owner-only staging before any credential work begins.
    ///
    /// # Errors
    /// Returns an error if a private temporary directory cannot be created.
    pub fn create_staging(&mut self) -> std::io::Result<std::sync::Arc<tempfile::TempDir>> {
        let mut builder = tempfile::Builder::new();
        builder.prefix("tilo-prepared-accounts-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let stage = std::sync::Arc::new(builder.tempdir()?);
        self.retain_staging(stage.clone());
        Ok(stage)
    }

    /// Retain a private prepared home until every worker using it exits.
    pub fn retain_staging(&mut self, stage: std::sync::Arc<tempfile::TempDir>) {
        self.staging_lease = Some(stage);
    }

    pub fn connection_available(&self, provider: ConnectionProvider) -> bool {
        let live = Custodian::live();
        let custodian = self.custodian.as_ref().unwrap_or(&live);
        if let Some(cloud) = provider.cloud() {
            custodian.is_configured(cloud.oauth_use())
        } else {
            let playback = provider == ConnectionProvider::SpotifyStreaming;
            custodian
                .use_config(account_custody::OAuthUse::new(
                    account_custody::OAuthIssuer::Spotify,
                    if playback {
                        account_custody::Capability::MusicStreaming
                    } else {
                        account_custody::Capability::Music
                    },
                ))
                .is_ok_and(|(configured, _)| {
                    [
                        "user-read-private",
                        if playback {
                            "streaming"
                        } else {
                            "user-library-read"
                        },
                    ]
                    .iter()
                    .all(|scope| {
                        configured
                            .scopes
                            .iter()
                            .any(|value| value.as_str() == *scope)
                    })
                })
        }
    }

    /// Validate a private installed/staged destination without writing there.
    pub fn validate_destination(&self, home: &Path) -> Result<PathBuf, ActionError> {
        let root = target_root(home)?;
        if self.sources.is_some() {
            self.boundary
                .as_ref()
                .ok_or(ActionError::SourceChanged)?
                .check_target(home)?;
        }
        Ok(root)
    }

    pub fn busy(&mut self) -> bool {
        self.poll();
        !self.pending.is_empty() || self.jobs.values().any(|job| job.result.is_some())
    }

    pub fn update_sources(
        &mut self,
        roots: SourceRoots,
        inventory: Inventory,
    ) -> Result<(), ActionError> {
        if self.busy() {
            return Err(ActionError::Busy);
        }
        let boundary = SourceBoundary::capture(&roots)?;
        self.sources = Some((roots, inventory));
        self.boundary = Some(boundary);
        Ok(())
    }

    pub fn connector(&self, home: &Path) -> Result<Connector, ActionError> {
        let root = target_root(home)?;
        if self.sources.is_some() {
            self.boundary
                .as_ref()
                .ok_or(ActionError::SourceChanged)?
                .check_target(home)?;
        }
        let connector = self.custodian.as_ref().map_or_else(
            || Connector::new(root.clone()),
            |custodian| Connector::with_custodian(root.clone(), custodian.clone()),
        );
        #[cfg(any(test, feature = "test-support"))]
        let connector = Connector {
            spotify_api: self.spotify_api.clone(),
            ..connector
        };
        Ok(connector)
    }

    fn available(&mut self) -> Result<(), ActionError> {
        if self.busy() {
            Err(ActionError::Busy)
        } else {
            Ok(())
        }
    }

    /// Explicit action on one reviewed source ID. Invoke only after home data
    /// transfer has settled, so this vault write cannot race the copy worker.
    pub fn import_selected(
        &mut self,
        candidate_id: &str,
        target_home: &Path,
    ) -> Result<String, ActionError> {
        self.available()?;
        let (roots, inventory) = self.sources.as_ref().ok_or(ActionError::UnknownSelection)?;
        let selected = inventory
            .candidates
            .iter()
            .find(|candidate| candidate.id == candidate_id)
            .filter(|candidate| candidate.can_import_credentials)
            .cloned()
            .ok_or(ActionError::UnknownSelection)?;
        let roots = roots.clone();
        let connector = self.connector(target_home)?;
        Ok(self.spawn(move || {
            #[cfg(target_os = "linux")]
            if selected.origin == super::Origin::Nextcloud {
                return connector.import_nextcloud(&roots, &selected);
            }
            #[cfg(target_os = "linux")]
            if selected.origin == super::Origin::Gnome {
                return connector.import_goa(&roots, &selected);
            }
            connector.import_rclone(&roots, &selected)
        }))
    }

    /// Explicit Nextcloud Login Flow v2. Blocking HTTP and private polling
    /// tokens remain on the worker; only the consent URL can reach the opener.
    pub fn connect_nextcloud_browser(
        &mut self,
        home: &Path,
        server: &str,
        label: &str,
    ) -> Result<String, ActionError> {
        self.available()?;
        let connector = self.connector(home)?;
        let server = server.to_owned();
        let label = label.to_owned();
        let (send_url, urls) = std::sync::mpsc::channel();
        let (cancel_sender, cancel_receiver) = std::sync::mpsc::channel();
        let requested = Arc::new(AtomicBool::new(false));
        let cancelled = requested.clone();
        let cleanup = self.staging_lease.as_ref().map(|_| connector.clone());
        let job = self.spawn(move || {
            if cancelled.load(Ordering::SeqCst) {
                return Err(ConnectionError::SignInFailed);
            }
            let flow = account_custody::nextcloud::begin(&server)
                .map_err(|_| ConnectionError::SignInFailed)?;
            if cancelled.load(Ordering::SeqCst)
                || send_url.send(flow.login_url().to_owned()).is_err()
            {
                return Err(ConnectionError::SignInFailed);
            }
            let login = flow
                .wait(
                    &cancel_receiver,
                    account_custody::nextcloud::POLL_INTERVAL,
                    account_custody::nextcloud::FLOW_LIFETIME,
                )
                .map_err(|_| ConnectionError::SignInFailed)?;
            // Always retain canonical custody of a returned new grant before
            // compensating cancellation, including receiver/controller drop.
            let grant_server = login.server.clone();
            let grant_login = login.login_name.clone();
            let grant_password = login.app_password.clone();
            let result = connector.finish_nextcloud(&label, login);
            if result.is_err() {
                let _ = account_custody::nextcloud::revoke_app_password(
                    &grant_server,
                    &grant_login,
                    &grant_password,
                );
            }

            if let Ok(id) = &result
                && cancelled.load(Ordering::SeqCst)
                && connector.disconnect(id).is_ok()
            {
                return Err(ConnectionError::SignInFailed);
            }
            result
        });
        self.nextcloud_urls.insert(job.clone(), urls);
        if let Some(work) = self.jobs.get_mut(&job) {
            work.cancel = Some(WorkCancel {
                loopback: None,
                nextcloud: Some(cancel_sender),
                requested,
            });
            work.cleanup = cleanup;
        }
        Ok(job)
    }
    /// Device-local consent URL, never a polling token or credential.
    pub fn take_nextcloud_url(&mut self, job: &str) -> Option<String> {
        let result = self.nextcloud_urls.get(job)?.try_recv();
        match result {
            Ok(url) => {
                self.nextcloud_urls.remove(job);
                Some(url)
            }
            Err(TryRecvError::Disconnected) => {
                self.nextcloud_urls.remove(job);
                None
            }
            Err(TryRecvError::Empty) => None,
        }
    }

    /// Save a caller-consented DAV app password in private target custody.
    pub fn connect_dav(
        &mut self,
        home: &Path,
        connection: DavConnection,
    ) -> Result<String, ActionError> {
        self.available()?;
        let connector = self.connector(home)?;
        Ok(self.spawn(move || {
            if connection.nextcloud {
                connector.connect_nextcloud_app_password(
                    &connection.label,
                    account_custody::nextcloud::NextcloudLogin {
                        server: connection.server,
                        login_name: connection.username,
                        app_password: connection.password,
                    },
                )
            } else {
                connector.connect_webdav(
                    &connection.label,
                    &connection.server,
                    &connection.username,
                    connection.password,
                )
            }
        }))
    }

    /// Explicit local-account selection; Connector verifies ownership and
    /// availability, and preserves any unrelated target entry.
    pub fn import_tilo(
        &mut self,
        account_id: &str,
        target_home: &Path,
    ) -> Result<String, ActionError> {
        self.available()?;
        let (roots, _) = self.sources.as_ref().ok_or(ActionError::UnknownSelection)?;
        let source = roots.config.join("tilo");
        let id = AccountId::new(account_id);
        let connector = self.connector(target_home)?;
        Ok(self.spawn(move || connector.import_tilo(&source, &id)))
    }

    pub fn import_reviewed_tilo(
        &mut self,
        source_id: &str,
        target_home: &Path,
    ) -> Result<String, ActionError> {
        self.available()?;
        let (roots, inventory) = self.sources.as_ref().ok_or(ActionError::UnknownSelection)?;
        let selected = inventory
            .tilo_accounts
            .iter()
            .find(|candidate| candidate.source_id == source_id)
            .cloned()
            .ok_or(ActionError::UnknownSelection)?;
        let source = roots.config.join("tilo");
        let connector = self.connector(target_home)?;
        Ok(self.spawn(move || connector.import_tilo_reviewed(&source, &selected)))
    }

    /// Begins consent after a person's connect action. Public URL may be
    /// opened through the existing host URI portal; no opener is called here.
    /// Omitting a source is an intentional new connection, not auto-discovery.
    pub fn begin(
        &mut self,
        provider: ConnectionProvider,
        candidate_id: Option<&str>,
        target_home: &Path,
    ) -> Result<ConsentLink, ActionError> {
        self.available()?;
        let selected = match candidate_id {
            None => None,
            Some(id) => Some(
                self.sources
                    .as_ref()
                    .and_then(|(_, inventory)| {
                        inventory
                            .candidates
                            .iter()
                            .find(|candidate| candidate.id == id)
                    })
                    .ok_or(ActionError::UnknownSelection)?,
            ),
        };
        if selected.is_some_and(|candidate| !provider.accepts(candidate)) {
            return Err(ActionError::WrongProvider);
        }
        let label = selected.map_or_else(
            || {
                match provider {
                    ConnectionProvider::GoogleDrive => "Google Drive",
                    ConnectionProvider::OneDrive => "OneDrive",
                    ConnectionProvider::Dropbox => "Dropbox",
                    ConnectionProvider::Spotify => "Spotify",
                    ConnectionProvider::SpotifyStreaming => "Spotify playback",
                }
                .to_string()
            },
            |candidate| candidate.label.clone(),
        );
        let connector = self.connector(target_home)?;
        let authorization = match provider.cloud() {
            Some(cloud) => connector.begin_oauth(
                cloud,
                selected.and_then(|candidate| candidate.identity.as_deref()),
            ),
            None if provider == ConnectionProvider::SpotifyStreaming => {
                connector.begin_spotify_streaming()
            }
            None => connector.begin_spotify(),
        }
        .map_err(|_| ActionError::ConnectionFailed)?;
        let id = uuid::Uuid::new_v4().to_string();
        let link = ConsentLink {
            connection_id: id.clone(),
            url: authorization.url().to_string(),
            needs_pasted_code: authorization.needs_pasted_code(),
        };
        self.pending.insert(
            id,
            PendingConnection {
                provider,
                label,
                home: target_home.to_path_buf(),
                connector,
                authorization,
            },
        );
        Ok(link)
    }

    /// Starts the waiting/exchange worker; the verifier, authorization code
    /// and resulting grant remain in private owned worker state.
    pub fn complete(&mut self, connection_id: &str) -> Result<String, ActionError> {
        self.complete_inner(connection_id, None)
    }

    pub fn complete_with_code(
        &mut self,
        connection_id: &str,
        code: zeroize::Zeroizing<String>,
    ) -> Result<String, ActionError> {
        if code.trim().is_empty() || code.len() > 4096 {
            return Err(ActionError::ConnectionFailed);
        }
        self.complete_inner(connection_id, Some(code))
    }

    fn complete_inner(
        &mut self,
        connection_id: &str,
        code: Option<zeroize::Zeroizing<String>>,
    ) -> Result<String, ActionError> {
        self.poll();
        if self.jobs.values().any(|job| job.result.is_some()) {
            return Err(ActionError::Busy);
        }
        let pending = self
            .pending
            .get(connection_id)
            .ok_or(ActionError::UnknownConnection)?;
        if pending.authorization.needs_pasted_code() != code.is_some() {
            return Err(ActionError::ConnectionFailed);
        }
        target_root(&pending.home)?;
        if self.sources.is_some() {
            self.boundary
                .as_ref()
                .ok_or(ActionError::SourceChanged)?
                .check_target(&pending.home)?;
        }
        let pending = self
            .pending
            .remove(connection_id)
            .ok_or(ActionError::UnknownConnection)?;
        let cancel = WorkCancel {
            loopback: pending.authorization.canceller(),
            nextcloud: None,
            requested: Arc::new(AtomicBool::new(false)),
        };
        let requested = cancel.requested.clone();
        let cleanup = self
            .staging_lease
            .as_ref()
            .map(|_| pending.connector.clone());
        let job = self.spawn(move || {
            let result = match (pending.provider.cloud(), code) {
                (Some(cloud), Some(code)) => pending.connector.finish_oauth_with_code(
                    cloud,
                    &pending.label,
                    pending.authorization,
                    code,
                ),
                (Some(cloud), None) => {
                    pending
                        .connector
                        .finish_oauth(cloud, &pending.label, pending.authorization)
                }
                (None, None) if pending.provider == ConnectionProvider::SpotifyStreaming => pending
                    .connector
                    .finish_spotify_streaming(pending.authorization),
                (None, None) => pending.connector.finish_spotify(pending.authorization),
                (None, Some(_)) => Err(ConnectionError::InvalidConfiguration),
            };
            if let Ok(id) = &result
                && requested.load(Ordering::SeqCst)
                && pending.connector.disconnect(id).is_ok()
            {
                return Err(ConnectionError::SignInFailed);
            }
            result
        });
        if let Some(work) = self.jobs.get_mut(&job) {
            work.cancel = Some(cancel);
            work.cleanup = cleanup;
        }
        Ok(job)
    }

    /// Dropping an uncompleted connection releases its private listener and
    /// verifier. Running credential writes are not represented as cancellable.
    pub fn cancel(&mut self, connection_id: &str) -> bool {
        self.pending.remove(connection_id).is_some()
    }

    /// Stop a sign-in still waiting for its provider redirect. Once the
    /// redirect is approved, the controller observes any completed receipt
    /// and removes the staged grant if cancellation raced completion.
    pub fn cancel_job(&mut self, job_id: &str) -> bool {
        let Some(work) = self.jobs.get_mut(job_id) else {
            return false;
        };
        let Some(cancel) = work.cancel.as_ref() else {
            return false;
        };
        cancel.cancel();
        true
    }

    /// The controller has retained this successful staged grant as a selected
    /// prepared account. Only then can drop stop compensating this receipt.
    pub fn acknowledge_prepared(&mut self, job_id: &str) {
        if let Some(work) = self.jobs.get_mut(job_id)
            && matches!(work.receipt.state, AccountJobState::Completed { .. })
        {
            work.cleanup = None;
        }
    }

    /// A cancellation removed the newly staged grant. Release all matching
    /// cleanup receipts before a later connection can reuse a fixed slot.
    pub fn acknowledge_removed(&mut self, account_id: &str) {
        for work in self.jobs.values_mut() {
            if matches!(&work.receipt.state, AccountJobState::Completed { account_id: completed } if completed == account_id)
            {
                work.cleanup = None;
            }
        }
    }

    pub fn disconnect_prepared(
        &mut self,
        account_id: &str,
        target_home: &Path,
    ) -> Result<String, ActionError> {
        self.available()?;
        let connector = self.connector(target_home)?;
        let id = AccountId::new(account_id);
        Ok(self.spawn(move || {
            connector.disconnect(&id)?;
            Ok(id)
        }))
    }

    pub fn forget_prepared(
        &mut self,
        account_id: &str,
        target_home: &Path,
    ) -> Result<String, ActionError> {
        self.available()?;
        let connector = self.connector(target_home)?;
        let id = AccountId::new(account_id);
        Ok(self.spawn(move || {
            connector.forget_prepared(&id)?;
            Ok(id)
        }))
    }

    pub fn status(&mut self) -> Vec<AccountJob> {
        self.poll();
        self.jobs
            .values()
            .map(|work| work.receipt.clone())
            .collect()
    }

    fn spawn(
        &mut self,
        operation: impl FnOnce() -> Result<AccountId, ConnectionError> + Send + 'static,
    ) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let (sender, result) = std::sync::mpsc::channel();
        let lease = self.staging_lease.clone();
        std::thread::spawn(move || {
            let _lease = lease;
            let _ = sender.send(operation());
        });
        self.jobs.insert(
            id.clone(),
            Work {
                receipt: AccountJob {
                    id: id.clone(),
                    state: AccountJobState::Running,
                },
                result: Some(result),
                cancel: None,
                cleanup: None,
            },
        );
        id
    }

    fn poll(&mut self) {
        for work in self.jobs.values_mut() {
            let Some(receiver) = &work.result else {
                continue;
            };
            let outcome = match receiver.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => Err(ConnectionError::StorageUnavailable),
            };
            work.result = None;
            work.receipt.state = match outcome {
                Ok(id) => AccountJobState::Completed {
                    account_id: id.as_str().into(),
                },
                Err(reason) => AccountJobState::Failed { reason },
            };
        }
    }
}

impl Drop for AccountActions {
    fn drop(&mut self) {
        for work in self.jobs.values_mut() {
            let Some(connector) = work.cleanup.take() else {
                continue;
            };
            if let Some(cancel) = &work.cancel {
                cancel.cancel();
            }
            let completed = match &work.receipt.state {
                AccountJobState::Completed { account_id } => Some(AccountId::new(account_id)),
                _ => None,
            };
            let receiver = work.result.take();
            let lease = self.staging_lease.clone();
            std::thread::spawn(move || {
                let _lease = lease;
                let id = completed.or_else(|| receiver?.recv().ok()?.ok());
                if let Some(id) = id {
                    let _ = connector.disconnect(&id);
                }
            });
        }
    }
}

/// A copied .config symlink must never make account import modify the old
/// system. Check every existing destination component, including vault paths,
/// before invoking path-based canonical stores. Caller owns the reviewed home.
fn target_root(home: &Path) -> Result<PathBuf, ActionError> {
    let metadata = std::fs::symlink_metadata(home).map_err(|_| ActionError::TargetUnavailable)?;
    if metadata.is_symlink() {
        return Err(ActionError::TargetRedirected);
    }
    require_private_owner(&metadata)?;
    let home = home
        .canonicalize()
        .map_err(|_| ActionError::TargetUnavailable)?;
    if !home.is_dir() {
        return Err(ActionError::TargetUnavailable);
    }
    for relative in [
        ".config",
        ".config/tilo",
        ".config/tilo/accounts",
        ".config/tilo/accounts/vault",
        ".config/tilo/accounts/handoff",
    ] {
        match std::fs::symlink_metadata(home.join(relative)) {
            Ok(metadata) if metadata.is_symlink() || !metadata.is_dir() => {
                return Err(ActionError::TargetRedirected);
            }
            Ok(metadata) => require_private_owner(&metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(ActionError::TargetUnavailable),
        }
    }
    Ok(home.join(".config/tilo"))
}

#[derive(Clone)]
struct SourceBoundary {
    home: SourceIdentity,
    config: Option<SourceIdentity>,
}
#[derive(Clone)]
struct SourceIdentity {
    path: PathBuf,
    canonical: PathBuf,
    #[cfg(unix)]
    identity: (u64, u64),
}
impl SourceIdentity {
    fn capture(path: &Path) -> Result<Self, ActionError> {
        let metadata = std::fs::metadata(path).map_err(|_| ActionError::SourceChanged)?;
        if !metadata.is_dir() {
            return Err(ActionError::SourceChanged);
        }
        Ok(Self {
            path: path.to_path_buf(),
            canonical: path
                .canonicalize()
                .map_err(|_| ActionError::SourceChanged)?,
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (metadata.dev(), metadata.ino())
            },
        })
    }
    fn unchanged(&self) -> Result<(), ActionError> {
        let current = Self::capture(&self.path)?;
        if current.canonical != self.canonical {
            return Err(ActionError::SourceChanged);
        }
        #[cfg(unix)]
        if current.identity != self.identity {
            return Err(ActionError::SourceChanged);
        }
        Ok(())
    }
}
impl SourceBoundary {
    fn capture(roots: &SourceRoots) -> Result<Self, ActionError> {
        let home = SourceIdentity::capture(&roots.home)?;
        let config = if roots.config.exists() {
            Some(SourceIdentity::capture(&roots.config)?)
        } else {
            None
        };
        Ok(Self { home, config })
    }
    fn check_target(&self, target: &Path) -> Result<(), ActionError> {
        let target = target
            .canonicalize()
            .map_err(|_| ActionError::TargetUnavailable)?;
        self.home.unchanged()?;
        if let Some(config) = &self.config {
            config.unchanged()?;
        }
        for source in std::iter::once(&self.home).chain(self.config.iter()) {
            if target.starts_with(&source.canonical) || source.canonical.starts_with(&target) {
                return Err(ActionError::TargetOverlapsSource);
            }
        }
        Ok(())
    }
}

fn require_private_owner(metadata: &std::fs::Metadata) -> Result<(), ActionError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let owner = rustix::process::geteuid().as_raw();
        if metadata.uid() != owner || metadata.mode() & 0o7022 != 0 {
            return Err(ActionError::TargetNotPrivate);
        }
    }
    #[cfg(not(unix))]
    let _ = metadata;
    Ok(())
}
