//! Sign-in for the accounts Settings never takes a typed password for: Nextcloud through its
//! Login Flow v2, the cloud drives through their provider's own page (Google Drive, `OneDrive`,
//! Dropbox), and `iCloud` Drive with an Apple ID and a verification code. The page is opened in
//! the person's browser; the code exchange runs where the network is (the session's account
//! service when Settings runs in one, whose sandbox has none, else this program).

use std::sync::Arc;

use account_credentials::icloud::{ICloudAuthError, ICloudChallenge, ICloudSignIn};
use account_credentials::nextcloud::{self, NextcloudError};
use account_credentials::{
    Authorization, CredentialError, CredentialStore, LoopbackCancel, OAuthCloud,
};
use accounts::cloud_types::FilesMode;
use ui::widget::{Button, Dropdown, Row, Spacer, TextInput, TextInputKind};

use crate::account_probe::connect::{CloudProgress, Connection};
use crate::accounts::AccountKind;
use crate::channel::{self, Receiver, TryRecvError};
use crate::i18n::{t, t_args};
use provider_settings_ui::widgets::{Item, SettingsSection, field_row, note, row, rows, section};

/// Set a connection setting of a config that is, or becomes, a JSON object.
fn set(config: &mut serde_json::Value, key: &str, value: serde_json::Value) {
    if !config.is_object() {
        *config = serde_json::json!({});
    }
    if let Some(map) = config.as_object_mut() {
        map.insert(key.to_owned(), value);
    }
}

/// How long between polls of a sign-in the person approves in the browser.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(400);
/// How long a sign-in may take before it is given up.
const SIGN_IN_LIFETIME: std::time::Duration = std::time::Duration::from_secs(300);

pub trait BrowserOpener: Send + Sync {
    fn open(&self, url: &str) -> ui::portal::LaunchChannel;
}

pub struct PortalOpener;

impl BrowserOpener for PortalOpener {
    fn open(&self, url: &str) -> ui::portal::LaunchChannel {
        ui::portal::send_open_uri(url)
    }
}

/// A sign-in the person approved: what is kept for the account.
pub struct Completed {
    pub kind: AccountKind,
    pub existing_id: Option<String>,
    pub display_name: String,
    pub config: serde_json::Value,
    pub secret: accounts::Secret,
}

/// Copying a drive's files into the workspace once, through the device's migrations: the whole
/// drive is chosen, then brought into a workspace folder named after it, where it syncs as the
/// workspace's own files.
pub(super) struct CopyOnce {
    pub(super) account: String,
    source: &'static str,
    found: Option<(String, String)>,
    finding: Option<ui::CatalogueCall>,
    /// How many of its steps were asked: the choice, then the copy.
    asked: usize,
    running: Option<ui::InvocationCall>,
    line: Option<String>,
    ended: Option<Result<(), String>>,
}

impl CopyOnce {
    fn new(kind: AccountKind, account: String) -> Option<Self> {
        let source = match kind {
            AccountKind::GoogleDrive => "gdrive",
            AccountKind::OneDrive => "onedrive",
            AccountKind::Dropbox => "dropbox",
            AccountKind::ICloudDrive => "icloud",
            _ => return None,
        };
        Some(Self {
            account,
            source,
            found: None,
            finding: None,
            asked: 0,
            running: None,
            line: None,
            ended: None,
        })
    }

    pub(super) const fn going(&self) -> bool {
        self.ended.is_none()
    }

