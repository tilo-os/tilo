//! Session-private authorization. Workers never write custody: only polling the
//! currently selected attempt can publish a verified result. No value here is
//! document-backed, and backend diagnostics never cross the fixed-status boundary.

pub mod browser;
pub mod hints;
pub mod view;

use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use account_custody::portable::{self, PasswordSealedAccounts, PortableBinding};
use account_custody::{Custodian, LoopbackCancel, OAuthCloud, OAuthIssuer, OAuthUse};
use accounts::{Account, AccountId, AccountStore, Secret, SecretVault};
use zeroize::Zeroize;

use browser::{BrowserOpener, LiveBrowserOpener};
use view::{AccountRequest, AccountsCommand, AccountsStatus, Provider};

/// Deliberately fixed: never carries provider responses or private fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountsError {
    Privileged,
    InvalidSession,
    Unavailable,
    Busy,
    InvalidRequest,
    Storage,
}

impl std::fmt::Display for AccountsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Privileged => "Account authorization requires an unprivileged session",
            Self::InvalidSession => "Private installer session is unavailable",
            Self::Unavailable => "Account authorization is unavailable",
            Self::Busy => "Account authorization is still stopping",
            Self::InvalidRequest => "Requested account capabilities are unavailable or invalid",
            Self::Storage => "Private account custody is unavailable",
        })
    }
}

impl std::error::Error for AccountsError {}

struct Verified {
    entries: Vec<(Account, PendingCredential)>,
    calendar: Option<CalendarReview>,
}

struct CalendarReview {
    entry: usize,
    config: calendar::CalDavConfig,
    discovered: Vec<calendar::RemoteCalendar>,
    google: bool,
}

impl Drop for Verified {
    fn drop(&mut self) {
        for (_, secret) in &mut self.entries {
            if let PendingCredential::Existing(secret) = secret {
                secret.password.zeroize();
                if let Some(token) = &mut secret.token {
                    token.zeroize();
                }
            }
        }
    }
}

struct PrivateTokens(account_custody::TokenSet);

impl Drop for PrivateTokens {
    fn drop(&mut self) {
        self.0.access_token.zeroize();
        self.0.refresh_token.zeroize();
    }
}

// Calendar discovery owns its combined grant; only selected service operations
// and private custody staging are available to the installer.
enum PendingGrant {
    Existing(PrivateTokens),
    GoogleCalendar(Arc<tilo_dav::oauth::PendingGoogleCalendar>),
}

impl PendingGrant {
    fn resolve_storage(
        &self,
        custody: &Custodian,
        cloud: OAuthCloud,
    ) -> Result<serde_json::Value, AccountsError> {
        match self {
            Self::Existing(tokens) => custody
                .resolve_storage(cloud, &tokens.0)
                .map_err(|_| AccountsError::Unavailable),
            Self::GoogleCalendar(pending) => pending
                .resolve_storage(custody)
                .map_err(|_| AccountsError::Unavailable),
        }
    }

    fn mail_identity(
        &self,
        custody: &Custodian,
        issuer: OAuthIssuer,
    ) -> Result<String, AccountsError> {
        match self {
            Self::Existing(tokens) => {
                let result = if issuer == OAuthIssuer::Microsoft {
                    custody.microsoft_mail_identity(&tokens.0)
                } else {
                    custody.gmail_identity(&tokens.0)
                };
                result.map_err(|_| AccountsError::Unavailable)
            }
            Self::GoogleCalendar(pending) => pending
                .mail_identity(custody)
                .map_err(|_| AccountsError::Unavailable),
        }
    }

    fn staging_credential(&self) -> PendingCredential {
        match self {
            Self::Existing(tokens) => {
                PendingCredential::Existing(account_custody::oauth_secret(&tokens.0))
            }
            Self::GoogleCalendar(pending) => PendingCredential::GoogleCalendar(Arc::clone(pending)),
        }
    }
}

enum PendingCredential {
    Existing(Secret),
    GoogleCalendar(Arc<tilo_dav::oauth::PendingGoogleCalendar>),
}

