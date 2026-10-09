//! External Accounts pane — one inventory over the canonical account stores
//! that installed clients consume.
//!
//! ## What's wired
//!
//! - **Add** and **Edit** use one device-local form for email, `CalDAV`, `WebDAV`,
//!   and Nextcloud metadata; email setup auto-configures IMAP/SMTP.
//! - **Sign in again** replaces only the sealed vault credential.
//! - Google Drive, `OneDrive` and Dropbox connect through `OAuth` in the
//!   system browser, Nextcloud through Login Flow v2 (an app password minted
//!   for this device, never a typed password), and `iCloud` Drive with an
//!   Apple ID and verification code; see `cloud_signin`. A provider without
//!   a configured client id shows why it cannot be connected instead of
//!   starting a flow that cannot finish.
//! - **Remove** is explicitly confirmed, then transactionally clears the
//!   canonical metadata, sealed credential, and local sync-health record.
//! - **Sync now**, **Pause sync** and **Resume sync** reach a file account's
//!   mount through the cloud account service (`cloud_control`), offered
//!   exactly while the service lists the account as mounted.
//!
//! ## Live vs. sample
//!
//! Production loads the configured stores and renders an empty state when none
//! are available.
//! Screenshot and widget tests may inject [`crate::accounts::sample_directory`]
//! explicitly; those rows are labelled examples and omit mutation controls.
//!
//! Connection checks run asynchronously through bounded, protocol-level
//! probes. No form draft or credential is written to collaborative state.

use std::rc::Rc;

use tracing::warn;
use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow};
use ui::tilo_resource::{self, Effect};
use ui::widget::{Button, Column, Row, Spacer, Text, TextInput, TextInputKind, Widget};

use crate::accounts::{AccountConfig, AccountDirectory, AccountKind, AccountRoot, AccountView};
use crate::effects::SettingsEffect;
use crate::host::PaneCx;
use crate::i18n::{t, t_args};
use crate::panes::model::{PaneModel, Shared};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    El, StatusTone, caption, card, card_with_action, rows, semantic_status_pill, setting_row,
    value_row,
};

mod cloud_control;
mod cloud_signin;
pub(crate) use cloud_control::CloudMounts;
pub use cloud_signin::{BrowserOpener, PortalOpener};
use runtime_fs::cloud::CloudRequest;
#[cfg(test)]
mod signin_tests;

/// Adding, editing, checking and removing the person's external accounts.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum AccountsEffect {
    /// Show or hide public metadata in the browser identity picker.
    #[effect(row, device_only, idempotent)]
    SetBrowserIdentityEnabled { account: String, enabled: bool },
    /// Remove browser picker metadata; this does not revoke sign-in or access.
    #[effect(row, device_only, destructive)]
    RemoveBrowserIdentity { account: String },
    /// Publish the signed-in Tilo identity's public browser connection metadata.
    #[effect(device_only, idempotent)]
    PublishBrowserIdentity {
        #[param("Canonical HTTPS origin of the existing Tilo host")]
        origin: String,
    },
    /// Add an account: mail, caldav, webdav, nextcloud, google-drive,
    /// onedrive, dropbox or icloud-drive.
    #[effect(view)]
    AddAccount {
        #[param("The kind of account, by its slug")]
        kind: String,
    },
    /// Edit an account's details.
    #[effect(view, row)]
    EditAccount {
        #[param("The account's id")]
        account: String,
    },
    /// Sign an account in again.
    #[effect(view, row, label = "Sign in again")]
    SignInAgain {
        #[param("The account's id")]
        account: String,
    },
    /// Check and save the account form, or take the sign-in's next step.
    #[effect(label = "Save account")]
    SaveAccountForm,
    /// Close the account form or sign-in without saving.
    #[effect(view)]
    CancelAccountForm,
    /// Check and save the password typed to sign an account in again.
    #[effect(label = "Save password")]
    SaveNewPassword,
    /// Close the sign-in-again form without saving.
    #[effect(view)]
    CancelNewPassword,
    /// Check that an account's server accepts its saved sign-in.
    #[effect(job, returns, row, idempotent, label = "Check connection")]
    TestAccount {
        #[param("The account's id")]
        account: String,
    },
    /// Remove an account, its saved credential and its sync record.
    #[effect(row, destructive, label = "Remove account")]
    RemoveAccount {
        #[param("The account's id")]
        account: String,
    },
    /// Sync a mounted file account now; one that could not start is tried
    /// again.
    #[effect(row, job, device_only, idempotent, label = "Sync now")]
    SyncNow {
        #[param("The account's id")]
        account: String,
    },
    /// Pause a mounted file account's background sync and uploads.
    #[effect(row, job, device_only, idempotent, label = "Pause sync")]
    PauseSync {
        #[param("The account's id")]
        account: String,
    },
    /// Resume a paused file account's sync.
    #[effect(row, job, device_only, idempotent, label = "Resume sync")]
    ResumeSync {
        #[param("The account's id")]
        account: String,
    },
}

/// Answer `effect` on the area's model; a connection check runs on a
/// worker.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: AccountsEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    match effect {
        AccountsEffect::PublishBrowserIdentity { origin } => {
            app.publish_browser_identity(origin)?;
            return Ok(Effected::done());
        }
        AccountsEffect::SetBrowserIdentityEnabled { account, enabled } => {
            app.set_browser_identity_enabled(account, enabled)?;
            return Ok(Effected::done());
        }
        AccountsEffect::RemoveBrowserIdentity { account } => {
            app.remove_browser_identity(account)?;
            return Ok(Effected::done());
        }
        _ => {}
    }
    let shared = app.accounts().model();
    let mut model = shared.borrow_mut();
    match effect {
        AccountsEffect::PublishBrowserIdentity { .. }
        | AccountsEffect::SetBrowserIdentityEnabled { .. }
        | AccountsEffect::RemoveBrowserIdentity { .. } => {
            return Err("Identity publication was not handled".into());
        }
        AccountsEffect::AddAccount { kind } => {
            let kind =
                AccountKind::from_slug(&kind).ok_or_else(|| "unknown account kind".to_string())?;
            model.command(AccountViewCommand::Add(kind));
        }
        AccountsEffect::EditAccount { account } => model.command(AccountViewCommand::Edit(account)),
        AccountsEffect::SignInAgain { account } => {
            model.command(AccountViewCommand::Reauthenticate(account));
        }
        AccountsEffect::SaveAccountForm => model.command(AccountViewCommand::Submit),
        AccountsEffect::CancelAccountForm => model.command(AccountViewCommand::Cancel),
        AccountsEffect::SaveNewPassword => {
            model.command(AccountViewCommand::SubmitReauthentication);
        }
        AccountsEffect::CancelNewPassword => {
            model.command(AccountViewCommand::CancelReauthentication);
        }
        AccountsEffect::RemoveAccount { account } => model.remove(&account)?,
        AccountsEffect::SyncNow { account } => {
            return cloud(model, CloudRequest::SyncNow { account }, cx);
        }
        AccountsEffect::PauseSync { account } => {
            return cloud(model, CloudRequest::Pause { account }, cx);
        }
        AccountsEffect::ResumeSync { account } => {
            return cloud(model, CloudRequest::Resume { account }, cx);
        }
        AccountsEffect::TestAccount { account } => {
            let check = model.connection_check(&account)?;
            drop(model);
            return Ok(cx.thread(
                move |_| check(),
                |_: &mut SettingsApp, outcome: Result<String, String>, _| {
                    complete_account_check(outcome)
                },
            ));
        }
    }
    Ok(Effected::done())
}

fn complete_account_check(outcome: Result<String, String>) -> EffectAnswer {
    outcome.map(Effected::value)
}

/// Send `request` to the cloud account service on a worker; the pane asks
/// the service for its mounts again once it answered.
fn cloud(
    model: std::cell::RefMut<'_, AccountsPane>,
    request: CloudRequest,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let send = model.cloud.request(request)?;
    drop(model);
    Ok(cx.thread(
        move |_| send(),
        |app: &mut SettingsApp, outcome: Result<(), String>, _| {
            app.accounts().model().borrow_mut().cloud.invalidate();
            outcome.map(|()| Effected::done())
        },
    ))
}

/// What the External Accounts area keeps for the window: the model, read
/// when first needed.
pub struct AccountsArea {
    cx: PaneCx,
    model: std::cell::OnceCell<Shared<AccountsPane>>,
    account_root: Option<AccountRoot>,
}

impl AccountsArea {
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            cx: cx.clone(),
            model: std::cell::OnceCell::new(),
            account_root: None,
        }
    }

    pub(crate) fn with_account_root(mut self, root: AccountRoot) -> Self {
        self.account_root = Some(root);
        self
    }

    /// The model over the person's accounts; a screenshot shows the sample
    /// accounts and the form its scene asks for.
    pub fn model(&self) -> &Shared<AccountsPane> {
        self.model.get_or_init(|| {
            let scene = self.cx.scene();
            let root = scene
                .and_then(|scene| scene.account_root.clone())
                .map_or_else(
                    || self.account_root.clone().unwrap_or_else(AccountRoot::live),
                    AccountRoot::at,
                );
            let mut model = if scene.is_some() {
                AccountsPane::from_dir_in(root, crate::accounts::sample_directory(), false)
            } else {
                AccountsPane::in_root(root)
            };
            if let Some(kind) = scene.and_then(|scene| scene.account_form) {
                model = model.with_open_form_for_shot(kind);
            }
            Rc::new(std::cell::RefCell::new(model.with_host(self.cx.clone())))
        })
    }

    /// Take in finished checks and sign-in steps, if the model exists.
    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get() {
            let _ = model.borrow_mut().poll();
        }
    }

    /// One row per account, offering what can be done to it.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let model = self.model().borrow();
        if model.is_sample {
            return Vec::new();
        }
        let offer = |effect: AccountsEffect| SettingsEffect::from(effect).bound();
        let mut published: Vec<LiveRow> = AccountKind::ALL
            .into_iter()
            .flat_map(|kind| model.dir.views_of(kind))
            .map(|view| {
                let id = view.id.clone();
                let mut row = LiveRow::new(
                    format!("settings.account:{id}"),
                    "settings.account",
                    view.label.clone(),
                );
                if !view.kind.is_cloud_storage() {
                    row = row.operation(offer(AccountsEffect::EditAccount {
                        account: id.clone(),
                    }));
                }
                if let Some(mount) = model.cloud.mount(&id) {
                    row = row.operation(offer(AccountsEffect::SyncNow {
                        account: id.clone(),
                    }));
                    row = row.operation(offer(if mount.paused {
                        AccountsEffect::ResumeSync {
                            account: id.clone(),
                        }
                    } else {
                        AccountsEffect::PauseSync {
                            account: id.clone(),
                        }
                    }));
                }
                row.operation(offer(AccountsEffect::SignInAgain {
                    account: id.clone(),
                }))
                .operation(offer(AccountsEffect::TestAccount {
                    account: id.clone(),
                }))
                .operation(offer(AccountsEffect::RemoveAccount { account: id }))
            })
            .collect();
        for account in model
            .account_root
            .store()
            .list(&accounts::person::TILO_IDENTITY_TYPE.into())
        {
            let id = account.id.as_str().to_owned();
            published.push(
                LiveRow::new(
                    format!("settings.browser-identity:{id}"),
                    "settings.browser-identity",
                    account.display_name.clone(),
                )
                .operation(offer(AccountsEffect::SetBrowserIdentityEnabled {
                    account: id.clone(),
                    enabled: !account.enabled,
                }))
                .operation(offer(AccountsEffect::RemoveBrowserIdentity { account: id })),
            );
        }
        published
    }
}

