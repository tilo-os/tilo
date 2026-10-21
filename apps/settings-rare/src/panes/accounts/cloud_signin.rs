//! Sign-in for the accounts that never take a typed password in Settings:
//! Google Drive, `OneDrive` and Dropbox (`OAuth` in the system browser),
//! Nextcloud (Login Flow v2 in the system browser) and `iCloud` Drive (Apple
//! ID, password, then the verification code).
//!
//! [`SignInFlow`] is the device-local state of one such sign-in. Network
//! steps run on worker threads; the pane's `sync` polls
//! [`SignInFlow::poll`] and persists the [`Completed`] result through its
//! transactional account save. The browser is opened through the desktop's
//! URI opener (`ui::portal`), never a spawned host tool. Google's files-only
//! path stays available; explicit service choices request Gmail only with
//! consent and validate its mailbox before either service account is saved.

use std::sync::Arc;

use account_custody::icloud::{ICloudAuthError, ICloudChallenge, ICloudSignIn};
use account_custody::nextcloud::{self, NextcloudError};
use account_custody::{Authorization, Custodian, CustodyError, LoopbackCancel, OAuthCloud};
use accounts::cloud_types::FilesMode;
use ui::widget::{Button, Checkbox, Dropdown, Row, Spacer, TextInput, TextInputKind};

use crate::accounts::AccountKind;
use crate::channel::{self, Receiver, TryRecvError};
use crate::i18n::t;
use crate::widgets::{Item, SettingsSection, field_row, note, row, rows, section};

/// Opens a URL in the person's browser.
pub trait BrowserOpener: Send + Sync {
    /// Start opening `url`; the outcome arrives on the channel.
    fn open(&self, url: &str) -> ui::portal::LaunchChannel;
}

/// The desktop's URI opener (the `OpenURI` portal inside the sandbox).
pub struct PortalOpener;

impl BrowserOpener for PortalOpener {
    fn open(&self, url: &str) -> ui::portal::LaunchChannel {
        ui::portal::send_open_uri(url)
    }
}

/// A finished sign-in, ready to be saved.
pub struct Completed {
    pub kind: AccountKind,
    pub existing_id: Option<String>,
    pub display_name: String,
    /// Non-secret connection settings merged into the account envelope.
    pub config: serde_json::Value,
    pub secret: accounts::Secret,
    /// A mailbox validated by Gmail, only after explicit mail consent.
    pub mail: Option<accounts::Account>,
}

/// A daemon freeze is released unless metadata was committed successfully.
struct ImportPreparation {
    socket: std::path::PathBuf,
    account: String,
    token: Option<String>,
    writer: Option<tilo_files::native::lock::FileLock>,
}

impl Drop for ImportPreparation {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            let socket = self.socket.clone();
            let account = self.account.clone();
            let writer = self.writer.take();
            std::thread::spawn(move || {
                let _writer = writer;
                if let Err(error) = runtime_fs::cloud::CloudClient::connect(socket)
                    .and_then(|mut client| client.abort_import(&account, &token))
                {
                    tracing::warn!(%error, "could not restore cloud sync after cancelled Import transition");
                }
            });
        }
    }
}

fn finish_import(mut preparation: Option<ImportPreparation>) -> Result<(), String> {
    if let Some(prepared) = preparation.as_mut() {
        let token = prepared.token.as_deref().ok_or("missing import token")?;
        runtime_fs::cloud::CloudClient::connect(&prepared.socket)
            .and_then(|mut client| client.complete_import(&prepared.account, token))
            .map_err(|error| error.to_string())?;
        prepared.token = None;
    }
    Ok(())
}

enum Outcome {
    Prepared(ImportPreparation, Box<Completed>),
    Saved(accounts::AccountId),
    NeedsAuthorization(OAuthCloud),
    Completed(Box<Completed>),
    NeedsCode(ICloudChallenge),
    /// The code was wrong; ask again for the same challenge.
    CodeRejected(ICloudChallenge, String),
    Failed(String),
}

impl Outcome {
    fn completed(completed: Completed) -> Self {
        Self::Completed(Box::new(completed))
    }
}

enum Cancel {
    Loopback(LoopbackCancel),
    Channel(std::sync::mpsc::Sender<()>),
}

enum Stage {
    Form,
    Waiting {
        /// A login URL still to be opened (Nextcloud learns it from the
        /// server first).
        urls: Option<Receiver<String>>,
        launch: Option<ui::portal::LaunchChannel>,
        result: Receiver<Outcome>,
        cancel: Option<Cancel>,
    },
    Code {
        challenge: ICloudChallenge,
        apple_id: String,
    },
    /// Every redirect port registered for the provider was taken, so the
    /// provider shows the code on its own page for the person to paste.
    PastedCode {
        cloud: OAuthCloud,
        authorization: Box<Authorization>,
    },
}

/// See the module docs.
pub struct SignInFlow {
    kind: AccountKind,
    existing_id: Option<String>,
    custodian: Custodian,
    opener: Arc<dyn BrowserOpener>,
    name: ui::LocalState<String>,
    server: ui::LocalState<String>,
    apple_id: ui::LocalState<String>,
    password: ui::LocalState<String>,
    code: ui::LocalState<String>,
    stage: Stage,
    /// An outcome received by [`SignInFlow::wait_for_test`], applied by the
    /// next [`SignInFlow::poll`].
    ready: Option<Outcome>,
    message: Option<String>,
    files: ui::LocalState<bool>,
    google_mail: ui::LocalState<bool>,
    files_mode: ui::LocalState<String>,
    original_mode: Option<FilesMode>,
    existing_config: serde_json::Value,
    expected_mail: Option<String>,
    preparation: Option<ImportPreparation>,
    saved_id: Option<accounts::AccountId>,
}

/// Choices captured before the browser starts, with reauthentication metadata.
struct SelectedServices {
    files: bool,
    mail: bool,
    mode: FilesMode,
    config: serde_json::Value,
    expected_mail: Option<String>,
}