#[derive(Default)]
struct Cancellation {
    cancelled: bool,
    loopback: Option<LoopbackCancel>,
    nextcloud: Option<mpsc::SyncSender<()>>,
}

impl Cancellation {
    fn cancel(&mut self) {
        self.cancelled = true;
        if let Some(listener) = self.loopback.take() {
            listener.cancel();
        }
        if let Some(sender) = self.nextcloud.take() {
            let _ = sender.try_send(());
        }
    }
}

struct Attempt {
    cancel: Arc<Mutex<Cancellation>>,
    result: mpsc::Receiver<Result<Verified, AccountsError>>,
}

/// Owned by the installer session, never the shared profile or install worker.
/// Call `command` for drained form commands and `poll` on each UI sync; publish
/// its fixed status and private `connected` projection to the local form only.
/// `selected` contains opaque private
/// IDs; `stage_selected` returns ciphertext for the existing staging handoff.
pub struct AccountsController {
    root: PathBuf,
    custody: Arc<Custodian>,
    opener: Arc<dyn BrowserOpener>,
    attempt: Option<Attempt>,
    worker_alive: Arc<AtomicBool>,
    selected: Vec<AccountId>,
    pending: Option<Verified>,
    status: AccountsStatus,
}

impl AccountsController {
    /// Start a fresh isolated canonical custody store below an existing, owned
    /// 0700 session directory. Both real and effective uid must be non-root.
    ///
    /// # Errors
    /// Rejects privileged callers, invalid session directories, unavailable Browser
    /// supervision, or failure to create the private custody store.
    pub fn live(session_root: &Path) -> Result<Self, AccountsError> {
        if rustix::process::getuid().is_root() || rustix::process::geteuid().is_root() {
            return Err(AccountsError::Privileged);
        }
        validate_session(session_root)?;
        let opener = LiveBrowserOpener::new().map_err(|_| AccountsError::Unavailable)?;
        Self::create(session_root, Arc::new(opener), |custody| custody)
    }

    /// Test seam retains real canonical custody and OAuth callback handling.
    #[cfg(test)]
    pub(crate) fn fixture(
        session_root: &Path,
        opener: Arc<dyn BrowserOpener>,
        configure: impl FnOnce(Custodian) -> Custodian,
    ) -> Result<Self, AccountsError> {
        Self::create(session_root, opener, configure)
    }