fn kind_shown(kind: AccountKind) -> String {
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

fn account_status(
    is_sample: bool,
    enabled: bool,
    health: Option<accounts::AccountSyncHealth>,
) -> (String, StatusTone) {
    if is_sample {
        (t("accounts-status-example"), StatusTone::Neutral)
    } else if !enabled {
        (t("accounts-status-disabled"), StatusTone::Neutral)
    } else {
        match health {
            Some(accounts::AccountSyncHealth::Syncing) => {
                (t("accounts-status-syncing"), StatusTone::Info)
            }
            Some(accounts::AccountSyncHealth::Healthy) => {
                (t("accounts-status-healthy"), StatusTone::Success)
            }
            Some(accounts::AccountSyncHealth::Offline) => {
                (t("accounts-status-offline"), StatusTone::Warning)
            }
            Some(accounts::AccountSyncHealth::NeedsAuthentication) => {
                (t("accounts-status-reauth"), StatusTone::Warning)
            }
            Some(accounts::AccountSyncHealth::Error) => {
                (t("accounts-status-error"), StatusTone::Warning)
            }
            Some(accounts::AccountSyncHealth::Never) | None => {
                (t("accounts-status-never"), StatusTone::Neutral)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountViewCommand {
    Add(AccountKind),
    Edit(String),
    Reauthenticate(String),
    Submit,
    Cancel,
    SubmitReauthentication,
    CancelReauthentication,
}

/// The account form's fields, as checked and then committed.
struct AccountDraft {
    kind: AccountKind,
    /// The account being edited; `None` adds a new one.
    existing_id: Option<String>,
    /// The id a new account takes once its draft has been checked.
    assigned_id: Option<accounts::AccountId>,
    name: String,
    identity: String,
    username: String,
    server: String,
    password: String,
}

struct ProbeJob {
    receiver: std::sync::mpsc::Receiver<Result<String, String>>,
    deadline: ui::time::Instant,
}

impl ProbeJob {
    const DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

    fn new(receiver: std::sync::mpsc::Receiver<Result<String, String>>) -> Self {
        Self {
            receiver,
            deadline: ui::time::now() + Self::DEADLINE,
        }
    }

    fn poll(&self, stopped: &str) -> Option<Result<String, String>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(stopped.into())),
            Err(std::sync::mpsc::TryRecvError::Empty) if ui::time::now() >= self.deadline => {
                Some(Err(t("accounts-timeout")))
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
        }
    }
}

impl Default for AccountsPane {
    fn default() -> Self {
        Self::new()
    }
}

/// The person's external accounts as the pane shows them, with the forms
/// and sign-ins in progress.
pub struct AccountsPane {
    /// The window this model serves, for binding its controls; none for a
    /// model a test drives directly.
    cx: Option<PaneCx>,
    account_root: AccountRoot,
    /// The revision of the person's accounts last shown, so an edit from
    /// another window or device is shown too.
    seen_accounts: Option<u64>,
    dir: AccountDirectory,
    sync_statuses: accounts::AccountSyncStatusStore,
    vault: accounts::SecretVault,
    /// Whether `dir` is the illustrative sample (the real stores are empty).
    is_sample: bool,
    form_kind: Option<AccountKind>,
    form_error: Option<String>,
    form_account_id: Option<String>,
    form_password: ui::LocalState<String>,
    form_name: ui::LocalState<String>,
    form_email: ui::LocalState<String>,
    form_user: ui::LocalState<String>,
    form_server: ui::LocalState<String>,
    reauth_password: ui::LocalState<String>,
    reauth_error: Option<String>,
    reauth_id: Option<String>,
    account_tester: std::sync::Arc<dyn crate::account_probe::AccountTester>,
    form_probe: Option<ProbeJob>,
    form_candidate: Option<AccountDraft>,
    reauth_probe: Option<ProbeJob>,
    reauth_candidate: Option<(accounts::AccountId, accounts::Secret)>,
    /// A running browser or Apple ID sign-in.
    sign_in: Option<cloud_signin::SignInFlow>,
    custodian: account_custody::Custodian,
    opener: std::sync::Arc<dyn BrowserOpener>,
    /// The file accounts the cloud service has mounted.
    cloud: CloudMounts,
    changed: bool,
}

impl AccountsPane {
    /// The live pane over the person's account stores.
    #[must_use]
    pub fn new() -> Self {
        Self::in_root(AccountRoot::live())
    }

    /// The live pane over the account stores under `account_root`.
    pub fn in_root(account_root: AccountRoot) -> Self {
        // The live pane shows the *real* stores — empty until the user adds an
        // account. It never substitutes illustrative sample accounts: showing
        // fabricated `ada@example.com` rows would present made-up data on an
        // empty state. The deterministic `--shots`/tests use
        // `from_dir(sample_directory(), true)` for a rich still instead.
        Self {
            dir: AccountDirectory::load_in(&account_root),
            sync_statuses: account_root.statuses(),
            vault: account_root.vault(),
            is_sample: false,
            form_kind: None,
            form_error: None,
            form_account_id: None,
            form_password: ui::LocalState::new(String::new()),
            form_name: ui::LocalState::new(String::new()),
            form_email: ui::LocalState::new(String::new()),
            form_user: ui::LocalState::new(String::new()),
            form_server: ui::LocalState::new(String::new()),
            reauth_password: ui::LocalState::new(String::new()),
            reauth_error: None,
            reauth_id: None,
            account_tester: std::sync::Arc::new(crate::account_probe::LiveAccountTester),
            form_probe: None,
            form_candidate: None,
            reauth_probe: None,
            reauth_candidate: None,
            sign_in: None,
            custodian: account_root.custodian(),
            opener: std::sync::Arc::new(PortalOpener),
            cloud: CloudMounts::live(),
            changed: false,
            cx: None,
            account_root,
            seen_accounts: None,
        }
    }

    /// Test / `--shots` constructor over an explicit directory. `is_sample`
    /// controls whether Remove buttons render (real accounts) or not
    /// (illustration).
    pub fn from_dir(dir: AccountDirectory, is_sample: bool) -> Self {
        Self::from_dir_in(AccountRoot::live(), dir, is_sample)
    }

    /// [`Self::from_dir`] whose edits persist under `account_root`.
    pub fn from_dir_in(account_root: AccountRoot, dir: AccountDirectory, is_sample: bool) -> Self {
        Self {
            dir,
            sync_statuses: accounts::AccountSyncStatusStore::default(),
            vault: account_root.vault(),
            is_sample,
            form_kind: None,
            form_error: None,
            form_account_id: None,
            form_password: ui::LocalState::new(String::new()),
            form_name: ui::LocalState::new(String::new()),
            form_email: ui::LocalState::new(String::new()),
            form_user: ui::LocalState::new(String::new()),
            form_server: ui::LocalState::new(String::new()),
            reauth_password: ui::LocalState::new(String::new()),
            reauth_error: None,
            reauth_id: None,
            account_tester: std::sync::Arc::new(crate::account_probe::LiveAccountTester),
            form_probe: None,
            form_candidate: None,
            reauth_probe: None,
            reauth_candidate: None,
            sign_in: None,
            custodian: account_root.custodian(),
            opener: std::sync::Arc::new(PortalOpener),
            cloud: CloudMounts::at(None),
            changed: false,
            cx: None,
            account_root,
            seen_accounts: None,
        }
    }

    /// Know the cloud service's mounts from `cloud` (tests).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_cloud_mounts(mut self, cloud: CloudMounts) -> Self {
        self.cloud = cloud;
        self
    }

    pub fn is_sample(&self) -> bool {
        self.is_sample
    }

    pub fn form_open(&self) -> bool {
        self.form_kind.is_some()
    }

    pub fn directory(&self) -> &AccountDirectory {
        &self.dir
    }

    /// Harness seam for exercising vault persistence without ever placing a
    /// credential in the Automerge document.
    #[doc(hidden)]
    pub fn set_form_password_for_test(&self, password: &str) {
        self.form_password.set(password.to_string());
    }

    #[doc(hidden)]
    pub fn set_form_fields_for_test(&self, name: &str, email: &str, user: &str, server: &str) {
        self.form_name.set(name.into());
        self.form_email.set(email.into());
        self.form_user.set(user.into());
        self.form_server.set(server.into());
    }

    #[doc(hidden)]
    pub fn submit_form_for_test(&mut self) {
        self.command(AccountViewCommand::Submit);
    }

    /// Drive the same nonblocking sync polling the live host uses until the
    /// pending account probe and its synchronous disk transaction finish.
    /// Bounded by the probe's production deadline; unlike a fixed number of
    /// scheduler yields this remains reliable when the test suite is busy.
    #[doc(hidden)]
    pub fn finish_form_submission_for_test(&mut self) -> bool {
        let deadline = ui::time::now() + ProbeJob::DEADLINE;
        while self.form_open() && ui::time::now() < deadline {
            let _ = self.poll();
            if self.form_open() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        !self.form_open()
    }

    /// The model serving `cx`'s window: its controls invoke the effects.
    #[must_use]
    pub(crate) fn with_host(mut self, cx: PaneCx) -> Self {
        self.cx = Some(cx);
        self
    }

    /// `button`, invoking `effect` in the window this model serves.
    fn bound(&self, button: Button, effect: AccountsEffect) -> Button {
        match &self.cx {
            Some(cx) => cx.bind_invocation(button, effect),
            None => button,
        }
    }

    #[must_use]
    pub fn with_account_tester(
        mut self,
        tester: std::sync::Arc<dyn crate::account_probe::AccountTester>,
    ) -> Self {
        self.account_tester = tester;
        self
    }

    /// Use `custodian` for cloud sign-ins (tests point it at a fake
    /// provider; builds link an iCloud provider through it).
    #[must_use]
    pub fn with_custodian(mut self, custodian: account_custody::Custodian) -> Self {
        self.custodian = custodian;
        self
    }

    /// Open sign-in pages with `opener` instead of the desktop portal.
    #[must_use]
    pub fn with_browser_opener(mut self, opener: std::sync::Arc<dyn BrowserOpener>) -> Self {
        self.opener = opener;
        self
    }

    pub fn sign_in_open(&self) -> bool {
        self.sign_in.is_some()
    }

    #[doc(hidden)]
    pub fn sign_in_message_for_test(&self) -> Option<&str> {
        self.sign_in
            .as_ref()
            .and_then(cloud_signin::SignInFlow::message)
    }

    #[doc(hidden)]
    pub fn sign_in_awaiting_code_for_test(&self) -> bool {
        self.sign_in
            .as_ref()
            .is_some_and(cloud_signin::SignInFlow::awaiting_code)
    }

    #[doc(hidden)]
    pub fn set_sign_in_fields_for_test(
        &self,
        name: &str,
        server: &str,
        apple_id: &str,
        password: &str,
    ) {
        if let Some(flow) = &self.sign_in {
            flow.set_fields_for_test(name, server, apple_id, password);
        }
    }

    #[doc(hidden)]
    pub fn set_sign_in_code_for_test(&self, code: &str) {
        if let Some(flow) = &self.sign_in {
            flow.set_code_for_test(code);
        }
    }

    /// Block until the running sign-in step reports, then apply it exactly
    /// as `sync` would. Returns whether a step reported before `timeout`.
    #[doc(hidden)]
    pub fn finish_sign_in_step_for_test(&mut self, timeout: std::time::Duration) -> bool {
        let Some(flow) = self.sign_in.as_mut() else {
            // The step completed and the sync that started it already saved it.
            return true;
        };
        if !flow.busy() {
            // Already reported and applied by the sync that started it.
            return true;
        }
        let reported = flow.wait_for_test(timeout);
        self.poll_sign_in();
        reported
    }

    fn open_sign_in(&mut self, kind: AccountKind, existing: Option<&accounts::Account>) {
        self.form_kind = None;
        self.reauth_id = None;
        self.sign_in = Some(cloud_signin::SignInFlow::new(
            kind,
            existing,
            self.custodian.clone(),
            std::sync::Arc::clone(&self.opener),
        ));
        self.changed = true;
    }

    fn close_sign_in(&mut self) {
        if let Some(flow) = self.sign_in.as_mut() {
            flow.cancel();
        }
        self.sign_in = None;
        self.changed = true;
    }

    fn poll_sign_in(&mut self) {
        let Some(flow) = self.sign_in.as_mut() else {
            return;
        };
        let (completed, changed) = flow.poll();
        if changed {
            self.changed = true;
        }
        let Some(completed) = completed else {
            return;
        };
        match Self::save_signed_in(&self.account_root, &completed) {
            Ok(id) => {
                let custodian = self.custodian.clone();
                // Publishing may renew a session over the network; the
                // account is already saved, so this never blocks the pane.
                std::thread::spawn(move || {
                    if let Err(error) = custodian.signed_in(&id) {
                        warn!(target: "tilo::settings", %error, "could not publish the first credential for a new sign-in");
                    }
                });
                self.sign_in = None;
                self.reload();
            }
            Err(error) => {
                if let Some(flow) = self.sign_in.as_mut() {
                    flow.save_failed(&error);
                }
            }
        }
        self.changed = true;
    }

    /// Persist a completed sign-in: the sealed grant first, then the
    /// account metadata, journaled like every other account mutation.
    fn save_signed_in(
        account_root: &AccountRoot,
        completed: &cloud_signin::Completed,
    ) -> Result<accounts::AccountId, String> {
        let type_id = completed.kind.type_id();
        let shared = account_root.store();
        let id = completed.existing_id.clone().map_or_else(
            || {
                accounts::AccountId::from(format!(
                    "{}-{}",
                    completed.kind.slug(),
                    uuid::Uuid::new_v4()
                ))
            },
            accounts::AccountId::from,
        );
        let mut stored = match (completed.existing_id.is_some(), shared.get(&id)) {
            (true, Some(stored)) if stored.type_id.as_str() == type_id => stored.clone(),
            (true, Some(_)) => {
                return Err("account type changed; refresh Settings and try again".into());
            }
            (true, None) => return Err("account no longer exists".into()),
            (false, Some(_)) => return Err("this account is already configured".into()),
            (false, None) => accounts::Account::new(id.clone(), type_id),
        };
        stored.display_name.clone_from(&completed.display_name);
        merge_config(&mut stored.config, completed.config.clone());
        if completed.kind == AccountKind::Nextcloud {
            let mut identity = serde_json::Map::new();
            identity.insert("id".into(), id.as_str().into());
            for service in ["use_files", "use_calendar", "use_mail"] {
                if stored.config.get(service).is_none() {
                    identity.insert(service.into(), true.into());
                }
            }
            merge_config(&mut stored.config, serde_json::Value::Object(identity));
        }
        Self::commit_account(account_root, stored, Some(&completed.secret))?;
        Ok(id)
    }

    /// Seed the ordinary add surface for a deterministic acceptance capture.
    #[must_use]
    pub(crate) fn with_open_form_for_shot(mut self, kind: AccountKind) -> Self {
        if kind.signs_in() {
            self.open_sign_in(kind, None);
        } else {
            self.form_kind = Some(kind);
            self.form_account_id = None;
        }
        self
    }

    #[doc(hidden)]
    pub fn set_reauth_password_for_test(&self, password: &str) {
        self.reauth_password.set(password.to_string());
    }

    #[doc(hidden)]
    pub fn reauth_error_for_test(&self) -> Option<&str> {
        self.reauth_error.as_deref()
    }

    fn open_edit(&mut self, id: &str) {
        let Some(view) = AccountKind::ALL
            .into_iter()
            .flat_map(|kind| self.dir.views_of(kind))
            .find(|view| view.id == id)
        else {
            return;
        };
        let kind = view.kind;
        self.clear_form_fields();
        let shared = self.account_root.store();
        if let Some(stored) = shared.get(&accounts::AccountId::from(view.id.clone())) {
            self.form_name.set(stored.display_name.clone());
            if kind == AccountKind::Mail {
                self.form_email.set(
                    stored
                        .config
                        .get("email")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .into(),
                );
                self.form_user.set(
                    stored
                        .config
                        .get("username")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .into(),
                );
            } else {
                self.form_user.set(
                    stored
                        .config
                        .get("username")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .into(),
                );
            }
            let server = stored
                .config
                .get(if kind == AccountKind::WebDav {
                    "base_url"
                } else {
                    "server_url"
                })
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            self.form_server.set(server.into());
        }
        self.form_kind = Some(kind);
        self.form_account_id = Some(view.id);
        self.form_error = None;
        self.form_password.set(String::new());
        self.changed = true;
    }

    /// Take `command`, as the pane's buttons and their effects ask.
    pub fn command(&mut self, command: AccountViewCommand) {
        match Some(command) {
            Some(AccountViewCommand::Add(kind)) if kind.signs_in() => {
                self.open_sign_in(kind, None);
            }
            Some(AccountViewCommand::Add(kind)) => {
                self.sign_in = None;
                self.form_kind = Some(kind);
                self.form_account_id = None;
                self.form_error = None;
                self.form_password.set(String::new());
                self.clear_form_fields();
                self.changed = true;
            }
            Some(AccountViewCommand::Edit(id)) => self.open_edit(&id),
            Some(AccountViewCommand::Reauthenticate(id)) => self.open_reauthentication(id),
            Some(AccountViewCommand::Submit) if self.sign_in.is_some() => {
                if let Some(flow) = self.sign_in.as_mut() {
                    flow.submit();
                }
                self.changed = true;
            }
            Some(AccountViewCommand::Cancel) if self.sign_in.is_some() => self.close_sign_in(),
            Some(AccountViewCommand::Submit) => self.submit_form(),
            Some(AccountViewCommand::Cancel) => self.cancel_form(),
            Some(AccountViewCommand::SubmitReauthentication) => self.submit_reauth(),
            Some(AccountViewCommand::CancelReauthentication) => self.cancel_reauth(),
            None => {}
        }
    }

    /// "Sign in again": a new browser / Apple ID sign-in for accounts that
    /// sign in, a replacement password otherwise.
    fn open_reauthentication(&mut self, id: String) {
        let stored = self
            .account_root
            .store()
            .get(&accounts::AccountId::from(id.clone()))
            .cloned();
        if let Some(account) = stored
            && let Some(kind) = AccountKind::from_type_id(account.type_id.as_str())
            && kind.signs_in()
        {
            self.open_sign_in(kind, Some(&account));
            return;
        }
        self.sign_in = None;
        self.reauth_id = Some(id);
        self.reauth_error = None;
        self.reauth_password.set(String::new());
        self.changed = true;
    }

    fn clear_form_fields(&self) {
        self.form_name.set(String::new());
        self.form_email.set(String::new());
        self.form_user.set(String::new());
        self.form_server.set(String::new());
        self.form_password.set(String::new());
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let mut panel: Vec<El> = Vec::new();

        let mut intro: Vec<El> = Vec::new();
        intro.push(Box::new(
            Text::new(&t("accounts-intro")).with_role(ui::TextRole::Caption),
        ));
        if self.vault.custody() == accounts::SecretCustody::LocalFile {
            intro.push(caption(&t("accounts-vault-local")));
        }
        if self.is_sample {
            // Be honest that the visible rows are an illustration, not real
            // connected accounts — the store is empty until you add one.
            intro.push(caption(&t("accounts-empty-sample")));
        } else if self.dir.is_empty() {
            // The live empty state: honest, no fabricated rows.
            intro.push(caption(&t("accounts-empty")));
        }
        let mut introduction = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(ui::tokens::Spacing::BASE.s2);
        for line in intro {
            introduction = introduction.push(line);
        }
        panel.push(Box::new(introduction));

        if let Some(flow) = &self.sign_in {
            panel.push(flow.card(&|button, effect| self.bound(button, effect)));
        } else if let Some(kind) = self.form_kind {
            panel.push(self.account_form(kind));
        }
        if let Some(id) = &self.reauth_id {
            panel.push(self.reauth_form(id));
        }
        panel.push(self.browser_identity_card());
        panel.push(self.mail_card());
        panel.push(self.kind_card(AccountKind::CalDav));
        panel.push(self.kind_card(AccountKind::WebDav));
        panel.push(self.kind_card(AccountKind::Nextcloud));
        for kind in [
            AccountKind::GoogleDrive,
            AccountKind::OneDrive,
            AccountKind::Dropbox,
            AccountKind::ICloudDrive,
        ] {
            panel.push(self.kind_card(kind));
        }

        crate::widgets::cards(panel)
    }

    fn browser_identity_card(&self) -> El {
        let store = self.account_root.store();
        let mut body = vec![caption(&t("accounts-browser-identity-about"))];
        for account in store.list(&accounts::person::TILO_IDENTITY_TYPE.into()) {
            let id = account.id.as_str().to_owned();
            let toggle = if account.enabled {
                "accounts-browser-identity-hide"
            } else {
                "accounts-browser-identity-show"
            };
            body.push(setting_row(
                &account.display_name,
                Box::new(self.bound(
                    Button::new(
                        &format!("settings.browser-identity.toggle:{id}"),
                        &t(toggle),
                    ),
                    AccountsEffect::SetBrowserIdentityEnabled {
                        account: id.clone(),
                        enabled: !account.enabled,
                    },
                )),
            ));
            if let Some(origin) = account
                .config
                .get("origin")
                .and_then(serde_json::Value::as_str)
            {
                body.push(caption(origin));
            }
            body.push(Box::new(self.bound(
                Button::new(
                    &format!("settings.browser-identity.remove:{id}"),
                    &t("accounts-browser-identity-remove"),
                ),
                AccountsEffect::RemoveBrowserIdentity { account: id },
            )));
        }
        card(&t("accounts-browser-identity-title"), rows(body))
    }

    fn mail_card(&self) -> El {
        let title = kind_shown(AccountKind::Mail);
        let add = t("accounts-add");
        let add_btn: El = Box::new(self.bound(
            Button::new("settings.accounts.add.mail", &add),
            AccountsEffect::AddAccount {
                kind: "mail".into(),
            },
        ));
        let views = self.dir.views_of(AccountKind::Mail);
        let body = self.kind_body(AccountKind::Mail, &views);
        card_with_action(&title, add_btn, body)
    }

    fn kind_card(&self, kind: AccountKind) -> El {
        let views = self.dir.views_of(kind);
        let add = t("accounts-add");
        card_with_action(
            &kind_shown(kind),
            Box::new(self.bound(
                Button::new(&format!("settings.accounts.add.{}", kind.slug()), &add),
                AccountsEffect::AddAccount {
                    kind: kind.slug().into(),
                },
            )),
            self.kind_body(kind, &views),
        )
    }

    fn kind_body(&self, kind: AccountKind, views: &[AccountView]) -> El {
        let mut body: Vec<El> = if views.is_empty() {
            vec![caption(&t_args(
                "accounts-kind-empty",
                &[("kind", kind_shown(kind).into())],
            ))]
        } else {
            views.iter().map(|v| self.account_row(v)).collect()
        };
        if kind.is_cloud_storage()
            && let Some(reason) = cloud_signin::unavailable_reason(kind, &self.custodian)
        {
            body.push(caption(&reason));
        } else if kind.best_effort() {
            body.push(caption(&t("accounts-best-effort-icloud")));
        }
        rows(body)
    }

    /// One account: label + connection summary, status, and a Remove button
    /// (only for real accounts — sample rows are illustrative).
    ///
    /// Sample rows report an "Example" status; they do not represent a live
    /// connection.
    #[allow(
        clippy::too_many_lines,
        reason = "the full account-row action hierarchy is kept together for auditability"
    )]
    fn account_row(&self, view: &AccountView) -> El {
        let id = accounts::AccountId::from(view.id.clone());
        let runtime = self.sync_statuses.get(&id);
        let mount = self.cloud.mount(&view.id);
        let (status, status_tone) = if mount.is_some_and(|mount| mount.paused) {
            (t("accounts-status-paused"), StatusTone::Neutral)
        } else {
            account_status(
                self.is_sample,
                view.enabled,
                runtime.map(|status| status.health),
            )
        };

        // Keep identity and health together; maintenance actions get their own
        // row so narrow layouts remain legible and touch targets never crush
        // the connection summary.
        let mut top = Row::new()
            .push(Box::new(
                Text::new(&view.label).with_role(ui::TextRole::Body),
            ))
            .push(Box::new(Spacer::new()));
        if view.kind.best_effort() {
            top = top.push(semantic_status_pill(
                &t("accounts-best-effort"),
                StatusTone::Neutral,
            ));
        }
        let top = top.push(semantic_status_pill(&status, status_tone));

        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(top))
            .push(Box::new(
                Text::new(&view.summary)
                    .with_role(ui::TextRole::Caption)
                    .dimmed(),
            ));
        if !self.is_sample {
            let sync_line = runtime.map_or_else(
                || t("accounts-sync-none"),
                |status| {
                    let when = status
                        .last_success_unix
                        .map_or_else(|| t("accounts-sync-never"), relative_sync_time);
                    match status.message.as_deref() {
                        Some(message) => t_args(
                            "accounts-sync-when-msg",
                            &[("when", when.as_str().into()), ("message", message.into())],
                        ),
                        None => t_args("accounts-sync-when", &[("when", when.into())]),
                    }
                },
            );
            column = column.push(Box::new(
                Text::new(&sync_line)
                    .with_role(ui::TextRole::Caption)
                    .dimmed(),
            ));
            let needs_sign_in = runtime.is_some_and(|status| {
                status.health == accounts::AccountSyncHealth::NeedsAuthentication
            });
            let reauth_label = if view.kind == AccountKind::ICloudDrive && needs_sign_in {
                t("accounts-reauth-icloud")
            } else {
                t("accounts-reauth")
            };
            let edit = t("accounts-edit");
            let check = if view.kind == AccountKind::Mail {
                t("accounts-check-server")
            } else {
                t("accounts-verify")
            };
            let remove = t("accounts-remove");
            let mut primary_actions = Row::new();
            if !view.kind.is_cloud_storage() {
                primary_actions = primary_actions.push(Box::new(self.bound(
                    Button::new(&format!("settings.accounts.edit.{}", view.id), &edit),
                    AccountsEffect::EditAccount {
                        account: view.id.clone(),
                    },
                )));
            }
            let primary_actions = primary_actions
                .push(Box::new(self.bound(
                    Button::new(
                        &format!("settings.accounts.reauth.{}", view.id),
                        &reauth_label,
                    ),
                    AccountsEffect::SignInAgain {
                        account: view.id.clone(),
                    },
                )))
                .push(Box::new(Spacer::new()));
            let secondary_actions = Row::new()
                .push(Box::new(self.bound(
                    Button::new(&format!("settings.accounts.test.{}", view.id), &check),
                    AccountsEffect::TestAccount {
                        account: view.id.clone(),
                    },
                )))
                .push(Box::new(Spacer::new()))
                .push(Box::new(
                    self.bound(
                        Button::new(&format!("settings.accounts.remove.{}", view.id), &remove)
                            .role(ui::widget::ButtonRole::Destructive),
                        AccountsEffect::RemoveAccount {
                            account: view.id.clone(),
                        },
                    ),
                ));
            column = column
                .push(Box::new(primary_actions))
                .push(Box::new(secondary_actions));
            if let Some(mount) = mount {
                column = column.push(Box::new(self.sync_actions(&view.id, mount.paused)));
            }
        }
        Box::new(column)
    }

    /// Sync now, and pause or resume, for a mounted file account.
    fn sync_actions(&self, id: &str, paused: bool) -> Row {
        let (toggle_id, toggle_label, toggle) = if paused {
            (
                format!("settings.accounts.resume-sync.{id}"),
                t("accounts-resume-sync"),
                AccountsEffect::ResumeSync {
                    account: id.to_owned(),
                },
            )
        } else {
            (
                format!("settings.accounts.pause-sync.{id}"),
                t("accounts-pause-sync"),
                AccountsEffect::PauseSync {
                    account: id.to_owned(),
                },
            )
        };
        Row::new()
            .push(Box::new(self.bound(
                Button::new(
                    &format!("settings.accounts.sync-now.{id}"),
                    &t("accounts-sync-now"),
                ),
                AccountsEffect::SyncNow {
                    account: id.to_owned(),
                },
            )))
            .push(Box::new(
                self.bound(Button::new(&toggle_id, &toggle_label), toggle),
            ))
            .push(Box::new(Spacer::new()))
    }

    /// The inline add-email form: fields bound to CRDT keys plus Save/Cancel.
    #[allow(
        clippy::too_many_lines,
        reason = "the form declares every credential field and accessibility label in one reviewable surface"
    )]
    fn account_form(&self, kind: AccountKind) -> El {
        let display_name = t("accounts-display-name");
        let identity_name = if kind == AccountKind::Mail {
            t("accounts-email")
        } else {
            t("accounts-username")
        };
        let identity_placeholder = if kind == AccountKind::Mail {
            "you@example.com".to_string()
        } else {
            t("accounts-username-or-email")
        };
        let login_name = t("accounts-login-name");
        let login_placeholder = t("accounts-login-placeholder");
        let password_label = t("accounts-password");
        let password_short = t("accounts-password-short");
        let server_label = t("accounts-server");
        let server_url = t("accounts-server-url");
        let server_placeholder = match kind {
            AccountKind::CalDav => "https://calendar.example.com/.well-known/caldav".to_string(),
            AccountKind::WebDav => {
                "https://files.example.com/remote.php/dav/files/you/".to_string()
            }
            AccountKind::Nextcloud => "https://cloud.example.com".to_string(),
            AccountKind::Mail => t("accounts-server-mail"),
            AccountKind::GoogleDrive
            | AccountKind::OneDrive
            | AccountKind::Dropbox
            | AccountKind::ICloudDrive => String::new(),
        };
        let save = t("accounts-save");
        let cancel = t("accounts-cancel");
        let name = TextInput::from_state(&self.form_name)
            .with_kind(TextInputKind::Name)
            .with_id("accounts.form.name")
            .with_accessible_name(&display_name)
            .with_placeholder(&display_name);
        let identity = TextInput::from_state(if kind == AccountKind::Mail {
            &self.form_email
        } else {
            &self.form_user
        })
        .with_kind(if kind == AccountKind::Mail {
            TextInputKind::Email
        } else {
            TextInputKind::Username
        })
        .with_id("accounts.form.identity")
        .with_accessible_name(&identity_name)
        .with_placeholder(&identity_placeholder);
        let user = TextInput::from_state(&self.form_user)
            .with_kind(TextInputKind::Username)
            .with_id("accounts.form.username")
            .with_accessible_name(&login_name)
            .with_placeholder(&login_placeholder);
        let password = TextInput::from_state(&self.form_password)
            .with_kind(TextInputKind::Password)
            .with_id("accounts.form.password")
            .with_accessible_name(&password_label)
            .with_secret(true)
            .with_placeholder(&password_short);
        let server = TextInput::from_state(&self.form_server)
            .with_kind(TextInputKind::Url)
            .with_id("accounts.form.server")
            .with_accessible_name(&server_url)
            .with_placeholder(&server_placeholder);
        let buttons = Row::new()
            .push(Box::new(self.bound(
                Button::new("settings.accounts.form.save", &save),
                AccountsEffect::SaveAccountForm,
            )))
            .push(Box::new(self.bound(
                Button::new("settings.accounts.form.cancel", &cancel),
                AccountsEffect::CancelAccountForm,
            )))
            .push(Box::new(Spacer::new()));
        let title = t_args(
            if self.form_account_id.is_some() {
                "accounts-form-edit"
            } else {
                "accounts-form-add"
            },
            &[("kind", kind_shown(kind).into())],
        );
        card(
            &title,
            rows({
                let mut fields = vec![
                    setting_row(&display_name, Box::new(name)),
                    setting_row(&identity_name, Box::new(identity)),
                ];
                if kind == AccountKind::Mail {
                    fields.push(setting_row(&login_name, Box::new(user)));
                } else {
                    fields.push(setting_row(&server_label, Box::new(server)));
                }
                if kind != AccountKind::Nextcloud {
                    fields.push(setting_row(&password_label, Box::new(password)));
                }
                if let Some(error) = &self.form_error {
                    fields.push(caption(&t_args(
                        "accounts-save-error",
                        &[("error", error.as_str().into())],
                    )));
                }
                fields.push(caption(&t("accounts-form-caption")));
                fields.push(Box::new(buttons));
                fields
            }),
        )
    }

    fn reauth_form(&self, id: &str) -> El {
        let password_label = t("accounts-reauth-password");
        let save = t("accounts-reauth-save");
        let cancel = t("accounts-cancel");
        let password = TextInput::from_state(&self.reauth_password)
            .with_kind(TextInputKind::Password)
            .with_id("accounts.reauth.password")
            .with_accessible_name(&password_label)
            .with_secret(true)
            .with_placeholder(&password_label);
        let mut fields = vec![
            value_row(&t("accounts-reauth-account"), id),
            setting_row(&t("accounts-reauth-credential"), Box::new(password)),
            caption(&t("accounts-reauth-caption")),
        ];
        if let Some(error) = &self.reauth_error {
            fields.push(caption(&t_args(
                "accounts-reauth-error",
                &[("error", error.as_str().into())],
            )));
        }
        fields.push(Box::new(
            Row::new()
                .push(Box::new(self.bound(
                    Button::new("settings.accounts.reauth.save", &save),
                    AccountsEffect::SaveNewPassword,
                )))
                .push(Box::new(self.bound(
                    Button::new("settings.accounts.reauth.cancel", &cancel),
                    AccountsEffect::CancelNewPassword,
                )))
                .push(Box::new(Spacer::new())),
        ));
        card(&t("accounts-reauth-title"), rows(fields))
    }

    /// Reload the real directory after a mutation. The live pane never
    /// substitutes the sample, so `is_sample` stays as constructed (always
    /// `false` for the live pane).
    fn reload(&mut self) {
        self.dir = AccountDirectory::load_in(&self.account_root);
        self.sync_statuses = self.account_root.statuses();
        self.vault = self.account_root.vault();
    }

    fn account_type(kind: AccountKind) -> &'static str {
        kind.type_id()
    }

    /// Build the exact non-secret metadata and sealed credential that will be
    /// checked and, only after that check succeeds, committed. Existing
    /// envelopes are cloned so fields contributed by another app or a newer
    /// provider survive an edit.
    #[allow(
        clippy::too_many_lines,
        reason = "one canonical conversion validates the complete set of account kinds and credential fields"
    )]
    fn prepare_account(
        account_root: &AccountRoot,
        draft: &AccountDraft,
    ) -> Result<(accounts::Account, accounts::Secret), String> {
        let kind = draft.kind;
        let existing_id = draft.existing_id.as_deref();
        let name = draft.name.as_str();
        let password = draft.password.as_str();
        if kind.is_cloud_storage() {
            return Err(format!(
                "{} accounts are connected by signing in",
                kind.label()
            ));
        }
        let identity = draft.identity.trim();
        let username = draft.username.trim();
        let server = draft.server.trim().trim_end_matches('/');
        if name.trim().is_empty() {
            return Err("enter a display name".into());
        }
        if identity.is_empty() {
            return Err(if kind == AccountKind::Mail {
                "enter an email address"
            } else {
                "enter a username"
            }
            .into());
        }
        if kind == AccountKind::Mail
            && (!identity.contains('@') || identity.contains(char::is_whitespace))
        {
            return Err("enter a valid email address".into());
        }
        if kind != AccountKind::Mail {
            let parsed = tilo_dav::Url::parse(server).map_err(|_| "enter a valid server URL")?;
            let localhost_http = cfg!(debug_assertions)
                && parsed.scheme() == "http"
                && parsed.host_str() == Some("localhost");
            if parsed.scheme() != "https" && !localhost_http {
                return Err(
                    "use an HTTPS server URL (localhost HTTP is allowed in development)".into(),
                );
            }
            if !parsed.username().is_empty() || parsed.password().is_some() {
                return Err("remove credentials from the server URL".into());
            }
        }
        if existing_id.is_none() && password.trim().is_empty() {
            return Err("enter a password or app password".into());
        }

        let shared = account_root.store();
        let id = existing_id.map_or_else(
            || {
                draft.assigned_id.clone().unwrap_or_else(|| {
                    accounts::AccountId::from(match kind {
                        AccountKind::Mail => identity.to_ascii_lowercase(),
                        _ => format!("{}-{}", kind.slug(), uuid::Uuid::new_v4()),
                    })
                })
            },
            |id| accounts::AccountId::from(id.to_string()),
        );
        let expected_type = Self::account_type(kind);
        let mut stored = match (existing_id, shared.get(&id)) {
            (Some(_), Some(stored)) if stored.type_id.as_str() == expected_type => stored.clone(),
            (Some(_), Some(_)) => {
                return Err("account type changed; refresh Settings and try again".into());
            }
            (Some(_), None) => return Err("account no longer exists".into()),
            (None, Some(_)) => return Err("this account is already configured".into()),
            (None, None) => accounts::Account::new(id.clone(), expected_type),
        };
        stored.display_name = name.trim().to_string();
        match kind {
            AccountKind::Mail if existing_id.is_none() => {
                let mut config = AccountConfig {
                    email: identity.to_string(),
                    display_name: name.trim().to_string(),
                    username: username.to_string(),
                    ..Default::default()
                };
                config.autofill_from_email();
                merge_config(
                    &mut stored.config,
                    serde_json::to_value(config).map_err(|_| "could not prepare email settings")?,
                );
            }
            AccountKind::Mail => merge_config(
                &mut stored.config,
                serde_json::json!({
                    "email": identity,
                    "display_name": name.trim(),
                    "username": if username.is_empty() { identity } else { username }
                }),
            ),
            AccountKind::CalDav => merge_config(
                &mut stored.config,
                serde_json::json!({"display_name": name.trim(), "server_url": server, "username": identity}),
            ),
            AccountKind::WebDav => merge_config(
                &mut stored.config,
                serde_json::json!({"id": id.to_string(), "label": name.trim(), "base_url": server, "username": identity}),
            ),
            AccountKind::Nextcloud => merge_config(
                &mut stored.config,
                serde_json::json!({"id": id.to_string(), "label": name.trim(), "server_url": server, "username": identity}),
            ),
            AccountKind::GoogleDrive
            | AccountKind::OneDrive
            | AccountKind::Dropbox
            | AccountKind::ICloudDrive => {
                return Err(format!(
                    "{} accounts are connected by signing in",
                    kind.label()
                ));
            }
        }
        let vault = account_root.vault();
        let mut secret = vault.get(&id).unwrap_or_default();
        if password.trim().is_empty() {
            // An unreadable vault is not one without this account's credential.
            if let Some(error) = vault.recovery_error() {
                return Err(format!("the credential vault could not be read: {error}"));
            }
            if secret.is_empty() {
                return Err("this account has no saved credential; enter one".into());
            }
        } else {
            // An edit may add/replace a password on an OAuth-capable account;
            // preserve any provider token fields we do not own.
            secret.password = password.trim().to_string();
        }
        Ok((stored, secret))
    }

    /// Commit a draft after its protocol probe succeeds. The draft is rebuilt
    /// over the latest canonical envelope to preserve concurrent/newer fields.
    fn save_account_with_id(
        account_root: &AccountRoot,
        draft: &AccountDraft,
    ) -> Result<(), String> {
        let (stored, prepared_secret) = Self::prepare_account(account_root, draft)?;
        let password = draft.password.as_str();
        let publishes = matches!(draft.kind, AccountKind::WebDav | AccountKind::Nextcloud);
        let id = stored.id.clone();
        Self::commit_account(
            account_root,
            stored,
            (!password.trim().is_empty()).then_some(&prepared_secret),
        )?;
        if publishes && !password.trim().is_empty() {
            // Replace the daemon's Basic credential right away rather than
            // after its next rejected request.
            if let Err(error) = account_root.custodian().publish(&id, false) {
                warn!(target: "tilo::settings", %error, "could not publish the updated credential");
            }
        }
        Ok(())
    }

    /// Journal and persist `stored` with its credential (when given): the
    /// vault first, so a metadata failure never leaves an account whose
    /// credential is missing.
    fn commit_account(
        account_root: &AccountRoot,
        stored: accounts::Account,
        secret: Option<&accounts::Secret>,
    ) -> Result<(), String> {
        let id = stored.id.clone();
        let mut shared = account_root.store();
        let original_store = shared.clone();
        let original_statuses = account_root.statuses();
        account_root.begin_mutation(&original_store, &original_statuses)?;
        // Persist credential custody first. If metadata then fails, the only
        // residue is an unreachable encrypted vault entry, never an account
        // that applications try to use without its credential.
        let mut previous_secret = None;
        if let Some(prepared_secret) = secret {
            let mut vault = account_root.vault();
            previous_secret = Some(vault.get(&id));
            vault.put(&id, prepared_secret);
            if let Err(error) = vault.try_save() {
                let _ = account_root.finish_mutation();
                return Err(format!("could not save credential: {error}"));
            }
        }
        shared.upsert(stored);
        if let Err(e) = account_root.save_store(&shared) {
            if let Some(previous) = previous_secret {
                let mut vault = account_root.vault();
                match previous {
                    Some(secret) => vault.put(&id, &secret),
                    None => {
                        vault.remove(&id);
                    }
                }
                if let Err(rollback) = vault.try_save() {
                    return Err(format!(
                        "could not persist account: {e}; credential rollback also failed: {rollback}"
                    ));
                }
            }
            let _ = account_root.finish_mutation();
            warn!(target: "tilo::settings", op = "account_store_save", error = %e, "failed to persist account metadata");
            return Err(format!("could not persist account: {e}"));
        }
        finish_or_recover(account_root, "account save")
    }

    #[cfg(test)]
    fn save_account(
        account_root: &AccountRoot,
        kind: AccountKind,
        existing_id: Option<&str>,
        name: &str,
        identity: &str,
        username: &str,
        server: &str,
        password: &str,
    ) -> Result<(), String> {
        Self::save_account_with_id(
            account_root,
            &AccountDraft {
                kind,
                existing_id: existing_id.map(str::to_string),
                assigned_id: None,
                name: name.to_string(),
                identity: identity.to_string(),
                username: username.to_string(),
                server: server.to_string(),
                password: password.to_string(),
            },
        )
    }

    /// Remove an account of `kind` by id from its owning store, persisting.
    pub(crate) fn remove_account(
        account_root: &AccountRoot,
        custodian: &account_custody::Custodian,
        kind: AccountKind,
        id: &str,
    ) -> Result<(), String> {
        let expected_type = kind.type_id();
        let account_id = accounts::AccountId::from(id.to_string());
        let original_store = account_root.store();
        if let Some(account) = original_store.get(&account_id)
            && account.type_id.as_str() != expected_type
        {
            return Err(
                "account type changed before removal; refresh Settings and try again".into(),
            );
        }
        let original_statuses = account_root.statuses();
        account_root.begin_mutation(&original_store, &original_statuses)?;
        let mut vault = account_root.vault();
        let original_secret = vault.get(&account_id);

        // Credential first prevents a metadata-only account from becoming
        // visible if vault persistence fails. Every later failure restores all
        // earlier writes; compensation failures are appended to the result.
        let credential_changed = vault.remove(&account_id);
        if credential_changed && let Err(error) = vault.try_save() {
            let _ = account_root.finish_mutation();
            return Err(format!("could not remove saved credential: {error}"));
        }

        let mut updated_store = original_store.clone();
        updated_store.remove(&account_id);
        if let Err(error) = account_root.save_store(&updated_store) {
            let rollback = if credential_changed {
                restore_secret(account_root, &account_id, original_secret.as_ref())
            } else {
                Ok(())
            };
            let rollback_succeeded = rollback.is_ok();
            let result = with_rollback_error(
                format!("could not remove account metadata: {error}"),
                rollback,
            );
            if rollback_succeeded {
                let _ = account_root.finish_mutation();
            }
            return Err(result);
        }

        let mut updated_statuses = original_statuses;
        updated_statuses.remove(&account_id);
        if let Err(error) = account_root.save_statuses(&updated_statuses) {
            let metadata_rollback = account_root
                .save_store(&original_store)
                .map_err(|e| e.to_string());
            let secret_rollback = if credential_changed {
                restore_secret(account_root, &account_id, original_secret.as_ref())
            } else {
                Ok(())
            };
            let rollback = combine_rollbacks(metadata_rollback, secret_rollback);
            let rollback_succeeded = rollback.is_ok();
            let result = with_rollback_error(
                format!("could not remove account sync status: {error}"),
                rollback,
            );
            if rollback_succeeded {
                let _ = account_root.finish_mutation();
            }
            return Err(result);
        }
        finish_or_recover(account_root, "account removal")?;
        // Give the grant back to the provider and withdraw the daemon's
        // handoff credential. Best effort, off the UI thread: the account is
        // already gone locally.
        if let (Some(account), Some(secret)) = (original_store.get(&account_id), original_secret)
            && account_custody::is_cloud_type(account.type_id.as_str())
        {
            let account = account.clone();
            let custodian = custodian.clone();
            std::thread::spawn(move || custodian.revoke(&account, &secret));
        }
        Ok(())
    }

    fn cancel_form(&mut self) {
        {
            if self.sign_in.is_some() {
                self.close_sign_in();
                return;
            }
            if self.form_kind.is_some() {
                self.form_kind = None;
                self.form_error = None;
                self.form_account_id = None;
                self.form_probe = None;
                self.form_candidate = None;
                self.form_password.set(String::new());
                self.clear_form_fields();
                self.changed = true;
            }
        }
    }

    /// Save starts a bounded protocol probe against a device-local draft. No
    /// metadata or credential is persisted until [`Self::poll_form_probe`]
    /// receives success.
    fn submit_form(&mut self) {
        {
            if let Some(flow) = self.sign_in.as_mut() {
                flow.submit();
                self.changed = true;
                return;
            }
            let name = self.form_name.with(Clone::clone);
            let email = self.form_email.with(Clone::clone);
            let user = self.form_user.with(Clone::clone);
            let server = self.form_server.with(Clone::clone);
            let password = self.form_password.with(Clone::clone);
            let Some(kind) = self.form_kind else {
                return;
            };
            if self.form_probe.is_some() {
                return;
            }
            let identity = if kind == AccountKind::Mail {
                email
            } else {
                user.clone()
            };
            let mut draft = AccountDraft {
                kind,
                existing_id: self.form_account_id.clone(),
                assigned_id: None,
                name,
                identity,
                username: user,
                server,
                password,
            };
            match Self::prepare_account(&self.account_root, &draft) {
                Ok((account, secret)) => {
                    draft.assigned_id = Some(account.id.clone());
                    let tester = std::sync::Arc::clone(&self.account_tester);
                    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
                    std::thread::spawn(move || {
                        let _ = sender.send(tester.test(&account, &secret));
                    });
                    self.form_candidate = Some(draft);
                    self.form_probe = Some(ProbeJob::new(receiver));
                    self.form_error = Some(t("accounts-checking"));
                    self.changed = true;
                }
                Err(error) => {
                    self.form_error = Some(error);
                    self.changed = true;
                }
            }
        }
    }

    fn poll_form_probe(&mut self) {
        let result = self
            .form_probe
            .as_ref()
            .and_then(|job| job.poll("account check stopped unexpectedly"));
        let Some(result) = result else {
            return;
        };
        self.form_probe = None;
        if let Err(error) = result {
            self.form_candidate = None;
            self.form_error = Some(error);
        } else {
            let Some(candidate) = self.form_candidate.take() else {
                self.form_error = Some("account check result had no pending draft".into());
                self.changed = true;
                return;
            };
            match Self::save_account_with_id(&self.account_root, &candidate) {
                Ok(()) => {
                    self.form_kind = None;
                    self.form_error = None;
                    self.form_account_id = None;
                    self.clear_form_fields();
                    self.reload();
                }
                Err(error) => self.form_error = Some(error),
            }
        }
        self.changed = true;
    }

    fn cancel_reauth(&mut self) {
        {
            self.reauth_id = None;
            self.reauth_error = None;
            self.reauth_probe = None;
            self.reauth_candidate = None;
            self.reauth_password.set(String::new());
            self.changed = true;
        }
    }

    fn submit_reauth(&mut self) {
        {
            if self.reauth_probe.is_some() {
                return;
            }
            if let Some(id) = self.reauth_id.clone() {
                let password = self.reauth_password.with(Clone::clone);
                if password.trim().is_empty() {
                    self.reauth_error = Some("enter a password or app password".into());
                    self.changed = true;
                    return;
                }
                let account_id = accounts::AccountId::from(id);
                let Some(account) = self.account_root.store().get(&account_id).cloned() else {
                    self.reauth_error = Some(t("accounts-gone"));
                    self.changed = true;
                    return;
                };
                let mut secret = self
                    .account_root
                    .vault()
                    .get(&account_id)
                    .unwrap_or_default();
                secret.password = password.trim().to_string();
                let tester = std::sync::Arc::clone(&self.account_tester);
                let candidate = secret.clone();
                let (sender, receiver) = std::sync::mpsc::sync_channel(1);
                std::thread::spawn(move || {
                    let _ = sender.send(tester.test(&account, &candidate));
                });
                self.reauth_candidate = Some((account_id, secret));
                self.reauth_probe = Some(ProbeJob::new(receiver));
                self.reauth_error = Some(t("accounts-checking-reauth"));
                self.changed = true;
            }
        }
    }

    fn poll_reauth_probe(&mut self) {
        let result = self
            .reauth_probe
            .as_ref()
            .and_then(|job| job.poll("credential check stopped unexpectedly"));
        let Some(result) = result else {
            return;
        };
        self.reauth_probe = None;
        if let Err(error) = result {
            self.reauth_candidate = None;
            self.reauth_error = Some(error);
        } else {
            let Some((account_id, secret)) = self.reauth_candidate.take() else {
                self.reauth_error = Some("credential check result had no pending draft".into());
                self.changed = true;
                return;
            };
            let mut vault = self.account_root.vault();
            vault.put(&account_id, &secret);
            match vault.try_save() {
                Ok(()) => {
                    self.reauth_id = None;
                    self.reauth_error = None;
                    self.reauth_password.set(String::new());
                }
                Err(error) => self.reauth_error = Some(error),
            }
        }
        self.changed = true;
    }
}

