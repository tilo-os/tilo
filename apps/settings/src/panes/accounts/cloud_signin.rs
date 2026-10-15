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
//! path stays available; a separate, explicit consent action requests Gmail
//! too and validates its mailbox before either service account is saved.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use account_custody::icloud::{ICloudAuthError, ICloudChallenge, ICloudSignIn};
use account_custody::nextcloud::{self, NextcloudError};
use account_custody::{Authorization, Custodian, CustodyError, LoopbackCancel, OAuthCloud};
use ui::widget::{Button, Row, Spacer, TextInput, TextInputKind};

use crate::accounts::AccountKind;
use crate::i18n::t;
use crate::widgets::{Item, SettingsSection, card, field_row, note, row, rows};

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

enum Outcome {
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
    Channel(mpsc::Sender<()>),
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
    google_mail: bool,
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
    google_mail: bool,
    expected_mail: Option<&str>,
) -> Result<Outcome, CustodyError> {
    let mount = if kind == AccountKind::Mail {
        serde_json::Value::Null
    } else {
        custodian.resolve_storage(cloud, tokens)?
    };
    let mail = if google_mail {
        let email = custodian.gmail_identity(tokens)?;
        if expected_mail.is_some_and(|expected| !email.eq_ignore_ascii_case(expected)) {
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
        let mut account = accounts::Account::new(accounts::AccountId::from(email.clone()), "email");
        account.display_name = email;
        account.set_config(&config);
        Some(account)
    } else {
        None
    };
    Ok(Outcome::completed(Completed {
        kind,
        existing_id,
        display_name,
        config: if kind == AccountKind::Mail {
            mail.as_ref()
                .ok_or_else(|| CustodyError::SignIn("Gmail sign-in requires mail consent".into()))?
                .config
                .clone()
        } else {
            serde_json::json!({
                "provider": cloud.type_id(),
                accounts::cloud_types::MOUNT_CONFIG_KEY: mount,
            })
        },
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
            google_mail: kind == AccountKind::Mail,
            expected_mail: existing
                .and_then(|account| account.config.get("email"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
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
        if !matches!(self.stage, Stage::PastedCode { .. }) {
            self.google_mail = self.kind == AccountKind::Mail;
        }
        self.submit_requested();
    }

    fn submit_requested(&mut self) {
        if self.busy() {
            return;
        }
        if let Some(reason) = unavailable_reason(self.kind, &self.custodian) {
            self.message = Some(reason);
            return;
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

    /// Explicit opt-in to the full Gmail IMAP/SMTP scope, separate from files-only consent.
    pub fn submit_with_google_mail(&mut self) {
        if !self.can_request_google_mail() {
            return;
        }
        self.google_mail = true;
        self.submit_requested();
    }

    fn start_oauth(&mut self, cloud: OAuthCloud) {
        let request = if self.kind == AccountKind::Mail {
            self.custodian.begin_sign_in_for(
                account_custody::OAuthUse::mail(account_custody::OAuthIssuer::Google),
                self.expected_mail.as_deref(),
            )
        } else if cloud == OAuthCloud::GoogleDrive {
            self.custodian.begin_google_sign_in(self.google_mail)
        } else {
            self.custodian.begin_sign_in(cloud, None)
        };
        let authorization = match request {
            Ok(authorization) => authorization,
            Err(error) => {
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
        let (sender, result) = mpsc::sync_channel(1);
        let kind = self.kind;
        let existing_id = self.existing_id.clone();
        let display_name = self.display_name();
        let custodian = self.custodian.clone();
        let google_mail = self.google_mail;
        let expected_mail = self.expected_mail.clone();
        std::thread::spawn(move || {
            let outcome = Custodian::finish_sign_in(authorization).and_then(|tokens| {
                oauth_completed(
                    &custodian,
                    cloud,
                    kind,
                    existing_id,
                    display_name,
                    &tokens,
                    google_mail,
                    expected_mail.as_deref(),
                )
            });
            let _ = sender.send(outcome.unwrap_or_else(|error| Outcome::Failed(error.to_string())));
        });
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
        let (sender, result) = mpsc::sync_channel(1);
        let kind = self.kind;
        let existing_id = self.existing_id.clone();
        let display_name = self.display_name();
        let custodian = self.custodian.clone();
        let google_mail = self.google_mail;
        let expected_mail = self.expected_mail.clone();
        std::thread::spawn(move || {
            let outcome =
                Custodian::finish_sign_in_with_code(authorization, &code).and_then(|tokens| {
                    oauth_completed(
                        &custodian,
                        cloud,
                        kind,
                        existing_id,
                        display_name,
                        &tokens,
                        google_mail,
                        expected_mail.as_deref(),
                    )
                });
            let _ = sender.send(outcome.unwrap_or_else(|error| Outcome::Failed(error.to_string())));
        });
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
        let (url_sender, urls) = mpsc::sync_channel(1);
        let (sender, result) = mpsc::sync_channel(1);
        let (stop, cancel) = mpsc::channel();
        let kind = self.kind;
        let existing_id = self.existing_id.clone();
        let display_name = self.display_name();
        std::thread::spawn(move || {
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
                Err(NextcloudError::Cancelled) => Outcome::Failed("sign-in was cancelled".into()),
                Err(error) => Outcome::Failed(error.to_string()),
            });
        });
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
        let (sender, result) = mpsc::sync_channel(1);
        let kind = self.kind;
        let existing_id = self.existing_id.clone();
        let display_name = self.display_name();
        std::thread::spawn(move || {
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
        });
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
        let (sender, result) = mpsc::sync_channel(1);
        let kind = self.kind;
        let existing_id = self.existing_id.clone();
        let display_name = self.display_name();
        std::thread::spawn(move || {
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
        });
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
        match outcome {
            Outcome::Completed(completed) => {
                self.stage = Stage::Form;
                self.message = Some("Saving…".into());
                (Some(*completed), true)
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
                Err(mpsc::RecvTimeoutError::Timeout) => return false,
                // The worker failed before learning a URL; its outcome follows.
                Err(mpsc::RecvTimeoutError::Disconnected) => {}
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
        self.message = Some(format!("Could not save the account: {error}"));
    }

    /// The flow's card.
    /// The sign-in card; `bind` gives each button its effect.
    pub fn card(&self, bind: &dyn Fn(Button, super::AccountsEffect) -> Button) -> SettingsSection {
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
            self.kind.label()
        );
        let mut fields: Vec<Item> = Vec::new();
        let unavailable = unavailable_reason(self.kind, &self.custodian);
        let waiting = self.busy();
        if let Some(reason) = &unavailable {
            fields.push(note(reason));
        } else if !waiting {
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
                _ => "Continue in browser",
            };
            buttons = buttons.push(Box::new(bind(
                Button::new("settings.accounts.form.save", label),
                super::AccountsEffect::SaveAccountForm,
            )));
            if self.can_request_google_mail() {
                fields.push(note(&t("accounts-google-mail-consent")));
                buttons = buttons.push(Box::new(bind(
                    Button::new(
                        "settings.accounts.google-mail",
                        &t("accounts-google-mail-connect"),
                    ),
                    super::AccountsEffect::ConnectGoogleMail,
                )));
            }
        }
        buttons = buttons
            .push(Box::new(bind(
                Button::new("settings.accounts.form.cancel", "Cancel"),
                super::AccountsEffect::CancelAccountForm,
            )))
            .push(Box::new(Spacer::new()));
        fields.push(row(Box::new(buttons)));
        card(&title, rows(fields))
    }

    fn form_fields(&self, fields: &mut Vec<Item>) {
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
                         password; it keeps a revocable access grant for your files, sealed on \
                         this device.",
                        cloud.sign_in_with()
                    )));
                }
            }
        }
    }
}