    /// Move the copy on through `remote`; whether what it shows changed.
    pub(super) fn poll(&mut self, remote: &ui::RemoteActions) -> bool {
        use ui::Arrival;
        use ui::tilo_actions::InvocationUpdate;
        use ui::tilo_actions::remote::{InvokeArgument, migrations};
        if !self.going() {
            return false;
        }
        if self.found.is_none() {
            let finding = self
                .finding
                .get_or_insert_with(|| remote.catalogue(migrations::query()));
            match finding.get() {
                Arrival::Pending { .. } => return false,
                Arrival::Ready(page) => match migrations::found(&page.rows) {
                    Some(found) => self.found = Some(found),
                    None => {
                        self.ended = Some(Err(t("accounts-copy-unavailable")));
                        return true;
                    }
                },
                Arrival::Failed(error) => {
                    self.ended = Some(Err(error.to_string()));
                    return true;
                }
            }
        }
        let Some(found) = &self.found else {
            return false;
        };
        if let Some(running) = &self.running {
            match running.get() {
                Arrival::Ready(InvocationUpdate::Progress { message, .. }) => {
                    let changed = message.is_some() && message != self.line;
                    if changed {
                        self.line = message;
                    }
                    return changed;
                }
                Arrival::Ready(InvocationUpdate::Succeeded { .. }) => self.running = None,
                Arrival::Ready(InvocationUpdate::Failed { error, .. }) => {
                    self.ended = Some(Err(error.message));
                    return true;
                }
                Arrival::Ready(InvocationUpdate::Cancelled { message, .. }) => {
                    self.ended = Some(Err(message.unwrap_or_default()));
                    return true;
                }
                Arrival::Failed(error) => {
                    self.ended = Some(Err(error.to_string()));
                    return true;
                }
                Arrival::Pending { .. } | Arrival::Ready(_) => return false,
            }
        }
        let mut arguments = serde_json::Map::new();
        arguments.insert("source".into(), self.source.into());
        arguments.insert("account".into(), self.account.clone().into());
        let step = match self.asked {
            0 => {
                // `path:` is the drive's top: everything in it.
                arguments.insert("keys".into(), serde_json::json!(["path:"]));
                arguments.insert("only".into(), true.into());
                migrations::CHOOSE
            }
            1 => {
                arguments.insert("yes".into(), true.into());
                migrations::START
            }
            _ => {
                self.ended = Some(Ok(()));
                return true;
            }
        };
        self.asked += 1;
        self.running = Some(remote.invoke(
            migrations::target(found, step),
            InvokeArgument::Json(arguments),
            None,
        ));
        true
    }

    /// What the account's row says of the copy.
    pub(super) fn shown(&self) -> String {
        match &self.ended {
            None => self.line.as_ref().map_or_else(
                || t("accounts-copy-going"),
                |line| t_args("accounts-copy-going-line", &[("line", line.clone().into())]),
            ),
            Some(Ok(())) => t("accounts-copy-done"),
            Some(Err(error)) => t_args("accounts-copy-failed", &[("error", error.clone().into())]),
        }
    }
}

/// A sign-in that needs a code before it can finish.
enum CodeStage {
    /// Apple sent a verification code to the person's trusted devices.
    Icloud {
        challenge: ICloudChallenge,
        apple_id: String,
    },
    /// The provider shows the code on its own page; the person pastes it here.
    Pasted {
        cloud: OAuthCloud,
        authorization: Box<Authorization>,
    },
    /// The session follows the sign-in and asked for the code.
    Session {
        session: Arc<Connection>,
        message: String,
    },
}

enum Outcome {
    Completed(Box<Completed>),
    NeedsCode(Box<CodeStage>),
    CodeRejected(Box<CodeStage>, String),
    Failed(String),
}

enum Cancel {
    Loopback(LoopbackCancel),
    Channel(std::sync::mpsc::Sender<()>),
}

enum Stage {
    Form,
    Waiting {
        /// The page still to be opened, and whether the session already shows it in a window it
        /// closes once the sign-in is over.
        urls: Option<Receiver<(String, bool)>>,
        launch: Option<ui::portal::LaunchChannel>,
        result: Receiver<Outcome>,
        cancel: Option<Cancel>,
    },
    Code(Box<CodeStage>),
}

pub struct SignInFlow {
    kind: AccountKind,
    existing_id: Option<String>,
    credential_store: CredentialStore,
    opener: Arc<dyn BrowserOpener>,
    name: ui::LocalState<String>,
    server: ui::LocalState<String>,
    apple_id: ui::LocalState<String>,
    password: ui::LocalState<String>,
    code: ui::LocalState<String>,
    files_mode: ui::LocalState<String>,
    original_mode: Option<FilesMode>,
    existing_config: serde_json::Value,
    stage: Stage,
    ready: Option<Outcome>,
    message: Option<String>,
}

impl SignInFlow {
    pub fn renews_an_account(&self) -> bool {
        self.existing_id.is_some()
    }