impl AccountsPane {
    /// Remove the account `id`, its credential and its sync record.
    ///
    /// # Errors
    ///
    /// Why the account could not be removed.
    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        if self.is_sample {
            return Err("example accounts cannot be removed".into());
        }
        let stored = self
            .account_root
            .store()
            .get(&accounts::AccountId::from(id.to_owned()))
            .cloned()
            .ok_or_else(|| t("accounts-gone"))?;
        let kind = AccountKind::from_type_id(stored.type_id.as_str())
            .ok_or_else(|| "unsupported account type".to_string())?;
        Self::remove_account(&self.account_root, &self.custodian, kind, id)?;
        self.reload();
        self.changed = true;
        Ok(())
    }

    /// The check of account `id`'s saved sign-in, to run on a worker: a
    /// protocol probe, or for a cloud drive a fresh token from the provider.
    fn connection_check(
        &self,
        id: &str,
    ) -> Result<impl FnOnce() -> Result<String, String> + Send + 'static, String> {
        let account_id = accounts::AccountId::from(id.to_owned());
        let account = self
            .account_root
            .store()
            .get(&account_id)
            .cloned()
            .ok_or_else(|| t("accounts-gone"))?;
        let secret = self.account_root.vault().get(&account_id).ok_or_else(|| {
            "this account has no saved credential; sign in again first".to_string()
        })?;
        let cloud = AccountKind::from_type_id(account.type_id.as_str())
            .is_some_and(AccountKind::is_cloud_storage);
        let custodian = self.custodian.clone();
        let tester = std::sync::Arc::clone(&self.account_tester);
        Ok(move || {
            if cloud {
                custodian
                    .publish(&account_id, true)
                    .map(|()| "The provider accepted the saved sign-in".to_string())
                    .map_err(|error| error.to_string())
            } else {
                tester.test(&account, &secret)
            }
        })
    }

    /// Take in finished checks and sign-in steps; whether anything shown
    /// changed.
    pub fn poll(&mut self) -> bool {
        let revision = self.account_root.revision();
        if !self.is_sample && revision != self.seen_accounts {
            if self.seen_accounts.is_some() {
                self.reload();
                if let Err(error) = self.account_root.project() {
                    tracing::warn!(%error, "the device's account file could not follow the person's accounts");
                }
                self.changed = true;
            }
            self.seen_accounts = revision;
        }
        self.poll_form_probe();
        self.poll_sign_in();
        self.poll_reauth_probe();
        if !self.is_sample && self.cloud.poll() {
            self.changed = true;
        }
        std::mem::take(&mut self.changed)
    }
}