/// The account configuration of an `OAuth` sign-in: the provider and its
/// storage as the provider describes it (drive ids, a team space root),
/// asked once with the fresh access token.
fn oauth_completed(
    custodian: &Custodian,
    cloud: OAuthCloud,
    kind: AccountKind,
    existing_id: Option<String>,
    display_name: String,
    tokens: &account_custody::TokenSet,
    selected: SelectedServices,
) -> Result<Outcome, CustodyError> {
    let kind = if cloud == OAuthCloud::GoogleDrive && !selected.files {
        AccountKind::Mail
    } else {
        kind
    };
    let mount = if kind == AccountKind::Mail {
        serde_json::Value::Null
    } else {
        custodian.resolve_storage(cloud, tokens)?
    };
    let mail = if selected.mail {
        let email = custodian.gmail_identity(tokens)?;
        if selected
            .expected_mail
            .as_deref()
            .is_some_and(|expected| !email.eq_ignore_ascii_case(expected))
        {
            return Err(CustodyError::SignIn(
                "Google returned a different mailbox; sign in with this account's email address"
                    .into(),
            ));
        }
        let config = accounts::email::AccountConfig {
            display_name: email.clone(),
            email: email.clone(),
            username: email.clone(),
            imap_host: "imap.gmail.com".into(),
            smtp_host: "smtp.gmail.com".into(),
            smtp_port: 587,
            auth: accounts::email::AuthMethod::OAuth,
            ..Default::default()
        };
        let mail_id = selected
            .config
            .get(accounts::cloud_types::MAIL_ACCOUNT_ID_KEY)
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&email);
        let mut account = accounts::Account::new(accounts::AccountId::from(mail_id), "email");
        account.display_name = email;
        account.config = config
            .to_value()
            .map_err(|error| CustodyError::Storage(error.to_string()))?;
        Some(account)
    } else {
        None
    };
    let mut config = selected.config;
    if !config.is_object() {
        config = serde_json::json!({});
    }
    if kind == AccountKind::Mail {
        let mailbox = mail
            .as_ref()
            .ok_or_else(|| CustodyError::SignIn("Gmail sign-in requires mail consent".into()))?;
        super::merge_config(&mut config, mailbox.config.clone());
    } else {
        super::merge_config(
            &mut config,
            serde_json::json!({
                "provider": cloud.type_id(),
                accounts::cloud_types::MOUNT_CONFIG_KEY: mount,
            }),
        );
        accounts::cloud_types::set_files_mode(&mut config, selected.mode);
        if let Some(mail) = &mail {
            super::merge_config(
                &mut config,
                serde_json::json!({
                    accounts::cloud_types::MAIL_ACCOUNT_ID_KEY: mail.id.to_string(),
                }),
            );
        }
    }
    Ok(Outcome::completed(Completed {
        kind,
        existing_id,
        display_name,
        config,
        secret: account_custody::oauth_secret(tokens),
        mail: if kind == AccountKind::Mail {
            None
        } else {
            mail
        },
    }))
}

/// The account configuration of an `iCloud` sign-in: the Apple ID and where
/// the storage daemon finds the account.
fn icloud_config(
    auth: &dyn account_custody::icloud::ICloudAuth,
    apple_id: &str,
) -> serde_json::Value {
    let mut config = serde_json::Map::new();
    config.insert("identity".into(), apple_id.into());
    let mount = auth.storage_config();
    if !mount.is_null() {
        config.insert(accounts::cloud_types::MOUNT_CONFIG_KEY.into(), mount);
    }
    serde_json::Value::Object(config)
}

/// Why this kind cannot start a sign-in in this build, if it cannot.
pub fn unavailable_reason(kind: AccountKind, custodian: &Custodian) -> Option<String> {
    let oauth_use = if kind == AccountKind::Mail {
        Some(account_custody::OAuthUse::mail(
            account_custody::OAuthIssuer::Google,
        ))
    } else {
        kind.oauth_cloud().map(OAuthCloud::oauth_use)
    };
    if let Some(oauth_use) = oauth_use {
        return match custodian.use_config(oauth_use) {
            Err(CustodyError::NotConfigured { provider, key }) => Some(format!(
                "{provider} sign-in is not configured in this build: it needs an OAuth client \
                 ID registered with {}, provided as {key} when Tilo is built or started.",
                oauth_use.issuer.label()
            )),
            _ => None,
        };
    }
    (kind == AccountKind::ICloudDrive && !custodian.icloud().is_available())
        .then(|| "iCloud Drive sign-in is not available in this build yet.".to_string())
}

impl SignInFlow {
    /// Whether the flow signs an account in again, rather than adding one.
    pub fn renews_an_account(&self) -> bool {
        self.existing_id.is_some()
    }