    fn create(
        session_root: &Path,
        opener: Arc<dyn BrowserOpener>,
        configure: impl FnOnce(Custodian) -> Custodian,
    ) -> Result<Self, AccountsError> {
        validate_session(session_root)?;
        let root = std::fs::canonicalize(session_root)
            .map_err(|_| AccountsError::InvalidSession)?
            .join(format!("online-accounts-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .map_err(|_| AccountsError::Storage)?;
        let custody = Arc::new(configure(Custodian::for_root(Some(root.clone()))));
        Ok(Self {
            root,
            custody,
            opener,
            attempt: None,
            worker_alive: Arc::new(AtomicBool::new(false)),
            selected: Vec::new(),
            pending: None,
            status: AccountsStatus::Ready,
        })
    }

    pub fn status(&self) -> AccountsStatus {
        self.status
    }

    /// Discovered collection labels remain local to this controller.
    pub fn calendars(&self) -> Vec<(String, String)> {
        self.pending
            .as_ref()
            .and_then(|verified| verified.calendar.as_ref())
            .map(|review| {
                review
                    .discovered
                    .iter()
                    .map(|calendar| (calendar.id.clone(), calendar.name.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn selected(&self) -> &[AccountId] {
        &self.selected
    }

    /// Private presentation of selected canonical metadata only. This never opens
    /// the credential vault or refreshes a grant. Unsaved discovery and source
    /// suggestions cannot make a service appear connected.
    pub fn connected(&self) -> Vec<view::ConnectedAccount> {
        connected_accounts(&self.root, &self.selected)
    }

    /// At most one worker and one pending outcome exist. Retry invalidates even
    /// a successful but not-yet-polled outcome before starting another attempt.
    pub fn command(&mut self, command: AccountsCommand) -> Result<(), AccountsError> {
        match command {
            AccountsCommand::SelectCalendars(selected) => {
                let verified = self.pending.as_mut().ok_or(AccountsError::InvalidRequest)?;
                let review = verified
                    .calendar
                    .as_ref()
                    .ok_or(AccountsError::InvalidRequest)?;
                let entry = verified
                    .entries
                    .get_mut(review.entry)
                    .ok_or(AccountsError::InvalidRequest)?;
                calendar::setup::select_calendars(
                    &mut entry.0,
                    &review.config,
                    &review.discovered,
                    &selected,
                    review.google,
                )
                .map_err(|_| AccountsError::InvalidRequest)?;
                let verified = self.pending.take().ok_or(AccountsError::InvalidRequest)?;
                let result = self.save(&verified);
                self.status = if result.is_ok() {
                    AccountsStatus::Added
                } else {
                    AccountsStatus::Failed
                };
                result
            }
            AccountsCommand::Cancel | AccountsCommand::SkipOffline => {
                self.cancel();
                self.status = AccountsStatus::Ready;
                Ok(())
            }
            AccountsCommand::Add(request) | AccountsCommand::Retry(request) => {
                self.cancel();
                let result = self.start(request);
                if result.is_err() {
                    self.status = AccountsStatus::Failed;
                }
                result
            }
        }
    }

    fn start(&mut self, request: AccountRequest) -> Result<(), AccountsError> {
        validate_request(&request)?;
        if self
            .selected
            .len()
            .saturating_add(usize::from(request.files))
            .saturating_add(usize::from(request.mail))
            .saturating_add(usize::from(request.calendar))
            > 64
        {
            return Err(AccountsError::InvalidRequest);
        }
        if self.worker_alive.swap(true, Ordering::AcqRel) {
            return Err(AccountsError::Busy);
        }
        let cancel = Arc::new(Mutex::new(Cancellation::default()));
        let (sender, result) = mpsc::sync_channel(1);
        let custody = self.custody.clone();
        let opener = self.opener.clone();
        let cancellation = cancel.clone();
        let alive = self.worker_alive.clone();
        let spawn = std::thread::Builder::new()
            .name("installer-account".into())
            .spawn(move || {
                let outcome = authorize(&custody, opener.as_ref(), &request, &cancellation);
                // The actor owns window closure. A stale worker must not close
                // the browser of a later attempt.
                let _ = sender.try_send(outcome);
                alive.store(false, Ordering::Release);
            });
        if spawn.is_err() {
            self.worker_alive.store(false, Ordering::Release);
            return Err(AccountsError::Unavailable);
        }
        self.attempt = Some(Attempt { cancel, result });
        self.status = AccountsStatus::SigningIn;
        Ok(())
    }

    pub fn poll(&mut self) -> AccountsStatus {
        let outcome = match self
            .attempt
            .as_ref()
            .map(|attempt| attempt.result.try_recv())
        {
            Some(Ok(outcome)) => outcome,
            Some(Err(mpsc::TryRecvError::Disconnected)) => Err(AccountsError::Unavailable),
            _ => return self.status,
        };
        self.cancel();
        self.status = match outcome {
            Ok(verified) if verified.calendar.is_some() => {
                self.pending = Some(verified);
                AccountsStatus::ReviewCalendars
            }
            Ok(verified) => match self.save(&verified) {
                Ok(()) => AccountsStatus::Added,
                Err(_) => AccountsStatus::Failed,
            },
            Err(_) => AccountsStatus::Failed,
        };
        self.status
    }

    fn save(&mut self, verified: &Verified) -> Result<(), AccountsError> {
        let mut store = AccountStore::load_in(&self.root);
        let mut vault = SecretVault::open_in(&self.root);
        if vault.recovery_error().is_some() {
            return Err(AccountsError::Storage);
        }
        for (account, secret) in &verified.entries {
            match secret {
                PendingCredential::Existing(secret) => vault.put(&account.id, secret),
                PendingCredential::GoogleCalendar(pending) => {
                    pending.stage(&account.id, &mut vault);
                }
            }
            store.upsert(account.clone());
        }
        vault.try_save().map_err(|_| AccountsError::Storage)?;
        store
            .save_in(&self.root)
            .map_err(|_| AccountsError::Storage)?;
        self.selected.extend(
            verified
                .entries
                .iter()
                .map(|(account, _)| account.id.clone()),
        );
        Ok(())
    }

    /// Only explicit selected IDs from this session may be transported. Binding
    /// must come from the authenticated install operation/target/user, never an
    /// imported artifact. Adoption belongs to the installed user's login.
    ///
    /// # Errors
    /// Rejects empty or foreign selections and failures reading or sealing custody.
    pub fn stage_selected(
        &mut self,
        selected: &[AccountId],
        password: &[u8],
        binding: &PortableBinding,
    ) -> Result<PasswordSealedAccounts, AccountsError> {
        self.cancel();
        if matches!(
            self.status,
            AccountsStatus::SigningIn | AccountsStatus::ReviewCalendars
        ) {
            self.status = AccountsStatus::Ready;
        }
        if selected.is_empty() || selected.iter().any(|id| !self.selected.contains(id)) {
            return Err(AccountsError::InvalidRequest);
        }
        portable::seal_selected_accounts_with_password(
            &self.root,
            selected,
            password,
            binding.as_bytes(),
        )
        .map_err(|_| AccountsError::Storage)
    }

    /// Seal selected session accounts under a caller-owned transport key.
    /// The caller must protect the key in authenticated setup custody and use
    /// the same operation/target/user binding when adopting the artifact.
    ///
    /// # Errors
    /// Rejects empty or foreign selections and failures reading or sealing custody.
    pub fn stage_selected_with_key(
        &mut self,
        selected: &[AccountId],
        key: &tilo_seal::SymmetricKey,
        binding: &PortableBinding,
    ) -> Result<portable::SealedAccounts, AccountsError> {
        self.cancel();
        if matches!(
            self.status,
            AccountsStatus::SigningIn | AccountsStatus::ReviewCalendars
        ) {
            self.status = AccountsStatus::Ready;
        }
        if selected.is_empty() || selected.iter().any(|id| !self.selected.contains(id)) {
            return Err(AccountsError::InvalidRequest);
        }
        portable::seal_selected_accounts(&self.root, selected, key, binding.as_bytes())
            .map_err(|_| AccountsError::Storage)
    }

    fn cancel(&mut self) {
        self.pending = None;
        if let Some(attempt) = self.attempt.take()
            && let Ok(mut cancel) = attempt.cancel.lock()
        {
            cancel.cancel();
        }
        let _ = self.opener.close();
    }
}

impl Drop for AccountsController {
    fn drop(&mut self) {
        self.cancel();
        // Workers never touch this directory, including after cancellation.
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn connected_accounts(root: &Path, selected: &[AccountId]) -> Vec<view::ConnectedAccount> {
    use accounts::cloud_types::{DROPBOX, GOOGLE_DRIVE, MOUNT_CONFIG_KEY, NEXTCLOUD, ONEDRIVE};
    let store = AccountStore::load_in(root);
    let mut connected: Vec<view::ConnectedAccount> = Vec::new();
    for id in selected {
        let Some(account) = store.get(id) else {
            continue;
        };
        let config = &account.config;
        let calendar = config
            .get("use_calendar")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            && config
                .pointer("/calendar/calendars")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|collections| !collections.is_empty());
        let kind = account.type_id.as_str();
        let provider = match kind {
            GOOGLE_DRIVE => Provider::Google,
            ONEDRIVE => Provider::Microsoft,
            DROPBOX => Provider::Dropbox,
            NEXTCLOUD => Provider::Nextcloud,
            "email" => match config.get("imap_host").and_then(serde_json::Value::as_str) {
                Some("imap.gmail.com") => Provider::Google,
                Some("outlook.office365.com") => Provider::Microsoft,
                _ => continue,
            },
            "caldav"
                if config
                    .get("calendar_provider")
                    .and_then(serde_json::Value::as_str)
                    == Some("google") =>
            {
                Provider::Google
            }
            "caldav"
                if config
                    .get("app_password")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true) =>
            {
                Provider::Nextcloud
            }
            _ => continue,
        };
        let files = match kind {
            GOOGLE_DRIVE | ONEDRIVE | DROPBOX => config
                .get(MOUNT_CONFIG_KEY)
                .is_some_and(serde_json::Value::is_object),
            NEXTCLOUD => config.get("use_files").and_then(serde_json::Value::as_bool) == Some(true),
            _ => false,
        };
        let mail = kind == "email";
        if !files && !mail && !calendar {
            continue;
        }
        let identity = if provider == Provider::Nextcloud {
            format!(
                "{} — {}",
                config
                    .get("username")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default(),
                config
                    .get("server_url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
            )
        } else {
            config
                .get("connected_identity")
                .or_else(|| config.get("email"))
                .and_then(serde_json::Value::as_str)
                .filter(|identity| !identity.is_empty())
                .unwrap_or_else(|| id.as_str())
                .to_owned()
        };
        if let Some(existing) = connected
            .iter_mut()
            .find(|existing| existing.provider == provider && existing.identity == identity)
        {
            existing.files |= files;
            existing.mail |= mail;
            existing.calendar |= calendar;
        } else {
            connected.push(view::ConnectedAccount {
                provider,
                identity,
                files,
                mail,
                calendar,
            });
        }
    }
    connected
}

fn validate_session(root: &Path) -> Result<(), AccountsError> {
    let metadata = std::fs::symlink_metadata(root).map_err(|_| AccountsError::InvalidSession)?;
    if !root.is_absolute()
        || !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(AccountsError::InvalidSession);
    }
    Ok(())
}

fn validate_request(request: &AccountRequest) -> Result<(), AccountsError> {
    if (!request.files && !request.mail && !request.calendar)
        || [&request.email, &request.server]
            .iter()
            .any(|value| value.len() > 2048 || value.chars().any(char::is_control))
        || (request.mail && !matches!(request.provider, Provider::Google | Provider::Microsoft))
        || (request.calendar && !matches!(request.provider, Provider::Google | Provider::Nextcloud))
    {
        return Err(AccountsError::InvalidRequest);
    }
    if request.provider == Provider::Nextcloud {
        account_custody::nextcloud::server_url(&request.server)
            .map_err(|_| AccountsError::InvalidRequest)?;
    }
    Ok(())
}

fn authorize(
    custody: &Custodian,
    opener: &dyn BrowserOpener,
    request: &AccountRequest,
    cancellation: &Mutex<Cancellation>,
) -> Result<Verified, AccountsError> {
    if request.provider == Provider::Nextcloud {
        return authorize_nextcloud(opener, request, cancellation);
    }
    let cloud = match request.provider {
        Provider::Google => OAuthCloud::GoogleDrive,
        Provider::Microsoft => OAuthCloud::OneDrive,
        Provider::Dropbox => OAuthCloud::Dropbox,
        Provider::Nextcloud => return Err(AccountsError::InvalidRequest),
    };
    let hint = (!request.email.is_empty()).then_some(request.email.as_str());
    let issuer = cloud.issuer();
    let capabilities = requested_capabilities(request, issuer);
    let authorization = custody
        .begin_sign_in_for_capabilities(issuer, &capabilities, hint)
        .map_err(|_| AccountsError::Unavailable)?;
    let (private_tokens, pending_calendar) = if request.calendar {
        launch_authorization(&authorization, opener, cancellation)?;
        let pending = tilo_dav::oauth::PendingGoogleCalendar::complete(authorization)
            .map_err(|_| AccountsError::Unavailable)?;
        let discovered = pending.discover().map_err(|_| AccountsError::Unavailable)?;
        (
            PendingGrant::GoogleCalendar(Arc::new(pending)),
            Some(discovered),
        )
    } else {
        (
            PendingGrant::Existing(complete_authorization(authorization, opener, cancellation)?),
            None,
        )
    };
    let mut verified = Verified {
        entries: Vec::new(),
        calendar: None,
    };
    if request.files {
        let mount = private_tokens.resolve_storage(custody, cloud)?;
        let mut account = new_account(cloud.type_id());
        account.config = serde_json::json!({"provider": cloud.type_id(), accounts::cloud_types::MOUNT_CONFIG_KEY: mount});
        verified
            .entries
            .push((account, private_tokens.staging_credential()));
    }
    if request.mail {
        let email = private_tokens.mail_identity(custody, issuer)?;
        if email.len() > 2048 || hint.is_some_and(|hint| !hint.eq_ignore_ascii_case(&email)) {
            return Err(AccountsError::Unavailable);
        }
        let mail_tokens = if issuer == OAuthIssuer::Microsoft {
            let authorization = custody
                .begin_sign_in_for(OAuthUse::mail(issuer), Some(&email))
                .map_err(|_| AccountsError::Unavailable)?;
            Some(complete_authorization(authorization, opener, cancellation)?)
        } else {
            None
        };
        if let Some(mail_tokens) = &mail_tokens
            && mail_tokens.0.scope.as_deref().is_some_and(|scopes| {
                !scopes.split_whitespace().any(|scope| {
                    scope.eq_ignore_ascii_case("https://outlook.office.com/IMAP.AccessAsUser.All")
                }) || !scopes
                    .split_whitespace()
                    .any(|scope| scope.eq_ignore_ascii_case("https://outlook.office.com/SMTP.Send"))
            })
        {
            return Err(AccountsError::Unavailable);
        }
        let config = issuer
            .mail_config(&email)
            .ok_or(AccountsError::Unavailable)?;
        let mut account = new_account("email");
        account.set_config(&config);
        verified.entries.push((
            account,
            mail_tokens.as_ref().map_or_else(
                || private_tokens.staging_credential(),
                |tokens| PendingCredential::Existing(account_custody::oauth_secret(&tokens.0)),
            ),
        ));
    }
    if let Some(pending) = pending_calendar {
        let (config, discovered) = calendar_model(pending);
        let mut account = new_account("caldav");
        account.config = serde_json::json!({});
        verified.calendar = Some(CalendarReview {
            entry: verified.entries.len(),
            config,
            discovered,
            google: true,
        });
        verified
            .entries
            .push((account, private_tokens.staging_credential()));
    }
    bind_confirmed_identity(&mut verified);
    Ok(verified)
}

fn bind_confirmed_identity(verified: &mut Verified) {
    // Only provider-confirmed identity may join independently stored services.
    // Files-only providers without an identity endpoint retain an opaque account
    // ID rather than treating the optional login hint as proof of identity.
    let identity = verified
        .entries
        .iter()
        .find_map(|(account, _)| {
            account
                .config
                .get("email")
                .and_then(serde_json::Value::as_str)
                .filter(|identity| !identity.is_empty())
                .map(str::to_owned)
        })
        .or_else(|| {
            verified
                .calendar
                .as_ref()
                .map(|review| review.config.username.clone())
                .filter(|identity| !identity.is_empty())
        });
    if let Some(identity) = identity {
        for (account, _) in &mut verified.entries {
            if let Some(config) = account.config.as_object_mut() {
                config.insert("connected_identity".into(), identity.clone().into());
            }
        }
    }
}

fn requested_capabilities(
    request: &AccountRequest,
    issuer: OAuthIssuer,
) -> Vec<account_custody::Capability> {
    use account_custody::Capability;
    let mut capabilities = Vec::new();
    if request.files {
        capabilities.push(Capability::Files);
    }
    if request.calendar {
        capabilities.push(Capability::Calendar);
    }
    if request.mail {
        capabilities.push(if issuer == OAuthIssuer::Microsoft {
            Capability::Identity
        } else {
            Capability::Mail
        });
    }
    capabilities
}

fn complete_authorization(
    authorization: account_custody::Authorization,
    opener: &dyn BrowserOpener,
    cancellation: &Mutex<Cancellation>,
) -> Result<PrivateTokens, AccountsError> {
    launch_authorization(&authorization, opener, cancellation)?;
    Ok(PrivateTokens(
        Custodian::finish_sign_in(authorization).map_err(|_| AccountsError::Unavailable)?,
    ))
}

fn launch_authorization(
    authorization: &account_custody::Authorization,
    opener: &dyn BrowserOpener,
    cancellation: &Mutex<Cancellation>,
) -> Result<(), AccountsError> {
    if authorization.needs_pasted_code() {
        return Err(AccountsError::Unavailable);
    }
    {
        let mut cancel = cancellation
            .lock()
            .map_err(|_| AccountsError::Unavailable)?;
        if cancel.cancelled {
            return Err(AccountsError::Unavailable);
        }
        cancel.loopback = authorization.canceller();
        // Serialize launch with cancellation: cancel cannot close first and
        // then have a stale worker reopen a window.
        opener
            .open(authorization.url())
            .map_err(|_| AccountsError::Unavailable)?;
        drop(cancel);
    }
    Ok(())
}

fn calendar_model(
    pending: tilo_dav::oauth::PendingCalendars,
) -> (calendar::CalDavConfig, Vec<calendar::RemoteCalendar>) {
    let config = calendar::CalDavConfig {
        server_url: pending.server_url,
        username: pending.username,
        ..Default::default()
    };
    let discovered = pending
        .collections
        .into_iter()
        .zip((0_u8..8).cycle())
        .map(|(collection, color)| calendar::RemoteCalendar {
            id: collection.href,
            name: collection.name,
            color,
            visible: true,
            read_only: collection.read_only,
        })
        .collect();
    (config, discovered)
}

fn new_account(kind: &str) -> Account {
    Account::new(AccountId::new(uuid::Uuid::new_v4().to_string()), kind)
}

fn authorize_nextcloud(
    opener: &dyn BrowserOpener,
    request: &AccountRequest,
    cancellation: &Mutex<Cancellation>,
) -> Result<Verified, AccountsError> {
    use account_custody::nextcloud;
    let flow = nextcloud::begin(&request.server).map_err(|_| AccountsError::Unavailable)?;
    let (sender, receiver) = mpsc::sync_channel(1);
    {
        let mut cancel = cancellation
            .lock()
            .map_err(|_| AccountsError::Unavailable)?;
        if cancel.cancelled {
            return Err(AccountsError::Unavailable);
        }
        cancel.nextcloud = Some(sender);
        opener
            .open(flow.login_url())
            .map_err(|_| AccountsError::Unavailable)?;
        // Keep cancellation serialized through launch, as for loopback OAuth.
        drop(cancel);
    }
    let login = flow
        .wait(
            &receiver,
            nextcloud::POLL_INTERVAL,
            nextcloud::FLOW_LIFETIME,
        )
        .map_err(|_| AccountsError::Unavailable)?;
    let mut account = new_account(if request.files {
        accounts::cloud_types::NEXTCLOUD
    } else {
        "caldav"
    });
    account.config = serde_json::json!({"server_url": login.server, "username": login.login_name, "app_password": true, "use_files": request.files});
    let calendar = if request.calendar {
        let pending = tilo_dav::oauth::PendingNextcloudCalendar::new(
            &login.server,
            &login.login_name,
            login.app_password.as_str(),
        )
        .map_err(|_| AccountsError::Unavailable)?;
        let discovered = pending.discover().map_err(|_| AccountsError::Unavailable)?;
        let (config, discovered) = calendar_model(discovered);
        Some(CalendarReview {
            entry: 0,
            config,
            discovered,
            google: false,
        })
    } else {
        None
    };
    Ok(Verified {
        entries: vec![(
            account,
            PendingCredential::Existing(Secret::password(login.app_password.as_str())),
        )],
        calendar,
    })
}

#[cfg(test)]
pub(crate) mod calendar_tests;
#[cfg(test)]
mod native_browser_tests;
#[cfg(test)]
mod oauth_tests;
#[cfg(test)]
pub(crate) mod tests;