impl PaneModel for AccountsPane {
    fn compose(&self) -> Box<dyn Widget> {
        self.build_composition()
    }

    fn poll(&mut self) -> bool {
        AccountsPane::poll(self)
    }

    fn name(&self) -> &'static str {
        "AccountsPane"
    }

    fn busy(&self) -> bool {
        self.form_probe.is_some()
            || self.reauth_probe.is_some()
            || self
                .sign_in
                .as_ref()
                .is_some_and(cloud_signin::SignInFlow::busy)
    }
}

fn relative_sync_time(unix: i64) -> String {
    let now = ui::time::unix_now_i64();
    let age = now.saturating_sub(unix).max(0);
    match age {
        0..=59 => t("accounts-just-now"),
        60..=3_599 => t_args("accounts-min-ago", &[("count", (age / 60).into())]),
        3_600..=86_399 => t_args("accounts-h-ago", &[("count", (age / 3_600).into())]),
        _ => t_args("accounts-d-ago", &[("count", (age / 86_400).into())]),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "callers construct per-kind JSON values solely to merge them into the persisted opaque configuration"
)]
fn merge_config(target: &mut serde_json::Value, updates: serde_json::Value) {
    let Some(updates) = updates.as_object() else {
        return;
    };
    if !target.is_object() {
        *target = serde_json::Value::Object(serde_json::Map::new());
    }
    if let Some(target) = target.as_object_mut() {
        for (key, value) in updates {
            target.insert(key.clone(), value.clone());
        }
    }
}