    pub fn new(
        kind: AccountKind,
        existing: Option<&accounts::Account>,
        custodian: Custodian,
        opener: Arc<dyn BrowserOpener>,
    ) -> Self {
        let text = |key: &str| {
            existing
                .and_then(|account| account.config.get(key))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        Self {
            kind,
            existing_id: existing.map(|account| account.id.to_string()),
            custodian,
            opener,
            name: ui::LocalState::new(
                existing
                    .map(|account| account.display_name.clone())
                    .unwrap_or_default(),
            ),
            server: ui::LocalState::new(text("server_url")),
            apple_id: ui::LocalState::new(text("identity")),
            password: ui::LocalState::new(String::new()),
            code: ui::LocalState::new(String::new()),
            stage: Stage::Form,
            ready: None,
            message: None,
            files: ui::LocalState::new(kind != AccountKind::Mail),
            google_mail: ui::LocalState::new(
                kind == AccountKind::Mail
                    || !text(accounts::cloud_types::MAIL_ACCOUNT_ID_KEY).is_empty(),
            ),
            files_mode: ui::LocalState::new(
                match existing.and_then(accounts::cloud_types::files_mode) {
                    Some(FilesMode::Import) => t("accounts-files-import"),
                    _ => t("accounts-files-sync"),
                },
            ),
            original_mode: existing.and_then(accounts::cloud_types::files_mode),
            existing_config: existing
                .map_or_else(|| serde_json::json!({}), |account| account.config.clone()),
            preparation: None,
            saved_id: None,
            expected_mail: existing
                .and_then(|account| {
                    account.config.get("email").or_else(|| {
                        account
                            .config
                            .get(accounts::cloud_types::MAIL_ACCOUNT_ID_KEY)
                    })
                })
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        }
    }

    /// Resolve a linked mailbox's stable identity through the canonical store.
    pub fn expect_mailbox(&mut self, email: Option<String>) {
        self.expected_mail = email;
    }

    /// Set the idle form's service choices for semantic actions.
    pub fn select_services(&self, files: bool, mail: bool, mode: FilesMode) -> Result<(), String> {
        if !matches!(self.stage, Stage::Form) {
            return Err("Service choices cannot change while sign-in is running".into());
        }
        self.validate_services(files, mail, mode)?;
        self.files.set(files);
        self.google_mail.set(mail);
        self.files_mode.set(match mode {
            FilesMode::Sync => t("accounts-files-sync"),
            FilesMode::Import => t("accounts-files-import"),
        });
        Ok(())
    }

    fn validate_services(&self, files: bool, mail: bool, mode: FilesMode) -> Result<(), String> {
        match self.kind {
            AccountKind::GoogleDrive if !files && !mail => {
                return Err("Select Files, Mail, or both before continuing".into());
            }
            AccountKind::Dropbox if !files || mail => {
                return Err("Dropbox requires Files and does not support Mail".into());
            }
            AccountKind::Mail if files || !mail => {
                return Err("This account requires Mail only".into());
            }
            AccountKind::GoogleDrive | AccountKind::Mail | AccountKind::Dropbox => {}
            _ if !files || mail || mode != FilesMode::Sync => {
                return Err("Service choices are not available for this account type".into());
            }
            _ => {}
        }
        if self.existing_id.is_some() && self.kind != AccountKind::Mail && !files {
            return Err("Disconnect this Files account using Remove account first; connect Mail separately to retain a mailbox".into());
        }
        if self
            .existing_config
            .get(accounts::cloud_types::MAIL_ACCOUNT_ID_KEY)
            .is_some()
            && !mail
        {
            return Err("Disconnect Mail using Remove account on the linked Mail account first, then manage Files here".into());
        }
        if self.existing_id.is_some()
            && self.kind != AccountKind::Mail
            && self.original_mode.is_none()
        {
            return Err(
                "This account has an unrecognized Files mode; repair it before signing in again"
                    .into(),
            );
        }
        Ok(())
    }

    fn unavailable_reason(&self) -> Option<String> {
        let kind =
            if self.kind == AccountKind::GoogleDrive && !self.files.get() && self.google_mail.get()
            {
                AccountKind::Mail
            } else {
                self.kind
            };
        unavailable_reason(kind, &self.custodian)
    }

    fn selected_services(&self) -> SelectedServices {
        SelectedServices {
            files: self.files.get(),
            mail: self.google_mail.get(),
            mode: if self.files_mode.get() == t("accounts-files-import") {
                FilesMode::Import
            } else {
                FilesMode::Sync
            },
            config: self.existing_config.clone(),
            expected_mail: self.expected_mail.clone(),
        }
    }

    /// A worker is running.
    pub fn busy(&self) -> bool {
        matches!(self.stage, Stage::Waiting { .. })
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn awaiting_code(&self) -> bool {
        matches!(self.stage, Stage::Code { .. } | Stage::PastedCode { .. })
    }

    #[doc(hidden)]
    pub fn set_fields_for_test(&self, name: &str, server: &str, apple_id: &str, password: &str) {
        self.name.set(name.into());
        self.server.set(server.into());
        self.apple_id.set(apple_id.into());
        self.password.set(password.into());
    }

    #[doc(hidden)]
    pub fn set_code_for_test(&self, code: &str) {
        self.code.set(code.into());
    }

    fn display_name(&self) -> String {
        let name = self.name.with(|name| name.trim().to_string());
        if name.is_empty() {
            self.kind.label().to_string()
        } else {
            name
        }
    }

    /// The primary action: start the sign-in, or verify the entered code.
    pub fn submit(&mut self) {
        self.submit_requested();
    }

    fn submit_requested(&mut self) {
        if self.busy() {
            return;
        }
        if matches!(self.stage, Stage::Form) {
            let selected = self.selected_services();
            if let Err(error) = self.validate_services(selected.files, selected.mail, selected.mode)
            {
                self.message = Some(error);
                return;
            }
        }
        self.message = None;
        if matches!(self.stage, Stage::PastedCode { .. }) {
            let Stage::PastedCode {
                cloud,
                authorization,
            } = std::mem::replace(&mut self.stage, Stage::Form)
            else {
                return;
            };
            self.redeem_pasted_code(cloud, *authorization);
            return;
        }
        if let Stage::Code {
            challenge,
            apple_id,
        } = &self.stage
        {
            let challenge = challenge.clone();
            let apple_id = apple_id.clone();
            self.verify_code(challenge, apple_id);
            return;
        }
        let selected = self.selected_services();
        let linked_mail = self
            .existing_config
            .get(accounts::cloud_types::MAIL_ACCOUNT_ID_KEY)
            .is_some();
        if self.existing_id.is_some()
            && self.original_mode.is_some_and(|mode| mode != selected.mode)
            && selected.files
            && selected.mail == linked_mail
        {
            self.save_mode_only();
            return;
        }
        if let Some(reason) = self.unavailable_reason() {
            self.preparation = None;
            self.message = Some(reason);
            return;
        }
        match if self.kind == AccountKind::Mail {
            Some(OAuthCloud::GoogleDrive)
        } else {
            self.kind.oauth_cloud()
        } {
            Some(cloud) => self.start_oauth(cloud),
            None if self.kind == AccountKind::Nextcloud => self.start_nextcloud(),
            None if self.kind == AccountKind::ICloudDrive => self.start_icloud(),
            None => self.message = Some("this account type does not sign in here".into()),
        }
    }

    /// Gmail consent is available only on Google's idle sign-in form.
    pub fn can_request_google_mail(&self) -> bool {
        self.kind == AccountKind::GoogleDrive && matches!(self.stage, Stage::Form)
    }

    /// Compatibility action opting into Files and the full Gmail IMAP/SMTP scope.
    pub fn submit_with_google_mail(&mut self) {
        if !self.can_request_google_mail() {
            return;
        }
        self.files.set(true);
        self.google_mail.set(true);
        self.submit_requested();
    }

    fn prepare_import(&mut self, completed: Box<Completed>) {
        let Some(account) = self.existing_id.clone() else {
            self.message = Some("Only an existing Sync account needs preparation".into());
            return;
        };
        let root = self
            .custodian
            .config_root()
            .map(std::path::Path::to_path_buf)
            .or_else(accounts::config_root);
        let (sender, result) = channel::sync_channel(1);
        let _ = crate::device_read::thread(
            "settings-cloud-sign-in",
            Box::new(move || {
                let prepared = runtime_fs::cloud::default_socket()
                    .ok_or_else(|| std::io::Error::other("cloud service is unavailable"))
                    .and_then(|socket| {
                        let root =
                            root.ok_or_else(|| std::io::Error::other("no account directory"))?;
                        let writer = runtime_fs::cloud::import_writer_lock(&root, &account)?;
                        let mut client = runtime_fs::cloud::CloudClient::connect(&socket)?;
                        let token = client.prepare_import(&account)?;
                        let prepared = ImportPreparation {
                            socket,
                            account,
                            token: Some(token),
                            writer: Some(writer),
                        };
                        client.begin_commit_import(
                            &prepared.account,
                            prepared.token.as_deref().unwrap_or_default(),
                        )?;
                        Ok(prepared)
                    });
                let _ = sender.send(match prepared {
                Ok(prepared) => Outcome::Prepared(prepared, completed),
                Err(error) => Outcome::Failed(format!(
                    "Cannot switch to Import safely: {error}. Pending uploads must finish before retrying. For an interrupted mode change, close this form and choose Recover mode change on the account."
                )),
            });
            }),
        );
        self.stage = Stage::Waiting {
            urls: None,
            launch: None,
            result,
            cancel: None,
        };
        self.message = Some("Preparing Import safely…".into());
    }

    fn save_mode_only(&mut self) {
        let custodian = self.custodian.clone();
        let Some(id) = self.existing_id.clone() else {
            self.message = Some("Connect this account before changing its Files mode".into());
            return;
        };
        let kind = self.kind;
        let display_name = self.display_name();
        let mut config = self.existing_config.clone();
        accounts::cloud_types::set_files_mode(&mut config, self.selected_services().mode);
        let (sender, result) = channel::sync_channel(1);
        let _ = crate::device_read::thread(
            "settings-cloud-sign-in",
            Box::new(move || {
                let outcome = (|| {
                    // No service was added: retain the approved grant, never ask for
                    // broader scopes merely to change local Files policy.
                    if let Some(cloud) = kind.oauth_cloud() {
                        let scopes =
                            custodian.oauth_scopes(&accounts::AccountId::from(id.as_str()))?;
                        let (provider, _) = custodian.use_config(cloud.oauth_use())?;
                        if !scopes.as_deref().is_some_and(|granted| {
                            provider.scopes.iter().all(|required| {
                                granted.split_whitespace().any(|scope| scope == required)
                            })
                        }) {
                            return Ok(Outcome::NeedsAuthorization(cloud));
                        }
                    }
                    let vault = custodian
                        .config_root()
                        .map_or_else(accounts::SecretVault::open, accounts::SecretVault::open_in);
                    if let Some(error) = vault.recovery_error() {
                        return Err(CustodyError::Storage(error.to_string()));
                    }
                    let secret = vault
                        .get(&accounts::AccountId::from(id.as_str()))
                        .ok_or(CustodyError::NeedsAuthentication)?;
                    Ok(Outcome::completed(Completed {
                        kind,
                        existing_id: Some(id),
                        display_name,
                        config,
                        secret,
                        mail: None,
                    }))
                })();
                let _ =
                    sender.send(outcome.unwrap_or_else(|error| Outcome::Failed(error.to_string())));
            }),
        );
        self.stage = Stage::Waiting {
            urls: None,
            launch: None,
            result,
            cancel: None,
        };
    }

    /// Finalize with the daemon on a worker, never on the UI thread.
    pub(super) fn finish_async(&mut self, id: accounts::AccountId) {
        let preparation = self.preparation.take();
        if preparation.is_none() {
            self.saved_id = Some(id);
            return;
        }
        let (sender, result) = channel::sync_channel(1);
        let _ = crate::device_read::thread(
            "settings-cloud-sign-in",
            Box::new(move || {
                let saved = finish_import(preparation).map(|()| id);
                let outcome = match saved {
                    Ok(id) => Outcome::Saved(id),
                    Err(error) => Outcome::Failed(format!(
                        "Could not save the mode change: {error}. Close this form and use Recover mode change on the account before retrying."
                    )),
                };
                let _ = sender.send(outcome);
            }),
        );
        self.stage = Stage::Waiting {
            urls: None,
            launch: None,
            result,
            cancel: None,
        };
        self.message = Some("Saving account mode safely…".into());
    }

    pub(super) fn take_saved(&mut self) -> Option<accounts::AccountId> {
        self.saved_id.take()
    }

    fn start_oauth(&mut self, cloud: OAuthCloud) {
        let request = if self.kind == AccountKind::Mail {
            self.custodian.begin_sign_in_for(
                account_custody::OAuthUse::mail(account_custody::OAuthIssuer::Google),
                self.expected_mail.as_deref(),
            )
        } else if cloud == OAuthCloud::GoogleDrive {
            self.custodian
                .begin_google_services_sign_in(self.files.get(), self.google_mail.get())
        } else {
            self.custodian.begin_sign_in(cloud, None)
        };
        let authorization = match request {
            Ok(authorization) => authorization,
            Err(error) => {
                self.preparation = None;
                self.message = Some(error.to_string());
                return;
            }
        };
        let launch = self.opener.open(authorization.url());
        let Some(canceller) = authorization.canceller() else {
            // The provider will show the code instead of redirecting.
            self.message = Some(format!(
                "Another program is using the ports Tilo listens on for {}'s answer. Sign in \
                 and allow access in your browser, then paste the code {} shows you here.",
                cloud.sign_in_with(),
                cloud.sign_in_with()
            ));
            self.code.set(String::new());
            self.stage = Stage::PastedCode {
                cloud,
                authorization: Box::new(authorization),
            };
            // The opener's outcome is not awaited here; a failure to open
            // shows as no code to paste.
            drop(launch);
            return;
        };
        let cancel = Cancel::Loopback(canceller);
        let (sender, result) = channel::sync_channel(1);
        let kind = self.kind;
        let existing_id = self.existing_id.clone();
        let display_name = self.display_name();
        let custodian = self.custodian.clone();
        let selected = self.selected_services();
        let _ = crate::device_read::thread(
            "settings-cloud-sign-in",
            Box::new(move || {
                let outcome = Custodian::finish_sign_in(authorization).and_then(|tokens| {
                    oauth_completed(
                        &custodian,
                        cloud,
                        kind,
                        existing_id,
                        display_name,
                        &tokens,
                        selected,
                    )
                });
                let _ =
                    sender.send(outcome.unwrap_or_else(|error| Outcome::Failed(error.to_string())));
            }),
        );

        self.message = Some(format!(
            "Continue in your browser: sign in with {} and allow access. This page updates when you are done.",
            cloud.sign_in_with()
        ));
        self.stage = Stage::Waiting {
            urls: None,
            launch: Some(launch),
            result,
            cancel: Some(cancel),
        };
    }

    fn redeem_pasted_code(&mut self, cloud: OAuthCloud, authorization: Authorization) {
        let code = self.code.with(|code| code.trim().to_string());
        if code.is_empty() {
            self.message = Some(format!(
                "paste the code {} showed you",
                cloud.sign_in_with()
            ));
            self.stage = Stage::PastedCode {
                cloud,
                authorization: Box::new(authorization),
            };
            return;
        }
        let (sender, result) = channel::sync_channel(1);
        let kind = self.kind;
        let existing_id = self.existing_id.clone();
        let display_name = self.display_name();
        let custodian = self.custodian.clone();
        let selected = self.selected_services();
        let _ = crate::device_read::thread(
            "settings-cloud-sign-in",
            Box::new(move || {
                let outcome =
                    Custodian::finish_sign_in_with_code(authorization, &code).and_then(|tokens| {
                        oauth_completed(
                            &custodian,
                            cloud,
                            kind,
                            existing_id,
                            display_name,
                            &tokens,
                            selected,
                        )
                    });
                let _ =
                    sender.send(outcome.unwrap_or_else(|error| Outcome::Failed(error.to_string())));
            }),
        );

        self.code.set(String::new());
        self.message = Some("Checking the code…".into());
        self.stage = Stage::Waiting {
            urls: None,
            launch: None,
            result,
            cancel: None,
        };
    }

    fn start_nextcloud(&mut self) {
        let server = self.server.with(|server| server.trim().to_string());
        if let Err(error) = nextcloud::server_url(&server) {
            self.message = Some(error.to_string());
            return;
        }
        let (url_sender, urls) = channel::sync_channel(1);
        let (sender, result) = channel::sync_channel(1);
        let (stop, cancel) = std::sync::mpsc::channel();
        let kind = self.kind;
        let existing_id = self.existing_id.clone();
        let display_name = self.display_name();
        let _ = crate::device_read::thread(
            "settings-cloud-sign-in",
            Box::new(move || {
                let outcome = nextcloud::begin(&server).and_then(|flow| {
                    let _ = url_sender.send(flow.login_url().to_string());
                    flow.wait(&cancel, nextcloud::POLL_INTERVAL, nextcloud::FLOW_LIFETIME)
                });
                let _ = sender.send(match outcome {
                    Ok(login) => Outcome::completed(Completed {
                        kind,
                        existing_id,
                        display_name: display_name.clone(),
                        config: serde_json::json!({
                            "label": display_name,
                            "server_url": login.server,
                            "username": login.login_name,
                            "app_password": true,
                        }),
                        secret: accounts::Secret::password(login.app_password.as_str()),
                        mail: None,
                    }),
                    Err(NextcloudError::Cancelled) => {
                        Outcome::Failed("sign-in was cancelled".into())
                    }
                    Err(error) => Outcome::Failed(error.to_string()),
                });
            }),
        );
        self.message = Some(
            "Contacting your Nextcloud server… Approve access in the browser page that opens; \
             Nextcloud then gives this device its own app password."
                .into(),
        );
        self.stage = Stage::Waiting {
            urls: Some(urls),
            launch: None,
            result,
            cancel: Some(Cancel::Channel(stop)),
        };
    }

    fn start_icloud(&mut self) {
        let apple_id = self.apple_id.with(|id| id.trim().to_string());
        let password = self.password.with(Clone::clone);
        // The password is used once for this request and never kept.
        self.password.set(String::new());
        if apple_id.is_empty() || password.is_empty() {
            self.message = Some("enter your Apple ID and password".into());
            return;
        }
        let auth = Arc::clone(self.custodian.icloud());
        let (sender, result) = channel::sync_channel(1);
        let kind = self.kind;
        let existing_id = self.existing_id.clone();
        let display_name = self.display_name();
        let _ = crate::device_read::thread(
            "settings-cloud-sign-in",
            Box::new(move || {
                let password = zeroize::Zeroizing::new(password);
                let outcome = match auth.sign_in(&apple_id, &password) {
                    Ok(ICloudSignIn::Complete(session)) => Outcome::completed(Completed {
                        kind,
                        existing_id,
                        display_name,
                        config: icloud_config(auth.as_ref(), &apple_id),
                        secret: account_custody::icloud_secret(&apple_id, &session),
                        mail: None,
                    }),
                    Ok(ICloudSignIn::NeedsCode(challenge)) => Outcome::NeedsCode(challenge),
                    Err(error) => Outcome::Failed(error.to_string()),
                };
                let _ = sender.send(outcome);
            }),
        );
        self.message = Some("Signing in to iCloud…".into());
        self.stage = Stage::Waiting {
            urls: None,
            launch: None,
            result,
            cancel: None,
        };
    }

    fn verify_code(&mut self, challenge: ICloudChallenge, apple_id: String) {
        let code: String = self
            .code
            .with(|code| code.chars().filter(char::is_ascii_digit).collect());
        if code.len() != 6 {
            self.message = Some("enter the six-digit verification code".into());
            return;
        }
        let auth = Arc::clone(self.custodian.icloud());
        let (sender, result) = channel::sync_channel(1);
        let kind = self.kind;
        let existing_id = self.existing_id.clone();
        let display_name = self.display_name();
        let _ = crate::device_read::thread(
            "settings-cloud-sign-in",
            Box::new(move || {
                let outcome = match auth.verify_code(&challenge, &code) {
                    Err(ICloudAuthError::InvalidCode) => {
                        Outcome::CodeRejected(challenge, ICloudAuthError::InvalidCode.to_string())
                    }
                    Ok(session) => Outcome::completed(Completed {
                        kind,
                        existing_id,
                        display_name,
                        config: icloud_config(auth.as_ref(), &apple_id),
                        secret: account_custody::icloud_secret(&apple_id, &session),
                        mail: None,
                    }),
                    Err(error) => Outcome::Failed(error.to_string()),
                };
                let _ = sender.send(outcome);
            }),
        );
        self.code.set(String::new());
        self.message = Some("Checking the code…".into());
        self.stage = Stage::Waiting {
            urls: None,
            launch: None,
            result,
            cancel: None,
        };
    }

    /// End a running step. The flow stays open at its form.
    pub fn cancel(&mut self) {
        self.preparation = None;
        self.ready = None;
        if let Stage::Waiting {
            cancel: Some(cancel),
            ..
        } = &self.stage
        {
            match cancel {
                Cancel::Loopback(loopback) => loopback.cancel(),
                Cancel::Channel(stop) => {
                    let _ = stop.send(());
                }
            }
        }
        self.stage = Stage::Form;
    }

    /// Advance running steps. Returns the result once, when the sign-in
    /// completed. Returns whether anything visible changed in `.1`.
    pub fn poll(&mut self) -> (Option<Completed>, bool) {
        let Stage::Waiting { urls, launch, .. } = &mut self.stage else {
            return (None, false);
        };
        let changed = if let Some(receiver) = urls
            && let Ok(url) = receiver.try_recv()
        {
            *launch = Some(self.opener.open(&url));
            *urls = None;
            self.message = Some(
                "Continue in your browser: sign in to Nextcloud and grant access. This page \
                 updates when you are done."
                    .into(),
            );
            true
        } else {
            false
        };
        let mut launch_failed = None;
        if let Some(channel) = launch {
            match channel.try_recv() {
                Ok(ui::portal::LaunchOutcome::Opened) | Err(TryRecvError::Disconnected) => {
                    *launch = None;
                }
                Ok(ui::portal::LaunchOutcome::Failed(error)) => launch_failed = Some(error),
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(error) = launch_failed {
            self.cancel();
            self.message = Some(format!(
                "Could not open your browser: {error}. Nothing was saved."
            ));
            return (None, true);
        }
        let Stage::Waiting { result, .. } = &mut self.stage else {
            return (None, changed);
        };
        let received = match self.ready.take() {
            Some(outcome) => Ok(outcome),
            None => result.try_recv(),
        };
        let outcome = match received {
            Ok(outcome) => outcome,
            Err(TryRecvError::Empty) => return (None, changed),
            Err(TryRecvError::Disconnected) => {
                Outcome::Failed("sign-in stopped unexpectedly".into())
            }
        };
        self.apply_outcome(outcome)
    }

    fn apply_outcome(&mut self, outcome: Outcome) -> (Option<Completed>, bool) {
        match outcome {
            Outcome::NeedsAuthorization(cloud) => {
                self.stage = Stage::Form;
                self.start_oauth(cloud);
                (None, true)
            }
            Outcome::Prepared(prepared, completed) => {
                self.stage = Stage::Form;
                self.preparation = Some(prepared);
                // Storing the account is the system's job: the sheet closes
                // when it lands and says why if it does not.
                self.message = None;
                (Some(*completed), true)
            }
            Outcome::Saved(id) => {
                self.stage = Stage::Form;
                self.saved_id = Some(id);
                (None, true)
            }
            Outcome::Completed(completed) => {
                if self.existing_id.is_some()
                    && self.original_mode == Some(FilesMode::Sync)
                    && completed
                        .config
                        .get("files_mode")
                        .and_then(serde_json::Value::as_str)
                        == Some("import")
                {
                    self.prepare_import(completed);
                    (None, true)
                } else {
                    self.stage = Stage::Form;
                    self.message = None;
                    (Some(*completed), true)
                }
            }
            Outcome::NeedsCode(challenge) => {
                self.stage = Stage::Code {
                    challenge,
                    apple_id: self.apple_id.with(|id| id.trim().to_string()),
                };
                self.message = Some(
                    "Apple sent a verification code to your trusted devices. Enter it to finish."
                        .into(),
                );
                (None, true)
            }
            Outcome::CodeRejected(challenge, error) => {
                self.stage = Stage::Code {
                    challenge,
                    apple_id: self.apple_id.with(|id| id.trim().to_string()),
                };
                self.message = Some(format!("{error}. Enter the newest code."));
                (None, true)
            }
            Outcome::Failed(error) => {
                self.preparation = None;
                self.stage = Stage::Form;
                self.message = Some(format!("Could not sign in: {error}"));
                (None, true)
            }
        }
    }

    /// Block until the running step reports (opening a login URL the
    /// worker learns on the way), keeping the outcome for the next
    /// [`Self::poll`]. Returns whether it reported before `timeout`.
    #[doc(hidden)]
    pub fn wait_for_test(&mut self, timeout: std::time::Duration) -> bool {
        let deadline = ui::time::now() + timeout;
        let remaining = || deadline.saturating_duration_since(ui::time::now());
        let Stage::Waiting {
            urls,
            launch,
            result,
            ..
        } = &mut self.stage
        else {
            return false;
        };
        if let Some(receiver) = urls.take() {
            match receiver.recv_timeout(remaining()) {
                Ok(url) => *launch = Some(self.opener.open(&url)),
                Err(channel::RecvTimeoutError::Timeout) => return false,
                // The worker failed before learning a URL; its outcome follows.
                Err(channel::RecvTimeoutError::Disconnected) => {}
            }
        }
        match result.recv_timeout(remaining()) {
            Ok(outcome) => {
                self.ready = Some(outcome);
                true
            }
            Err(_) => false,
        }
    }

    /// Report a failure to persist a completed sign-in.
    pub fn save_failed(&mut self, error: &str) {
        let changing_mode = self.preparation.take().is_some();
        self.message = Some(if changing_mode {
            format!(
                "Could not save the mode change: {error}. Restoring Sync. If Sync does not return, close this form and choose Recover mode change on the account."
            )
        } else {
            format!("Could not save the account: {error}")
        });
    }

    /// The flow's card.
    /// The sign-in card; `bind` gives each button its effect.
    pub fn section(
        &self,
        bind: &dyn Fn(Button, super::AccountsEffect) -> Button,
    ) -> SettingsSection {
        let title = format!(
            "{} {}",
            if self.existing_id.is_some() {
                if self.kind == AccountKind::ICloudDrive {
                    "Re-authenticate"
                } else {
                    "Sign in again to"
                }
            } else {
                "Connect"
            },
            if self.kind == AccountKind::GoogleDrive {
                t("accounts-google-provider")
            } else {
                self.kind.label().to_owned()
            }
        );
        let mut fields: Vec<Item> = Vec::new();
        let unavailable = self.unavailable_reason();
        let waiting = self.busy();
        if let Some(reason) = &unavailable {
            fields.push(note(reason));
        }
        if !waiting {
            match &self.stage {
                Stage::PastedCode { .. } => fields.push(field_row(
                    "Code",
                    Box::new(
                        TextInput::from_state(&self.code)
                            .with_kind(TextInputKind::Code)
                            .with_id("accounts.signin.code")
                            .with_accessible_name("Code from the provider")
                            .with_placeholder("Paste the code"),
                    ),
                )),
                Stage::Code { .. } => fields.push(field_row(
                    "Verification code",
                    Box::new(
                        TextInput::from_state(&self.code)
                            .with_kind(TextInputKind::Code)
                            .with_id("accounts.signin.code")
                            .with_accessible_name("Verification code")
                            .with_placeholder("123456"),
                    ),
                )),
                _ => self.form_fields(&mut fields),
            }
        }
        if let Some(message) = &self.message {
            fields.push(note(message));
        }
        if self.kind.best_effort() && unavailable.is_none() {
            fields.push(note(
                "Best effort: Apple offers no public iCloud Drive API, so Tilo uses the one \
                 iCloud.com uses. It can change without notice. Your Apple ID password is used \
                 only to sign in and is never stored.",
            ));
        }
        let mut buttons = Row::new();
        if unavailable.is_none() && !waiting {
            let label = match (&self.stage, self.kind) {
                (Stage::Code { .. } | Stage::PastedCode { .. }, _) => "Verify",
                (_, AccountKind::ICloudDrive) => "Sign in",
                _ => &t("accounts-services-continue"),
            };
            buttons = buttons.push(Box::new(bind(
                Button::new("settings.accounts.form.save", label)
                    .role(ui::widget::ButtonRole::Primary),
                super::AccountsEffect::SaveAccountForm,
            )));
        }
        buttons = buttons
            .push(Box::new(bind(
                Button::new("settings.accounts.form.cancel", &t("accounts-form-cancel"))
                    .role(ui::widget::ButtonRole::Secondary),
                super::AccountsEffect::CancelAccountForm,
            )))
            .push(Box::new(Spacer::new()));
        fields.push(row(Box::new(buttons)));
        section(&title, rows(fields))
    }

    fn form_fields(&self, fields: &mut Vec<Item>) {
        if matches!(self.kind, AccountKind::GoogleDrive | AccountKind::Dropbox) {
            fields.push(row(Box::new(
                Checkbox::from_state(&self.files)
                    .with_label(t("accounts-service-files"))
                    .with_id("accounts.signin.files"),
            )));
            if self.kind == AccountKind::GoogleDrive {
                fields.push(row(Box::new(
                    Checkbox::from_state(&self.google_mail)
                        .with_label(t("accounts-service-mail"))
                        .with_id("accounts.signin.mail"),
                )));
                fields.push(note(&t("accounts-google-mail-consent")));
            }
            fields.push(field_row(
                &t("accounts-files-mode"),
                Box::new(
                    Dropdown::from_state(
                        vec![t("accounts-files-sync"), t("accounts-files-import")],
                        &self.files_mode,
                    )
                    .with_id("accounts.signin.files-mode")
                    .with_accessible_name(t("accounts-files-mode")),
                ),
            ));
            fields.push(note(&t("accounts-files-mode-description")));
        }
        fields.push(field_row(
            "Display name",
            Box::new(
                TextInput::from_state(&self.name)
                    .with_kind(TextInputKind::Name)
                    .with_id("accounts.signin.name")
                    .with_accessible_name("Display name")
                    .with_placeholder(self.kind.label()),
            ),
        ));
        match self.kind {
            AccountKind::Nextcloud => {
                fields.push(field_row(
                    "Server",
                    Box::new(
                        TextInput::from_state(&self.server)
                            .with_kind(TextInputKind::Url)
                            .with_id("accounts.signin.server")
                            .with_accessible_name("Server URL")
                            .with_placeholder("https://cloud.example.com"),
                    ),
                ));
                fields.push(note(
                    "Continue opens your Nextcloud's sign-in page in the browser. After you \
                     approve, Nextcloud issues an app password for this device only; you can \
                     revoke it there under Security at any time.",
                ));
            }
            AccountKind::ICloudDrive => {
                fields.push(field_row(
                    "Apple ID",
                    Box::new(
                        TextInput::from_state(&self.apple_id)
                            .with_kind(TextInputKind::Email)
                            .with_id("accounts.signin.apple-id")
                            .with_accessible_name("Apple ID")
                            .with_placeholder("you@icloud.com"),
                    ),
                ));
                fields.push(field_row(
                    "Password",
                    Box::new(
                        TextInput::from_state(&self.password)
                            .with_kind(TextInputKind::Password)
                            .with_id("accounts.signin.password")
                            .with_accessible_name("Apple ID password")
                            .with_secret(true)
                            .with_placeholder("Password"),
                    ),
                ));
            }
            _ => {
                if let Some(cloud) = self.kind.oauth_cloud() {
                    fields.push(note(&format!(
                        "Continue opens {}'s sign-in page in your browser. Tilo never sees your \
                         password; it keeps a revocable access grant for selected services, sealed on \
                         this device.",
                        cloud.sign_in_with()
                    )));
                }
            }
        }
    }
}

#[cfg(test)]
mod service_choice_tests {
    use super::*;

    fn flow(kind: AccountKind, existing: Option<&accounts::Account>) -> SignInFlow {
        SignInFlow::new(
            kind,
            existing,
            Custodian::for_root(None),
            Arc::new(PortalOpener),
        )
    }

    #[test]
    fn service_choices_validate_and_default_to_files_sync() {
        let google = flow(AccountKind::GoogleDrive, None);
        assert!(google.selected_services().files);
        assert!(!google.selected_services().mail);
        assert_eq!(google.selected_services().mode, FilesMode::Sync);
        assert!(
            google
                .select_services(false, false, FilesMode::Sync)
                .is_err()
        );
        google
            .select_services(false, true, FilesMode::Import)
            .unwrap();
        assert!(!google.selected_services().files);
        assert!(google.selected_services().mail);
        let dropbox = flow(AccountKind::Dropbox, None);
        assert!(
            dropbox
                .select_services(true, true, FilesMode::Sync)
                .is_err()
        );
        assert!(
            dropbox
                .select_services(false, false, FilesMode::Sync)
                .is_err()
        );
        dropbox
            .select_services(true, false, FilesMode::Import)
            .unwrap();
    }

    #[test]
    fn reauthentication_retains_import_and_linked_mail_and_refuses_unsafe_transition() {
        let mut account = accounts::Account::new("drive", "google-drive");
        account.config = serde_json::json!({
            "files_mode": "import", "mail_account_id": "ada@example.org", "kept": true
        });
        let google = flow(AccountKind::GoogleDrive, Some(&account));
        assert_eq!(google.selected_services().mode, FilesMode::Import);
        assert!(google.selected_services().mail);
        assert_eq!(google.selected_services().config["kept"], true);
        assert_eq!(
            google.selected_services().expected_mail.as_deref(),
            Some("ada@example.org")
        );
        assert!(
            google
                .select_services(true, false, FilesMode::Import)
                .is_err()
        );
        google
            .select_services(true, true, FilesMode::Import)
            .unwrap();
        account.config = serde_json::json!({});
        let legacy = flow(AccountKind::GoogleDrive, Some(&account));
        assert_eq!(legacy.selected_services().mode, FilesMode::Sync);
        legacy
            .select_services(true, false, FilesMode::Import)
            .unwrap();
        // Selection is not permission to discard queued writes: submit must
        // first acquire a daemon freeze, even for legacy Sync metadata.
        assert!(legacy.preparation.is_none());
    }

    #[test]
    fn mail_only_completion_never_resolves_drive_and_validates_identity() {
        let server = test_http::Responder::start(vec![
            test_http::Reply::status(200)
                .header("Content-Type", "application/json")
                .body(r#"{"emailAddress":"ada@example.org"}"#),
            test_http::Reply::status(200)
                .header("Content-Type", "application/json")
                .body(r#"{"emailAddress":"other@example.org"}"#),
        ]);
        let custodian = Custodian::for_root(None)
            .with_gmail_profile_endpoint(format!("{}/profile", server.url()));
        // Only a mail scope: resolving a Drive mount would reject these tokens.
        let tokens = account_custody::TokenSet {
            scope: Some("https://mail.google.com/".into()),
            ..Default::default()
        };
        let mut google = flow(AccountKind::GoogleDrive, None);
        google
            .select_services(false, true, FilesMode::Sync)
            .unwrap();
        google.expected_mail = Some("ada@example.org".into());
        let completed = oauth_completed(
            &custodian,
            OAuthCloud::GoogleDrive,
            AccountKind::GoogleDrive,
            None,
            "Google".into(),
            &tokens,
            google.selected_services(),
        )
        .unwrap();
        let Outcome::Completed(completed) = completed else {
            panic!("sign-in did not complete")
        };
        assert_eq!(completed.kind, AccountKind::Mail);
        assert!(completed.mail.is_none());
        assert!(
            completed
                .config
                .get(accounts::cloud_types::MOUNT_CONFIG_KEY)
                .is_none()
        );
        assert_eq!(completed.config["email"], "ada@example.org");
        assert!(
            oauth_completed(
                &custodian,
                OAuthCloud::GoogleDrive,
                AccountKind::GoogleDrive,
                None,
                "Google".into(),
                &tokens,
                google.selected_services()
            )
            .is_err()
        );
    }
}