    pub fn new(
        kind: AccountKind,
        existing: Option<&accounts::Account>,
        credential_store: CredentialStore,
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
            credential_store,
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
            files_mode: ui::LocalState::new(
                match existing.and_then(accounts::cloud_types::files_mode) {
                    Some(FilesMode::Import) => t("accounts-files-import"),
                    _ => t("accounts-files-sync"),
                },
            ),
            original_mode: existing.and_then(accounts::cloud_types::files_mode),
            existing_config: existing
                .map_or_else(|| serde_json::json!({}), |account| account.config.clone()),
            stage: Stage::Form,
            ready: None,
            message: None,
        }
    }

    pub fn busy(&self) -> bool {
        matches!(self.stage, Stage::Waiting { .. })
    }

    pub fn awaiting_code(&self) -> bool {
        matches!(self.stage, Stage::Code(_))
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn kind(&self) -> AccountKind {
        self.kind
    }

    /// Why this kind cannot sign in in this build, when it cannot.
    pub fn unavailable_reason(&self) -> Option<String> {
        unavailable_reason(self.kind, &self.credential_store)
    }

    #[doc(hidden)]
    pub fn set_fields_for_test(&self, name: &str, server: &str) {
        self.name.set(name.into());
        self.server.set(server.into());
    }

    #[doc(hidden)]
    pub fn set_apple_fields_for_test(&self, apple_id: &str, password: &str) {
        self.apple_id.set(apple_id.into());
        self.password.set(password.into());
    }

    #[doc(hidden)]
    pub fn set_code_for_test(&self, code: &str) {
        self.code.set(code.into());
    }

    /// Choose whether the account keeps its files in sync or is only copied from.
    pub fn set_files_mode(&self, mode: FilesMode) {
        self.files_mode.set(match mode {
            FilesMode::Sync => t("accounts-files-sync"),
            FilesMode::Import => t("accounts-files-import"),
        });
    }

    pub fn selected_mode(&self) -> FilesMode {
        if self.files_mode.get() == t("accounts-files-import") {
            FilesMode::Import
        } else {
            FilesMode::Sync
        }
    }

    fn display_name(&self) -> String {
        let name = self.name.with(|name| name.trim().to_string());
        if name.is_empty() {
            kind_shown(self.kind)
        } else {
            name
        }
    }

    pub fn submit(&mut self) {
        if self.busy() {
            return;
        }
        if let Stage::Code(stage) = std::mem::replace(&mut self.stage, Stage::Form) {
            self.submit_code(*stage);
            return;
        }
        self.message = None;
        match self.kind {
            AccountKind::Nextcloud => self.start_nextcloud(),
            AccountKind::ICloudDrive => self.start_icloud(),
            AccountKind::GoogleDrive | AccountKind::OneDrive | AccountKind::Dropbox => {
                if let Some(reason) = self.unavailable_reason() {
                    self.message = Some(reason);
                    return;
                }
                if self.only_the_mode_changed() {
                    self.save_mode_only();
                } else {
                    self.start_oauth();
                }
            }
            AccountKind::Mail | AccountKind::CalDav | AccountKind::WebDav => {
                self.message = Some("this account type does not sign in here".into());
            }
        }
    }

    /// Whether signing in again would only change an existing cloud account's Files mode: no new
    /// consent, the grant it already has is enough.
    fn only_the_mode_changed(&self) -> bool {
        self.existing_id.is_some()
            && self
                .original_mode
                .is_some_and(|mode| mode != self.selected_mode())
    }

    /// The mode-only change: keep the account's configuration and grant, write the mode.
    fn save_mode_only(&mut self) {
        let Some(id) = self.existing_id.clone() else {
            self.message = Some("Connect this account before changing its Files mode".into());
            return;
        };
        let cloud = self.kind.oauth_cloud();
        let kind = self.kind;
        let existing = self.existing_config.clone();
        let mode = self.selected_mode();
        let store = self.credential_store.clone();
        let display_name = self.display_name();
        let (sender, result) = channel::sync_channel(1);
        let _ = crate::device_read::thread(
            "settings-cloud-sign-in",
            Box::new(move || {
                let outcome = (|| {
                    // Neither mode asks the provider for more than the other: the grant this
                    // account already has is the one both use.
                    let vault = accounts::SecretVault::open_person(store.config_root());
                    if let Some(error) = vault.recovery_error() {
                        return Err(CredentialError::Storage(error.to_string()));
                    }
                    let secret = vault
                        .get(&accounts::AccountId::from(id.as_str()))
                        .ok_or(CredentialError::NeedsAuthentication)?;
                    let mut config = existing;
                    if config.get("provider").is_none()
                        && let Some(cloud) = cloud
                    {
                        set(&mut config, "provider", cloud.type_id().into());
                    }
                    accounts::cloud_types::set_files_mode(&mut config, mode);
                    Ok(Outcome::Completed(Box::new(Completed {
                        kind,
                        existing_id: Some(id),
                        display_name,
                        config,
                        secret,
                    })))
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
            "settings-nextcloud-sign-in",
            Box::new(move || {
                let page = |url: String, shown: bool| {
                    let _ = url_sender.send((url, shown));
                };
                // Settings' sandbox has no network: the session follows the sign-in where it
                // connects for Settings, and Settings does only where it has a network itself.
                let outcome = match crate::account_probe::session() {
                    Some(session) => session
                        .nextcloud_sign_in(
                            &server,
                            page,
                            &cancel,
                            nextcloud::POLL_INTERVAL,
                            nextcloud::FLOW_LIFETIME,
                        )
                        .map(|login| (login.server, login.login_name, login.app_password)),
                    None => nextcloud::begin(&server)
                        .and_then(|flow| {
                            page(flow.login_url().to_string(), false);
                            flow.wait(&cancel, nextcloud::POLL_INTERVAL, nextcloud::FLOW_LIFETIME)
                        })
                        .map(|login| (login.server, login.login_name, login.app_password))
                        .map_err(|error| match error {
                            NextcloudError::Cancelled => "sign-in was cancelled".to_owned(),
                            error => error.to_string(),
                        }),
                };
                let _ = sender.send(match outcome {
                    Ok((server, login_name, app_password)) => {
                        Outcome::Completed(Box::new(Completed {
                            kind,
                            existing_id,
                            display_name: display_name.clone(),
                            config: serde_json::json!({
                                "label": display_name,
                                "server_url": server,
                                "username": login_name,
                                "app_password": true,
                            }),
                            secret: accounts::Secret::password(app_password.as_str()),
                        }))
                    }
                    Err(error) => Outcome::Failed(error),
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

    fn start_oauth(&mut self) {
        let Some(cloud) = self.kind.oauth_cloud() else {
            self.message = Some("this account type does not sign in on its provider's page".into());
            return;
        };
        if let Some(session) = crate::account_probe::session() {
            self.start_oauth_through(session);
            return;
        }
        let authorization = match self.credential_store.begin_sign_in(cloud, None) {
            Ok(authorization) => authorization,
            Err(error) => {
                self.message = Some(error.to_string());
                return;
            }
        };
        let url = authorization.url().to_owned();
        let Some(canceller) = authorization.canceller() else {
            // The provider shows the code instead of redirecting back.
            self.message = Some(t_args(
                "accounts-sign-in-pasted-waiting",
                &[("service", cloud.sign_in_with().into())],
            ));
            self.code.set(String::new());
            let _launch = self.opener.open(&url);
            self.stage = Stage::Code(Box::new(CodeStage::Pasted {
                cloud,
                authorization: Box::new(authorization),
            }));
            return;
        };
        let _launch = self.opener.open(&url);
        let (sender, result) = channel::sync_channel(1);
        let store = self.credential_store.clone();
        let kind = self.kind;
        let existing_id = self.existing_id.clone();
        let display_name = self.display_name();
        let mode = self.selected_mode();
        let existing_config = self.existing_config.clone();
        let _ = crate::device_read::thread(
            "settings-cloud-sign-in",
            Box::new(move || {
                let outcome = CredentialStore::finish_sign_in(authorization)
                    .and_then(|tokens| {
                        oauth_completed(
                            &store,
                            cloud,
                            kind,
                            existing_id,
                            display_name,
                            &tokens,
                            mode,
                            existing_config,
                        )
                    })
                    .map_or_else(
                        |error| Outcome::Failed(error.to_string()),
                        |completed| Outcome::Completed(Box::new(completed)),
                    );
                let _ = sender.send(outcome);
            }),
        );
        self.message = Some(t_args(
            "accounts-sign-in-browser",
            &[("service", cloud.sign_in_with().into())],
        ));
        self.stage = Stage::Waiting {
            urls: None,
            launch: None,
            result,
            cancel: Some(Cancel::Loopback(canceller)),
        };
    }

    fn start_oauth_through(&mut self, session: Arc<Connection>) {
        let provider = self.kind.type_id().to_owned();
        let label = self.display_name();
        let kind = self.kind;
        let existing_id = self.existing_id.clone();
        let mode = self.selected_mode();
        let existing_config = self.existing_config.clone();
        let (url_sender, urls) = channel::sync_channel(1);
        let (sender, result) = channel::sync_channel(1);
        let (stop, cancel) = std::sync::mpsc::channel();
        let _ = crate::device_read::thread(
            "settings-cloud-sign-in",
            Box::new(move || {
                let page = |url: String, shown: bool| {
                    let _ = url_sender.send((url, shown));
                };
                let Followed { progress, session } = follow_session(
                    session,
                    |session| session.cloud_sign_in(&provider, &label, None, None, None),
                    page,
                    &cancel,
                );
                let outcome = match progress {
                    Ok(CloudProgress::SignedIn(signed_in)) => Outcome::Completed(Box::new(
                        cloud_completed(signed_in, kind, existing_id, label, mode, existing_config),
                    )),
                    Ok(CloudProgress::Code { message }) => match session {
                        Some(session) => {
                            Outcome::NeedsCode(Box::new(CodeStage::Session { session, message }))
                        }
                        None => Outcome::Failed("the session did not keep the sign-in".into()),
                    },
                    Ok(CloudProgress::Page { .. } | CloudProgress::Waiting) => {
                        Outcome::Failed("the session did not finish the sign-in".into())
                    }
                    Err(error) => Outcome::Failed(error),
                };
                let _ = sender.send(outcome);
            }),
        );
        self.message = Some(t_args(
            "accounts-sign-in-browser",
            &[("service", kind_shown(self.kind).into())],
        ));
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
        let kind = self.kind;
        let existing_id = self.existing_id.clone();
        let display_name = self.display_name();
        let mode = self.selected_mode();
        let existing_config = self.existing_config.clone();
        let (sender, result) = channel::sync_channel(1);
        if let Some(session) = crate::account_probe::session() {
            let provider = accounts::cloud_types::ICLOUD_DRIVE.to_owned();
            let _ = crate::device_read::thread(
                "settings-cloud-sign-in",
                Box::new(move || {
                    let password = zeroize::Zeroizing::new(password);
                    let outcome = match session.cloud_sign_in(
                        &provider,
                        &display_name,
                        Some(&apple_id),
                        Some(&password),
                        None,
                    ) {
                        Ok(CloudProgress::SignedIn(signed_in)) => {
                            Outcome::Completed(Box::new(cloud_completed(
                                signed_in,
                                kind,
                                existing_id,
                                display_name,
                                mode,
                                existing_config,
                            )))
                        }
                        Ok(CloudProgress::Code { message }) => {
                            Outcome::NeedsCode(Box::new(CodeStage::Session { session, message }))
                        }
                        Ok(_) => Outcome::Failed("the session did not start the sign-in".into()),
                        Err(error) => Outcome::Failed(error),
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
            return;
        }
        let auth = Arc::clone(self.credential_store.icloud());
        let _ = crate::device_read::thread(
            "settings-cloud-sign-in",
            Box::new(move || {
                let password = zeroize::Zeroizing::new(password);
                let outcome = match auth.sign_in(&apple_id, &password) {
                    Ok(ICloudSignIn::Complete(session)) => {
                        let config = icloud_config(auth.as_ref(), &apple_id, mode);
                        Outcome::Completed(Box::new(Completed {
                            kind,
                            existing_id,
                            display_name,
                            config,
                            secret: account_credentials::icloud_secret(&apple_id, &session),
                        }))
                    }
                    Ok(ICloudSignIn::NeedsCode(challenge)) => {
                        Outcome::NeedsCode(Box::new(CodeStage::Icloud {
                            challenge,
                            apple_id,
                        }))
                    }
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

    #[allow(
        clippy::too_many_lines,
        reason = "one match over the stages a code answers, each a few lines"
    )]
    fn submit_code(&mut self, stage: CodeStage) {
        let code: String = self.code.with(|code| code.trim().to_string());
        if code.is_empty() {
            self.message = Some("enter the code you were shown".into());
            self.stage = Stage::Code(Box::new(stage));
            return;
        }
        self.code.set(String::new());
        let (sender, result) = channel::sync_channel(1);
        let kind = self.kind;
        let existing_id = self.existing_id.clone();
        let display_name = self.display_name();
        let mode = self.selected_mode();
        let existing_config = self.existing_config.clone();
        let store = self.credential_store.clone();
        let (stop, cancel) = std::sync::mpsc::channel();
        let mut waiting_cancel = None;
        match stage {
            CodeStage::Icloud {
                challenge,
                apple_id,
            } => {
                let auth = Arc::clone(self.credential_store.icloud());
                let _ = crate::device_read::thread(
                    "settings-cloud-sign-in",
                    Box::new(move || {
                        let outcome = match auth.verify_code(&challenge, &code) {
                            Ok(session) => {
                                let mut config = icloud_config(auth.as_ref(), &apple_id, mode);
                                if config.get("identity").is_none() {
                                    set(&mut config, "identity", apple_id.clone().into());
                                }
                                Outcome::Completed(Box::new(Completed {
                                    kind,
                                    existing_id,
                                    display_name,
                                    config,
                                    secret: account_credentials::icloud_secret(&apple_id, &session),
                                }))
                            }
                            Err(ICloudAuthError::InvalidCode) => Outcome::CodeRejected(
                                Box::new(CodeStage::Icloud {
                                    challenge,
                                    apple_id,
                                }),
                                ICloudAuthError::InvalidCode.to_string(),
                            ),
                            Err(error) => Outcome::Failed(error.to_string()),
                        };
                        let _ = sender.send(outcome);
                    }),
                );
                self.message = Some("Checking the code…".into());
            }
            CodeStage::Pasted {
                cloud,
                authorization,
            } => {
                let _ = crate::device_read::thread(
                    "settings-cloud-sign-in",
                    Box::new(move || {
                        let outcome =
                            CredentialStore::finish_sign_in_with_code(*authorization, &code)
                                .and_then(|tokens| {
                                    oauth_completed(
                                        &store,
                                        cloud,
                                        kind,
                                        existing_id,
                                        display_name,
                                        &tokens,
                                        mode,
                                        existing_config,
                                    )
                                })
                                .map_or_else(
                                    |error| Outcome::Failed(error.to_string()),
                                    |completed| Outcome::Completed(Box::new(completed)),
                                );
                        let _ = sender.send(outcome);
                    }),
                );
                self.message = Some("Checking the code…".into());
            }
            CodeStage::Session { session, .. } => {
                let _ = crate::device_read::thread(
                    "settings-cloud-sign-in",
                    Box::new(move || {
                        let Followed { progress, session } = follow_session(
                            session,
                            |session| session.cloud_code(&code),
                            |_, _| {},
                            &cancel,
                        );
                        let outcome = match progress {
                            Ok(CloudProgress::SignedIn(signed_in)) => {
                                Outcome::Completed(Box::new(cloud_completed(
                                    signed_in,
                                    kind,
                                    existing_id,
                                    display_name,
                                    mode,
                                    existing_config,
                                )))
                            }
                            Ok(CloudProgress::Code { message }) => match session {
                                Some(session) => Outcome::NeedsCode(Box::new(CodeStage::Session {
                                    session,
                                    message,
                                })),
                                None => {
                                    Outcome::Failed("the session did not keep the sign-in".into())
                                }
                            },
                            Ok(CloudProgress::Page { .. } | CloudProgress::Waiting) => {
                                Outcome::Failed("the session did not finish the sign-in".into())
                            }
                            Err(error) => Outcome::Failed(error),
                        };
                        let _ = sender.send(outcome);
                    }),
                );
                self.message = Some("Checking the code…".into());
                waiting_cancel = Some(Cancel::Channel(stop));
            }
        }
        self.stage = Stage::Waiting {
            urls: None,
            launch: None,
            result,
            cancel: waiting_cancel,
        };
    }

    pub fn cancel(&mut self) {
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

    pub fn poll(&mut self) -> (Option<Completed>, bool) {
        let Stage::Waiting { urls, launch, .. } = &mut self.stage else {
            return (None, false);
        };
        let changed = if let Some(receiver) = urls
            && let Ok((url, shown)) = receiver.try_recv()
        {
            if !shown {
                *launch = Some(self.opener.open(&url));
            }
            *urls = None;
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
            Outcome::Completed(completed) => {
                self.stage = Stage::Form;
                self.message = None;
                (Some(*completed), true)
            }
            Outcome::NeedsCode(stage) => {
                let message = match &*stage {
                    CodeStage::Icloud { .. } => t("accounts-sign-in-icloud-code"),
                    CodeStage::Pasted { .. } => t("accounts-sign-in-code"),
                    CodeStage::Session { message, .. } => message.clone(),
                };
                self.stage = Stage::Code(stage);
                self.message = Some(message);
                (None, true)
            }
            Outcome::CodeRejected(stage, error) => {
                self.stage = Stage::Code(stage);
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

    /// The copy saving `account` starts: a drive connected to be copied in once, or one switched
    /// from Sync to it, is copied into the workspace.
    pub(super) fn copy_once(&self, account: &accounts::Account) -> Option<CopyOnce> {
        let copies = accounts::cloud_types::files_mode(account) == Some(FilesMode::Import)
            && self.original_mode != Some(FilesMode::Import);
        copies
            .then(|| CopyOnce::new(self.kind, account.id.to_string()))
            .flatten()
    }

    pub fn save_failed(&mut self, error: &str) {
        self.message = Some(format!("Could not save the account: {error}"));
    }

    /// Block until the running step reports (opening a page the worker learns on the way),
    /// keeping the outcome for the next [`Self::poll`]. Returns whether it reported before
    /// `timeout`.
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
                Ok((url, false)) => *launch = Some(self.opener.open(&url)),
                Ok((_, true)) | Err(channel::RecvTimeoutError::Disconnected) => {}
                Err(channel::RecvTimeoutError::Timeout) => return false,
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

    pub fn section(
        &self,
        bind: &dyn Fn(Button, super::AccountsEffect) -> Button,
    ) -> SettingsSection {
        let service = kind_shown(self.kind);
        let title = t_args(
            if self.existing_id.is_some() {
                "accounts-sign-in-again-to"
            } else {
                "accounts-sign-in-connect"
            },
            &[("service", service.into())],
        );
        let mut fields: Vec<Item> = Vec::new();
        let unavailable = self.unavailable_reason();
        let waiting = self.busy();
        if let Some(reason) = &unavailable {
            fields.push(note(reason));
        }
        if let Some(message) = &self.message {
            fields.push(note(message));
        }
        if !waiting {
            match &self.stage {
                Stage::Code(_) => fields.push(field_row(
                    &t("accounts-sign-in-code"),
                    Box::new(
                        TextInput::from_state(&self.code)
                            .with_kind(TextInputKind::Code)
                            .with_id("accounts.signin.code")
                            .with_accessible_name(t("settings-a11y-provider-code"))
                            .with_placeholder(t("settings-paste-code")),
                    ),
                )),
                _ => self.form_fields(&mut fields),
            }
        }
        let mut buttons = Row::new();
        if unavailable.is_none() && !waiting {
            let label = match &self.stage {
                Stage::Code(_) => t("settings-accounts-verify"),
                _ if self.kind.is_cloud_storage() => t("accounts-services-continue"),
                _ => t("accounts-services-continue"),
            };
            buttons = buttons.push(Box::new(bind(
                Button::new("settings.accounts.form.save", &label)
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
        fields.push(field_row(
            &t("accounts-display-name"),
            Box::new(
                TextInput::from_state(&self.name)
                    .with_kind(TextInputKind::Name)
                    .with_id("accounts.signin.name")
                    .with_accessible_name(t("settings-a11y-display-name"))
                    .with_placeholder(kind_shown(self.kind)),
            ),
        ));
        match self.kind {
            AccountKind::Nextcloud => {
                fields.push(field_row(
                    &t("accounts-sign-in-server"),
                    Box::new(
                        TextInput::from_state(&self.server)
                            .with_kind(TextInputKind::Url)
                            .with_id("accounts.signin.server")
                            .with_accessible_name(t("settings-a11y-server-url"))
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
                    &t("accounts-sign-in-apple-id"),
                    Box::new(
                        TextInput::from_state(&self.apple_id)
                            .with_kind(TextInputKind::Name)
                            .with_id("accounts.signin.apple-id")
                            .with_accessible_name(t("accounts-sign-in-apple-id"))
                            .with_placeholder("name@icloud.com"),
                    ),
                ));
                fields.push(field_row(
                    &t("accounts-sign-in-password"),
                    Box::new(
                        TextInput::from_state(&self.password)
                            .with_kind(TextInputKind::Password)
                            .with_id("accounts.signin.password")
                            .with_accessible_name(t("accounts-sign-in-password")),
                    ),
                ));
                fields.push(note(&t("accounts-best-effort-icloud")));
                fields.push(note(&t("accounts-sign-in-icloud-note")));
            }
            AccountKind::GoogleDrive | AccountKind::OneDrive | AccountKind::Dropbox => {
                fields.push(note(&t("accounts-sign-in-browser-note")));
            }
            AccountKind::Mail | AccountKind::CalDav | AccountKind::WebDav => {}
        }
        if self.kind.is_cloud_storage() {
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
    }
}

/// A sign-in followed on the session's connection, and the connection it stands on (which the
/// code step finishes it through).
struct Followed {
    progress: Result<CloudProgress, String>,
    session: Option<Arc<Connection>>,
}

/// Follow a cloud sign-in the session started until it finishes, needs a code, or is cancelled.
fn follow_session(
    session: Arc<Connection>,
    first: impl FnOnce(&Connection) -> Result<CloudProgress, String>,
    page: impl Fn(String, bool),
    cancel: &std::sync::mpsc::Receiver<()>,
) -> Followed {
    use std::sync::mpsc::RecvTimeoutError;
    let started = ui::time::now();
    let mut progress = first(&session);
    loop {
        match progress {
            Ok(CloudProgress::Page { url, shown }) => page(url, shown),
            Ok(CloudProgress::Waiting) => {}
            Ok(CloudProgress::Code { message }) => {
                return Followed {
                    progress: Ok(CloudProgress::Code { message }),
                    session: Some(session),
                };
            }
            Ok(signed_in @ CloudProgress::SignedIn(_)) => {
                return Followed {
                    progress: Ok(signed_in),
                    session: None,
                };
            }
            Err(error) => {
                return Followed {
                    progress: Err(error),
                    session: None,
                };
            }
        }
        let remaining = SIGN_IN_LIFETIME.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            session.cloud_end();
            return Followed {
                progress: Err("sign-in was not completed in time".into()),
                session: None,
            };
        }
        match cancel.recv_timeout(POLL_INTERVAL.min(remaining)) {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                session.cloud_end();
                return Followed {
                    progress: Err("sign-in was cancelled".into()),
                    session: None,
                };
            }
        }
        progress = session.cloud_poll();
    }
}

/// The account a finished `OAuth` sign-in makes in Settings: the provider's storage resolved
/// once, and the Files mode the person chose.
fn oauth_completed(
    credential_store: &CredentialStore,
    cloud: OAuthCloud,
    kind: AccountKind,
    existing_id: Option<String>,
    display_name: String,
    tokens: &account_credentials::TokenSet,
    mode: FilesMode,
    mut config: serde_json::Value,
) -> Result<Completed, CredentialError> {
    let mount = credential_store.resolve_storage(cloud, tokens)?;
    if !config.is_object() {
        config = serde_json::json!({});
    }
    set(&mut config, "provider", cloud.type_id().into());
    set(&mut config, accounts::cloud_types::MOUNT_CONFIG_KEY, mount);
    accounts::cloud_types::set_files_mode(&mut config, mode);
    Ok(Completed {
        kind,
        existing_id,
        display_name,
        config,
        secret: account_credentials::oauth_secret(tokens),
    })
}

/// The account a cloud sign-in the session finished makes here: its configuration as the session
/// resolved it, with the person's name and Files mode.
fn cloud_completed(
    signed_in: crate::account_probe::connect::CloudSignedIn,
    kind: AccountKind,
    existing_id: Option<String>,
    display_name: String,
    mode: FilesMode,
    mut config: serde_json::Value,
) -> Completed {
    if !config.is_object() {
        config = signed_in.account.config.clone();
    }
    let mount = signed_in
        .account
        .config
        .get(accounts::cloud_types::MOUNT_CONFIG_KEY)
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    set(&mut config, accounts::cloud_types::MOUNT_CONFIG_KEY, mount);
    if let Some(provider) = signed_in.account.config.get("provider") {
        let provider = provider.clone();
        set(&mut config, "provider", provider);
    }
    accounts::cloud_types::set_files_mode(&mut config, mode);
    Completed {
        kind,
        existing_id,
        display_name,
        config,
        secret: signed_in.secret,
    }
}

fn icloud_config(
    auth: &dyn account_credentials::icloud::ICloudAuth,
    apple_id: &str,
    mode: FilesMode,
) -> serde_json::Value {
    let mut config = serde_json::Map::new();
    config.insert("identity".into(), apple_id.into());
    let mount = auth.storage_config();
    if !mount.is_null() {
        config.insert(accounts::cloud_types::MOUNT_CONFIG_KEY.into(), mount);
    }
    let mut config = serde_json::Value::Object(config);
    accounts::cloud_types::set_files_mode(&mut config, mode);
    config
}

/// Why `kind` cannot sign in in this build, when it cannot: its provider's client registration is
/// missing, or `iCloud`'s sign-in is.
pub(super) fn unavailable_reason(
    kind: AccountKind,
    credential_store: &CredentialStore,
) -> Option<String> {
    if crate::account_probe::session().is_some() {
        return None;
    }
    if let Some(oauth_use) = kind.oauth_cloud().map(OAuthCloud::oauth_use) {
        return match credential_store.use_config(oauth_use) {
            Err(CredentialError::NotConfigured { provider, key }) => Some(format!(
                "{provider} sign-in is not configured in this build: it needs an OAuth client \
                 ID registered with {}, provided as {key} when Tilo is built or started.",
                oauth_use.issuer.label()
            )),
            _ => None,
        };
    }
    (kind == AccountKind::ICloudDrive && !credential_store.icloud().is_available())
        .then(|| "iCloud Drive sign-in is not available in this build yet.".to_string())
}

pub(super) fn kind_shown(kind: AccountKind) -> String {
    t(match kind {
        AccountKind::Mail => "accounts-kind-mail",
        AccountKind::CalDav => "accounts-kind-caldav",
        AccountKind::WebDav => "accounts-kind-webdav",
        AccountKind::Nextcloud => "accounts-kind-nextcloud",
        AccountKind::GoogleDrive => "accounts-kind-google",
        AccountKind::OneDrive => "accounts-kind-onedrive",
        AccountKind::Dropbox => "accounts-kind-dropbox",
        AccountKind::ICloudDrive => "accounts-kind-icloud",
    })
}
