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
    entries: Vec<(Account, Secret)>,
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
            secret.password.zeroize();
            if let Some(token) = &mut secret.token {
                token.zeroize();
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
/// Call `command` for drained form commands and `poll` on each UI sync; copy only
/// its returned fixed status into the form. `selected` contains opaque private
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

    /// At most one worker and one pending outcome exist. Retry invalidates even
    /// a successful but not-yet-polled outcome before starting another attempt.
    ///
    /// # Errors
    /// Returns an error for invalid requests, a still-stopping worker, or failure
    /// to start authorization.
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
            vault.put(&account.id, secret);
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
    let private_tokens = complete_authorization(authorization, opener, cancellation)?;
    let tokens = &private_tokens.0;
    let mut verified = Verified {
        entries: Vec::new(),
        calendar: None,
    };
    if request.files {
        let mount = custody
            .resolve_storage(cloud, tokens)
            .map_err(|_| AccountsError::Unavailable)?;
        let mut account = new_account(cloud.type_id());
        account.config = serde_json::json!({"provider": cloud.type_id(), accounts::cloud_types::MOUNT_CONFIG_KEY: mount});
        verified
            .entries
            .push((account, account_custody::oauth_secret(tokens)));
    }
    if request.mail {
        let email = if issuer == OAuthIssuer::Microsoft {
            custody.microsoft_mail_identity(tokens)
        } else {
            custody.gmail_identity(tokens)
        }
        .map_err(|_| AccountsError::Unavailable)?;
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
        let mail_tokens = mail_tokens.as_ref().map_or(tokens, |tokens| &tokens.0);
        if issuer == OAuthIssuer::Microsoft
            && mail_tokens.scope.as_deref().is_some_and(|scopes| {
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
        verified
            .entries
            .push((account, account_custody::oauth_secret(mail_tokens)));
    }
    if request.calendar {
        let config = google_calendar_config(tokens)?;
        let mut account = new_account("caldav");
        account.config = serde_json::json!({});
        let discovered = discover(
            &config,
            tilo_dav::Credentials::bearer(tokens.access_token.clone()),
        )?;
        verified.calendar = Some(CalendarReview {
            entry: verified.entries.len(),
            config,
            discovered,
            google: true,
        });
        verified
            .entries
            .push((account, account_custody::oauth_secret(tokens)));
    }
    Ok(verified)
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
    Ok(PrivateTokens(
        Custodian::finish_sign_in(authorization).map_err(|_| AccountsError::Unavailable)?,
    ))
}

fn google_calendar_config(
    tokens: &account_custody::TokenSet,
) -> Result<calendar::CalDavConfig, AccountsError> {
    // Resolve the authenticated primary calendar through Google's API rather
    // than treating a login hint (or the REST-only `primary` alias) as a DAV ID.
    let client = tilo_dav::DavClient::new(&tilo_dav::Options {
        redirects: tilo_dav::Redirects::Refuse,
        ..Default::default()
    })
    .map_err(|_| AccountsError::Unavailable)?
    .with_credentials(tilo_dav::Credentials::bearer(tokens.access_token.clone()));
    let response = client
        .get("https://www.googleapis.com/calendar/v3/calendars/primary")
        .map_err(|_| AccountsError::Unavailable)?;
    if !response.status().is_success() {
        return Err(AccountsError::Unavailable);
    }
    let body = response
        .read_bounded(64 * 1024)
        .map_err(|_| AccountsError::Unavailable)?;
    let calendar: serde_json::Value =
        serde_json::from_slice(&body).map_err(|_| AccountsError::Unavailable)?;
    let id = calendar
        .get("id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 2048
                && !id.chars().any(|ch| ch.is_control() || ch.is_whitespace())
        })
        .ok_or(AccountsError::Unavailable)?;
    Ok(calendar::setup::google_config(id))
}

fn discover(
    config: &calendar::CalDavConfig,
    credentials: tilo_dav::Credentials,
) -> Result<Vec<calendar::RemoteCalendar>, AccountsError> {
    let server = url::Url::parse(&config.server_url).map_err(|_| AccountsError::Unavailable)?;
    let client = tilo_dav::DavClient::new(&tilo_dav::Options {
        redirects: tilo_dav::Redirects::Refuse,
        ..Default::default()
    })
    .map_err(|_| AccountsError::Unavailable)?
    .with_credentials(credentials);
    let collections =
        tilo_dav::discovery::calendars(&client, &server).map_err(|_| AccountsError::Unavailable)?;
    if collections.is_empty() {
        return Err(AccountsError::Unavailable);
    }
    Ok(collections
        .into_iter()
        .zip((0_u8..8).cycle())
        .map(|(collection, color)| calendar::RemoteCalendar {
            id: collection.href,
            name: collection.name,
            color,
            visible: true,
            read_only: collection.read_only,
        })
        .collect())
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
        let mut server = url::Url::parse(&login.server).map_err(|_| AccountsError::Unavailable)?;
        server.set_path(&format!(
            "{}/remote.php/dav/",
            server.path().trim_end_matches('/')
        ));
        let config = calendar::CalDavConfig {
            server_url: server.into(),
            username: login.login_name.clone(),
            ..Default::default()
        };
        let discovered = discover(
            &config,
            tilo_dav::Credentials::new(login.login_name.clone(), login.app_password.to_string()),
        )?;
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
        entries: vec![(account, Secret::password(login.app_password.as_str()))],
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
mod tests;