fn restore_secret(
    account_root: &AccountRoot,
    id: &accounts::AccountId,
    secret: Option<&accounts::Secret>,
) -> Result<(), String> {
    let mut vault = account_root.vault();
    match secret {
        Some(secret) => vault.put(id, secret),
        None => {
            vault.remove(id);
        }
    }
    vault.try_save()
}

fn combine_rollbacks(first: Result<(), String>, second: Result<(), String>) -> Result<(), String> {
    match (first, second) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(first), Err(second)) => Err(format!("{first}; {second}")),
    }
}

fn with_rollback_error(error: String, rollback: Result<(), String>) -> String {
    match rollback {
        Ok(()) => error,
        Err(rollback) => format!("{error}; rollback also failed: {rollback}"),
    }
}

fn finish_or_recover(account_root: &AccountRoot, operation: &str) -> Result<(), String> {
    if let Err(error) = account_root.finish_mutation() {
        return match account_root.recover_mutation() {
            Ok(()) => Err(format!(
                "could not complete {operation}: {error}; changes were rolled back"
            )),
            Err(recovery) => Err(format!(
                "could not complete {operation}: {error}; recovery also failed: {recovery}"
            )),
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::sample_directory;
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};
    use ui::Theme;

    struct RejectCandidate;

    struct AcceptSavedCredential;

    impl crate::account_probe::AccountTester for AcceptSavedCredential {
        fn test(
            &self,
            account: &accounts::Account,
            secret: &accounts::Secret,
        ) -> Result<String, String> {
            if account.id.as_str() != "connection@example.test"
                || secret.password != "test-only-password"
            {
                return Err("the probe did not receive the saved account credential".into());
            }
            Ok("The server accepted the saved sign-in".into())
        }
    }

    #[test]
    fn account_check_returns_its_saved_credential_probe_result() -> Result<(), String> {
        let (_dir, root) = scratch_root();
        let id = accounts::AccountId::from("connection@example.test");
        let account = accounts::Account::new(id.clone(), "email");
        let mut store = accounts::AccountStore::new();
        store.upsert(account);
        root.save_store(&store).map_err(|error| error.to_string())?;
        let mut vault = root.vault();
        vault.put(&id, &accounts::Secret::password("test-only-password"));
        vault.try_save()?;
        let pane = AccountsPane::in_root(root.clone())
            .with_account_tester(std::sync::Arc::new(AcceptSavedCredential));
        let check = pane.connection_check(id.as_str())?;
        let Effected::Done(output) = complete_account_check(check())? else {
            return Err("the finished account check remained pending".into());
        };
        let effect = AccountsEffect::TestAccount {
            account: id.as_str().into(),
        };
        let spec = effect.bound();
        spec.contract.result.validate(&output)?;
        if output.value != Some(serde_json::json!("The server accepted the saved sign-in"))
            || !output.resources.is_empty()
        {
            return Err("the account check must return its exact status without resources".into());
        }
        if root.vault().get(&id) != vault.get(&id) {
            return Err("checking the account must preserve its saved credential".into());
        }
        let rejecting =
            AccountsPane::in_root(root).with_account_tester(std::sync::Arc::new(RejectCandidate));
        let check = rejecting.connection_check(id.as_str())?;
        if complete_account_check(check()) != Err("server rejected candidate".into()) {
            return Err("the account check must propagate the probe's rejection".into());
        }
        Ok(())
    }

    impl crate::account_probe::AccountTester for RejectCandidate {
        fn test(
            &self,
            _account: &accounts::Account,
            _secret: &accounts::Secret,
        ) -> Result<String, String> {
            Err("server rejected candidate".into())
        }
    }

    /// A private config root: the stores, vault and mutation journal of one
    /// test never meet another's, and nothing reaches the user's own.
    fn scratch_root() -> (tempfile::TempDir, AccountRoot) {
        let dir = tempfile::tempdir().expect("scratch config root");
        let root = AccountRoot::at(dir.path().join("tilo"));
        (dir, root)
    }

    /// Drive the host's polling until the pending account probe reports,
    /// however long the probe thread takes to be scheduled.
    fn settle_probes(pane: &mut AccountsPane) {
        let deadline = ui::time::now() + ProbeJob::DEADLINE;
        loop {
            let _ = pane.poll();
            if pane.form_probe.is_none() && pane.reauth_probe.is_none() {
                return;
            }
            assert!(ui::time::now() < deadline, "account probe never reported");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn count_named(w: &dyn Widget, name: &str) -> usize {
        let mut n = usize::from(w.name() == name);
        for c in w.children() {
            n += count_named(c, name);
        }
        n
    }

    fn button_labels(w: &dyn Widget, labels: &mut Vec<String>) {
        if w.name() == "Button"
            && let Some(label) = w.accessibility().and_then(|node| node.name)
        {
            labels.push(label);
        }
        for c in w.children() {
            button_labels(c, labels);
        }
    }

    fn access_nodes(w: &dyn Widget, nodes: &mut Vec<ui::a11y::AccessNode>) {
        if let Some(node) = w.accessibility() {
            nodes.push(node);
        }
        for child in w.children() {
            access_nodes(child, nodes);
        }
    }

    fn lay_out(pane: &AccountsPane) -> Box<dyn Widget> {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut composed = pane.build_composition();
        let _ = composed.build_layout(&mut taffy, &theme, &mut node_map);
        composed
    }

    /// Sample directories are read-only, real ones get labelled destructive remove buttons, and empty stores stay empty.
    #[test]
    fn accounts_pane_renders_only_real_accounts_as_removable() {
        // Case `sample_directory_renders_read_only_without_remove_buttons`.
        {
            // The sample is illustration: one live Add button per provider kind,
            // and no Remove buttons. A scratch root keeps whatever the runner's
            // own account stores hold (a browser identity, say) out of the count.
            let (_scratch, root) = scratch_root();
            let pane = AccountsPane::from_dir_in(root, sample_directory(), true);
            let composed = lay_out(&pane);
            assert_eq!(
                count_named(composed.as_ref(), "Button"),
                AccountKind::ALL.len(),
                "every provider kind can be added over the sample"
            );
        }

        // Case `real_directory_renders_remove_buttons`.
        {
            // Without stored passwords, real rows offer removal but do not invent
            // a reauthentication action.
            let dir = sample_directory(); // 2 mail + 1 cal + 1 nextcloud = 4 accounts
            let (_scratch, root) = scratch_root();
            let pane = AccountsPane::from_dir_in(root, dir, false);
            let composed = lay_out(&pane);
            // One Add per kind + four maintenance actions on each of four accounts.
            assert_eq!(
                count_named(composed.as_ref(), "Button"),
                AccountKind::ALL.len() + 16
            );
        }

        // Case `account_removal_is_labeled_and_visibly_destructive`.
        {
            // Real accounts get an explicit Remove action rather than a lone
            // trash glyph. The confirmation flow still owns the mutation; this
            // assertion guards the decision point users see before it.
            let (_scratch, root) = scratch_root();
            let pane = AccountsPane::from_dir_in(root, sample_directory(), false);
            let composed = lay_out(&pane);
            let mut labels = Vec::new();
            button_labels(composed.as_ref(), &mut labels);
            assert_eq!(
                labels
                    .iter()
                    .filter(|label| label.as_str() == "Remove")
                    .count(),
                4,
                "one explicit Remove action per real account"
            );
            assert_eq!(
                labels
                    .iter()
                    .filter(|label| label.as_str() == "Add account")
                    .count(),
                AccountKind::ALL.len(),
                "each provider card exposes a compact add action"
            );
        }

        // Case `live_pane_over_empty_store_does_not_fabricate_sample`.
        {
            let (_dir, root) = scratch_root();
            let pane = AccountsPane::in_root(root);
            assert!(!pane.is_sample(), "the live pane never flags the sample");
            assert!(
                pane.directory().is_empty(),
                "an empty store renders empty — no fabricated ada@example.com rows"
            );
            // And no account of any kind leaked in from the sample.
            for kind in AccountKind::ALL {
                assert!(
                    pane.directory().views_of(kind).is_empty(),
                    "{kind:?} must be empty"
                );
            }
        }
    }

    /// Removing an account asks first; a connection check reports later.
    #[test]
    fn removal_asks_first_and_checks_run_as_jobs() {
        let declarations = AccountsEffect::declarations();
        let declared = |id: &str| {
            declarations
                .iter()
                .find(|declaration| declaration.id == id)
                .unwrap()
        };
        assert_eq!(
            declared(AccountsEffect::REMOVE_ACCOUNT.id).risk,
            ui::tilo_actions::RiskLevel::Destructive
        );
        assert!(matches!(
            declared(AccountsEffect::TEST_ACCOUNT.id).answers,
            ui::tilo_resource::effect::Answers::Later { .. }
        ));
    }

    /// The inline add form: local drafts, labelled fields, empty-email no-op and rejected adds.
    #[test]
    fn account_form_drafts_are_local_accessible_and_validated() {
        // Case `add_button_opens_the_inline_form`.
        {
            let mut pane = AccountsPane::from_dir(sample_directory(), true);
            assert!(!pane.form_open());
            pane.command(AccountViewCommand::Add(AccountKind::Mail));
            assert!(pane.form_open(), "Add opens the form");
            assert!(pane.poll(), "opening the form requests a rebuild");
            // Cancel closes it.
            pane.command(AccountViewCommand::Cancel);
            assert!(!pane.form_open(), "Cancel closes the form");
        }

        // Case `form_drafts_are_local_and_every_field_has_explicit_accessibility`.
        {
            let mut pane = AccountsPane::from_dir(sample_directory(), true);
            pane.command(AccountViewCommand::Add(AccountKind::WebDav));
            pane.set_form_fields_for_test("Home files", "", "ada", "https://files.example.test");
            pane.set_form_password_for_test("never-collaborate-this");
            let _ = pane.poll();
            let composed = lay_out(&pane);
            let mut nodes = Vec::new();
            access_nodes(composed.as_ref(), &mut nodes);
            for name in [
                "Display name",
                "Username",
                "Server URL",
                "Password or app password",
            ] {
                assert!(
                    nodes.iter().any(|node| {
                        node.role == ui::a11y::Role::TextInput && node.name.as_deref() == Some(name)
                    }),
                    "missing named text input: {name}"
                );
            }
            let password = nodes
                .iter()
                .find(|node| {
                    node.role == ui::a11y::Role::TextInput
                        && node.name.as_deref() == Some("Password or app password")
                })
                .expect("password field semantics");
            assert!(password.state.protected);
            assert_eq!(
                password.value, None,
                "screen readers must not receive secrets"
            );
        }

        // Case `save_with_empty_email_is_a_noop`.
        {
            // Guard: saving a blank address must not create an account (and must
            // not touch disk). We assert the pure guard directly.
            let (_dir, root) = scratch_root();
            assert!(
                AccountsPane::save_account(
                    &root,
                    AccountKind::Mail,
                    None,
                    "Nobody",
                    "  ",
                    "",
                    "",
                    ""
                )
                .is_err()
            );
            assert!(root.store().is_empty());
        }

        // Case `add_rejection_keeps_the_draft_open_and_persists_nothing`.
        {
            let (_dir, root) = scratch_root();
            let mut pane = AccountsPane::in_root(root.clone())
                .with_account_tester(std::sync::Arc::new(RejectCandidate));
            pane.command(AccountViewCommand::Add(AccountKind::Mail));
            pane.set_form_fields_for_test(
                "Rejected",
                "rejected@example.test",
                "rejected@example.test",
                "",
            );
            pane.set_form_password_for_test("candidate-password");
            pane.command(AccountViewCommand::Submit);
            settle_probes(&mut pane);
            assert_eq!(
                pane.form_error.as_deref(),
                Some("server rejected candidate")
            );
            assert!(pane.form_open(), "a rejected draft remains editable");
            assert!(root.store().is_empty());
            assert!(
                root.vault()
                    .get(&accounts::AccountId::from("rejected@example.test"))
                    .is_none(),
                "a rejected credential never reaches the vault"
            );
        }
    }

    #[test]
    fn vault_save_failure_preserves_existing_account_metadata() {
        // Isolate custody selection from an actual session Secret Service, and
        // isolate ConfigGuard's environment from other tests in this process.
        const CHILD: &str = "TILO_SETTINGS_VAULT_FAILURE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "panes::accounts::tests::vault_save_failure_preserves_existing_account_metadata", "--nocapture"])
                .env(CHILD, "1")
                .env("PATH", "")
                .status().unwrap();
            assert!(status.success());
            return;
        }
        let (_dir, account_root) = scratch_root();
        let root = account_root.path().unwrap();
        let old_id = accounts::AccountId::from("existing@example.test");
        let mut store = accounts::AccountStore::new();
        store.upsert(accounts::Account::new(old_id.clone(), "email"));
        account_root.save_store(&store).unwrap();
        let store_path = accounts::store_path_in(&root);
        let metadata_before = std::fs::read(&store_path).unwrap();
        let mut vault = accounts::SecretVault::open_in(&root);
        vault.put(&old_id, &accounts::Secret::password("old-password"));
        vault.try_save().unwrap();
        assert_eq!(
            accounts::SecretVault::open_in(&root).custody(),
            accounts::SecretCustody::LocalFile
        );
        let dir = accounts::vault_dir_in(&root);
        let ciphertext_before = std::fs::read(dir.join("vault.enc")).unwrap();
        let malformed_key = [7u8; 31];
        std::fs::write(dir.join("vault.key"), malformed_key).unwrap();

        let error = AccountsPane::save_account(
            &account_root,
            AccountKind::Mail,
            None,
            "New account",
            "new@example.test",
            "new@example.test",
            "",
            "new-password",
        )
        .unwrap_err();
        assert!(
            error.starts_with("could not save credential:"),
            "must reach vault save, not fail account intent: {error}"
        );
        assert_eq!(std::fs::read(dir.join("vault.key")).unwrap(), malformed_key);
        assert_eq!(
            std::fs::read(dir.join("vault.enc")).unwrap(),
            ciphertext_before
        );
        assert_eq!(std::fs::read(&store_path).unwrap(), metadata_before);
        let saved = account_root.store();
        assert_eq!(saved.all().len(), 1);
        assert!(saved.get(&old_id).is_some());
        assert!(
            saved
                .get(&accounts::AccountId::from("new@example.test"))
                .is_none()
        );
    }

    /// Edits keep unknown config and toggles; re-authentication probes before persisting.
    #[test]
    fn account_edits_preserve_config_and_probe_before_persisting() {
        // Case `edit_preserves_envelope_unknown_config_and_service_toggles`.
        {
            let (_dir, root) = scratch_root();
            let id = accounts::AccountId::from("nextcloud-stable-id");
            let mut original = accounts::Account::new(id.clone(), crate::accounts::NEXTCLOUD_TYPE);
            original.display_name = "Old label".into();
            original.enabled = false;
            original.tint = 7;
            original.owner = Some("local-owner".into());
            original.config = serde_json::json!({
                "id": "nextcloud-stable-id",
                "label": "Old label",
                "server_url": "https://old.example",
                "username": "old-user",
                "use_files": false,
                "use_calendar": true,
                "use_mail": false,
                "future_provider_field": {"kept": true}
            });
            let mut store = accounts::AccountStore::new();
            store.upsert(original);
            root.save_store(&store).expect("seed canonical account");
            let mut vault = root.vault();
            vault.put(
                &id,
                &accounts::Secret {
                    password: "old-password".into(),
                    token: Some("future-provider-token".into()),
                },
            );
            vault.try_save().expect("seed credential");

            AccountsPane::save_account(
                &root,
                AccountKind::Nextcloud,
                Some(id.as_str()),
                "New label",
                "new-user",
                "",
                "https://new.example/",
                "",
            )
            .expect("edit account");

            let saved = root.store().get(&id).cloned().expect("saved account");
            assert!(!saved.enabled);
            assert_eq!(saved.tint, 7);
            assert_eq!(saved.owner.as_deref(), Some("local-owner"));
            assert_eq!(saved.config["use_files"], false);
            assert_eq!(saved.config["use_calendar"], true);
            assert_eq!(saved.config["use_mail"], false);
            assert_eq!(saved.config["future_provider_field"]["kept"], true);
            assert_eq!(saved.config["server_url"], "https://new.example");
            assert_eq!(saved.config["username"], "new-user");
            assert_eq!(
                root.vault().get(&id),
                Some(accounts::Secret {
                    password: "old-password".into(),
                    token: Some("future-provider-token".into()),
                })
            );
        }

        // Case `reauthentication_probes_candidate_before_persisting`.
        {
            let (_dir, root) = scratch_root();
            let id = accounts::AccountId::from("reauth@example.test");
            let mut account = accounts::Account::new(id.clone(), "email");
            account.display_name = "Reauth".into();
            account.config = serde_json::json!({
                "email": id.as_str(), "username": id.as_str(),
                "imap_host": "imap.example.test", "imap_port": 993
            });
            let mut store = accounts::AccountStore::new();
            store.upsert(account);
            root.save_store(&store).expect("seed account");
            let mut vault = root.vault();
            vault.put(&id, &accounts::Secret::password("old-password"));
            vault.try_save().expect("seed credential");
            let mut pane = AccountsPane::in_root(root.clone())
                .with_account_tester(std::sync::Arc::new(RejectCandidate));
            pane.command(AccountViewCommand::Reauthenticate(id.to_string()));
            pane.set_reauth_password_for_test("candidate-password");
            pane.command(AccountViewCommand::SubmitReauthentication);
            settle_probes(&mut pane);
            assert_eq!(
                pane.reauth_error_for_test(),
                Some("server rejected candidate")
            );
            assert_eq!(
                root.vault().get(&id).map(|secret| secret.password),
                Some("old-password".into()),
                "rejected candidate must never replace the stored credential"
            );
        }
    }

    /// Sync now and pause or resume ride on exactly the accounts the cloud
    /// service lists as mounted, and the window offers the same controls.
    #[test]
    fn account_actions_include_sync_now_for_mounted_accounts() {
        use runtime_fs::cloud::{CloudCapabilities, CloudMount, MountHealth};
        let mount = |account: &str, paused: bool| CloudMount {
            account: account.into(),
            mount_point: std::path::PathBuf::from("/home/ada/Remote").join(account),
            health: Some(MountHealth::Healthy),
            paused,
            capabilities: CloudCapabilities::default(),
        };
        for paused in [false, true] {
            let (_scratch, root) = scratch_root();
            // Sample accounts take fresh ids each time: read them back.
            let pane = AccountsPane::from_dir_in(root.clone(), sample_directory(), false);
            let files = pane.directory().views_of(AccountKind::Nextcloud)[0]
                .id
                .clone();
            let mail = pane.directory().views_of(AccountKind::Mail)[0].id.clone();
            let pane = pane.with_cloud_mounts(CloudMounts::known(vec![mount(&files, paused)]));
            let composed = lay_out(&pane);
            let mut labels = Vec::new();
            button_labels(composed.as_ref(), &mut labels);
            let profile = tempfile::tempdir().unwrap();
            let cx = PaneCx::in_file(profile.path().join("settings.json"));
            let area = AccountsArea::new(&cx).with_account_root(root);
            let _ = area.model.set(Rc::new(std::cell::RefCell::new(pane)));
            let actions = |account: &str| -> Vec<String> {
                let row = format!("settings.account:{account}");
                area.rows()
                    .into_iter()
                    .find(|published| published.id() == row)
                    .map(|published| {
                        published
                            .into_resource()
                            .actions
                            .into_iter()
                            .map(|spec| spec.id)
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let mounted = actions(&files);
            assert!(mounted.contains(&"sync-now".to_owned()), "{mounted:?}");
            let (offered, withheld) = if paused {
                ("resume-sync", "pause-sync")
            } else {
                ("pause-sync", "resume-sync")
            };
            assert!(mounted.contains(&offered.to_owned()), "{mounted:?}");
            assert!(!mounted.contains(&withheld.to_owned()), "{mounted:?}");
            let unmounted = actions(&mail);
            assert!(
                unmounted.contains(&"test-account".to_owned()),
                "{unmounted:?}"
            );
            assert!(
                unmounted
                    .iter()
                    .all(|id| !matches!(id.as_str(), "sync-now" | "pause-sync" | "resume-sync")),
                "an account the service has not mounted offers no sync control: {unmounted:?}"
            );
            assert_eq!(
                labels
                    .iter()
                    .filter(|label| label.as_str() == "Sync now")
                    .count(),
                1,
                "{labels:?}"
            );
            let toggle = if paused { "Resume sync" } else { "Pause sync" };
            assert_eq!(
                labels
                    .iter()
                    .filter(|label| label.as_str() == toggle)
                    .count(),
                1,
                "{labels:?}"
            );
        }
    }
}
