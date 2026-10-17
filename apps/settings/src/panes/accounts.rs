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

use accounts::email::AccountConfig;
use tracing::warn;
use ui::framework::{ActionControl, EffectAnswer, EffectCx, Effected, LiveRow};
use ui::tilo_resource::{self, Effect};
use ui::widget::{Button, Row, Spacer, TextInput, TextInputKind, Widget};

use crate::accounts::{AccountDirectory, AccountKind, AccountRoot, AccountView};
use crate::effects::SettingsEffect;
use crate::host::PaneCx;
use crate::i18n::{t, t_args};
use crate::panes::model::{PaneModel, Shared};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    Body, El, Item, SettingsSection, StatusTone, card, field_row, item_row, note, row, rows,
    semantic_status_pill, setting_row, setting_row_described, untitled, value_row,
};

mod browser_sessions;
mod calendar_selection;
mod cloud_control;
mod cloud_signin;
mod import_review;
mod thunderbird_import;
mod vault_lane;
pub(crate) use cloud_control::CloudMounts;
pub use cloud_signin::{BrowserOpener, PortalOpener};
use runtime_fs::cloud::CloudRequest;
use vault_lane::Step;
// Drives the FUSE-backed cloud service, which exists only on Linux.
#[cfg(test)]
#[cfg(target_os = "linux")]
mod cloud_import_acceptance;
#[cfg(test)]
mod signin_tests;

/// Adding, editing, checking and removing the person's external accounts.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum AccountsEffect {
    /// Forget one selected website account through the browser, including its local lease.
    #[effect(row, job, returns, device_only, destructive)]
    RemoveBrowserServiceSession { session_id: String },
    /// Discover only ordinary account metadata from a granted prefs.js path.
    #[effect(job, returns, device_only, kind = "read")]
    DiscoverThunderbirdAccounts { prefs: String },
    /// Choose ordinary Thunderbird prefs through the existing file portal.
    #[effect(job, returns, device_only, kind = "read")]
    ChooseThunderbirdMetadata,
    /// Read the current metadata review and its exact revision; no secrets.
    #[effect(returns, kind = "read")]
    InspectThunderbirdImport,
    /// Select a reviewed identity and an explicit supported sign-in method.
    #[effect(view)]
    SelectThunderbirdAccount {
        source: String,
        authentication: String,
        selected: bool,
        revision: u64,
    },
    /// Import only selected configuration as pending sign-in metadata.
    #[effect(job, returns, device_only, idempotent)]
    CommitThunderbirdImport { revision: u64 },
    /// Discard the metadata review and fence unfinished discovery callbacks.
    #[effect(view)]
    CancelThunderbirdImport,
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
    /// Choose calendars on an existing Nextcloud account, reusing its sign-in.
    #[effect(row, device_only, label = "Connect calendars")]
    ConnectNextcloudCalendars { account: String },
    /// Read the discovered, secret-free calendar choices for the current form.
    #[effect(returns, kind = "read")]
    InspectCalendarChoices,
    /// Select a discovered collection before saving this account.
    #[effect(view)]
    SelectCalendar { href: String, selected: bool },
    /// Select the services on the open provider connection form.
    #[effect(view)]
    SelectAccountServices {
        files: bool,
        mail: bool,
        #[param("Files mode: sync or import")]
        files_mode: String,
    },
    /// Compatibility action requesting both Google services.
    #[effect(label = "Continue with Files and Mail")]
    ConnectGoogleMail,
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
    #[effect(row, job, destructive, label = "Remove account")]
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
    /// Recover an interrupted Files mode change after closing the connection form.
    #[effect(row, job, device_only, idempotent, label = "Recover mode change")]
    RecoverCloudMode { account: String },
}

/// Answer `effect` on the area's model; a connection check runs on a
/// worker, and what changes the credential vault answers once the vault
/// took it, in turn with the account changes under way (`vault_lane`).
pub(crate) fn answer(
    app: &SettingsApp,
    effect: AccountsEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    match effect {
        AccountsEffect::RemoveBrowserServiceSession { session_id } => {
            let area = app.accounts();
            if area.removal.borrow().is_some() {
                return Err("An account removal is already running".into());
            }
            if !area
                .model()
                .borrow()
                .browser_service_sessions()
                .iter()
                .any(|account| account.id.as_str() == session_id)
            {
                return Err("This selected browser account is unavailable".into());
            }
            {
                let mut model = area.model().borrow_mut();
                model.browser_removal_pending = true;
                model.browser_removal_error = None;
                model.changed = true;
            }
            *area.removal.borrow_mut() = Some(browser_sessions::Removal::new(
                session_id,
                cx.reply(),
                &area.remote,
            ));
            return Ok(Effected::Pending);
        }
        AccountsEffect::DiscoverThunderbirdAccounts { prefs } => {
            return thunderbird_import::discover(app, prefs, cx);
        }
        AccountsEffect::ChooseThunderbirdMetadata => return thunderbird_import::choose(app, cx),
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
    answer_model(app, effect, cx)
}

fn answer_model(
    app: &SettingsApp,
    effect: AccountsEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let shared = app.accounts().model();
    let mut model = shared.borrow_mut();
    match effect {
        AccountsEffect::DiscoverThunderbirdAccounts { .. }
        | AccountsEffect::ChooseThunderbirdMetadata => {
            return Err(t("accounts-import-read-failed"));
        }
        AccountsEffect::InspectThunderbirdImport => {
            return model.import_review.inspect().map(Effected::value);
        }
        AccountsEffect::SelectThunderbirdAccount {
            source,
            authentication,
            selected,
            revision,
        } => {
            model
                .import_review
                .select(&source, &authentication, selected, revision)?;
            model.changed = true;
        }
        AccountsEffect::CommitThunderbirdImport { revision } => {
            return commit_thunderbird_import(&mut model, revision, cx);
        }
        AccountsEffect::CancelThunderbirdImport => {
            model.import_review.cancel()?;
            model.changed = true;
        }
        AccountsEffect::RemoveBrowserServiceSession { .. }
        | AccountsEffect::PublishBrowserIdentity { .. }
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
        AccountsEffect::ConnectNextcloudCalendars { account } => {
            model.connect_nextcloud_calendars(&account)?;
            app.request_pane(crate::categories::Category::ExternalAccounts);
        }
        AccountsEffect::InspectCalendarChoices => {
            return Ok(Effected::value(model.calendar_choice_summary()));
        }
        AccountsEffect::SelectCalendar { href, selected } => {
            model.select_calendar(&href, selected)?;
            app.request_pane(crate::categories::Category::ExternalAccounts);
        }
        AccountsEffect::SaveAccountForm => {
            if !model.calendar_choices.is_empty()
                && !model.calendar_choices.iter().any(|(_, selected)| *selected)
            {
                return Err(t("accounts-calendars-empty"));
            }
            if !model.calendar_choices.is_empty() {
                model.validate_calendar_review()?;
            }
            model.command(AccountViewCommand::Submit);
        }
        AccountsEffect::ConnectGoogleMail => model.connect_google_mail()?,
        AccountsEffect::SelectAccountServices {
            files,
            mail,
            files_mode,
        } => {
            model.select_account_services(files, mail, &files_mode)?;
        }
        AccountsEffect::CancelAccountForm => model.command(AccountViewCommand::Cancel),
        AccountsEffect::SaveNewPassword => {
            model.command(AccountViewCommand::SubmitReauthentication);
        }
        AccountsEffect::CancelNewPassword => {
            model.command(AccountViewCommand::CancelReauthentication);
        }
        AccountsEffect::RemoveAccount { account } => {
            if model.is_sample {
                return Err(SAMPLE_NOT_REMOVABLE.into());
            }
            let reply = cx.reply();
            model.remove_then(&account, move |result| {
                reply.finish(result.map(|()| ui::tilo_actions::ActionOutput::default()));
            });
            return Ok(Effected::Pending);
        }
        AccountsEffect::SyncNow { account } => {
            return cloud(model, CloudRequest::SyncNow { account }, cx);
        }
        AccountsEffect::PauseSync { account } => {
            return cloud(model, CloudRequest::Pause { account }, cx);
        }
        AccountsEffect::ResumeSync { account } => {
            return cloud(model, CloudRequest::Resume { account }, cx);
        }
        AccountsEffect::RecoverCloudMode { account } => return recover_mode(model, account, cx),
        AccountsEffect::TestAccount { account } => return test_account(model, &account, cx),
    }
    Ok(Effected::done())
}

fn commit_thunderbird_import(
    model: &mut AccountsPane,
    revision: u64,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    // No account work under way: answered at once, as before.
    if model.lane.is_idle() {
        let root = model.account_root.clone();
        let result = model.import_review.commit(&root, revision)?;
        model.queue_reload();
        model.changed = true;
        return Ok(Effected::value(result));
    }
    // Otherwise in turn with the account mutations under way, which
    // journal the stores this changes.
    let reply = cx.reply();
    model.run_in_lane(move |pane| {
        let root = pane.account_root.clone();
        let result = pane.import_review.commit(&root, revision);
        if result.is_ok() {
            pane.queue_reload();
            pane.changed = true;
        }
        reply.finish(result.map(|status| ui::tilo_actions::ActionOutput {
            value: Some(serde_json::Value::String(status)),
            resources: Vec::new(),
        }));
        Step::Done
    });
    Ok(Effected::Pending)
}

fn recover_mode(
    model: std::cell::RefMut<'_, AccountsPane>,
    account: String,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    if model.sign_in.is_some() {
        return Err("Close the account connection form before recovering its mode change".into());
    }
    cloud(model, CloudRequest::RecoverImport { account }, cx)
}

fn test_account(
    model: std::cell::RefMut<'_, AccountsPane>,
    account: &str,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let check = model.connection_check(account)?;
    drop(model);
    Ok(crate::device_read::job(
        cx,
        check,
        |_: &mut SettingsApp, outcome: Result<String, String>, _| complete_account_check(outcome),
    ))
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
    Ok(crate::device_read::job(
        cx,
        send,
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
    pub(crate) remote: ui::RemoteActions,
    removal: std::cell::RefCell<Option<browser_sessions::Removal>>,
    model: std::cell::OnceCell<Shared<AccountsPane>>,
    account_root: Option<AccountRoot>,
    /// The credential vault, its custody and the sync statuses, opened off
    /// the UI thread after the window's first frame ([`Self::start_read`]).
    read: crate::device_read::DeviceRead<AccountsRead>,
}

/// What the External Accounts area opens of the device: the credential
/// vault (a Secret Service round trip for its custody and key), the
/// custodian (an HTTP client for iCloud sign-in) and the accounts' sync
/// statuses. Opened off the UI thread; the person's directory, which a
/// host's record may hold, is read on the UI thread
/// ([`AccountsPane::from_read`]).
pub(crate) struct AccountsRead {
    statuses: accounts::AccountSyncStatusStore,
    vault: accounts::SecretVault,
    custodian: account_custody::Custodian,
}

impl AccountsRead {
    /// Open the stores under `root`.
    fn of(root: &AccountRoot) -> Self {
        Self {
            statuses: root.statuses(),
            vault: root.vault(),
            custodian: root.custodian(),
        }
    }

    /// Open the stores under the config root `explicit`, or the user's: the
    /// same stores as the root's, whose person's record none of them read.
    fn under(explicit: Option<std::path::PathBuf>) -> Self {
        Self::of(&explicit.map_or_else(AccountRoot::live, AccountRoot::at))
    }
}

impl AccountsArea {
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            cx: cx.clone(),
            remote: ui::RemoteActions::current(),
            removal: std::cell::RefCell::default(),
            model: std::cell::OnceCell::new(),
            account_root: None,
            read: crate::device_read::DeviceRead::default(),
        }
    }

    /// Open the vault, the custodian and the statuses off the UI thread;
    /// `false` when they have been opened or are being opened, or for a
    /// screenshot, which opens none of them.
    pub(crate) fn start_read(&self) -> bool {
        if self.is_open() || self.cx.scene().is_some() {
            return false;
        }
        let explicit = self
            .account_root
            .as_ref()
            .and_then(|root| root.explicit().map(std::path::Path::to_path_buf));
        self.read.start(move || AccountsRead::under(explicit))
    }

    /// Whether the read [`Self::start_read`] started has ended.
    pub(crate) fn read_finished(&self) -> bool {
        self.read.is_finished()
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
            let read = self.read.take();
            let mut model = if scene.is_some() {
                AccountsPane::from_dir_in(root, crate::accounts::sample_directory(), false)
            } else if read.is_some() && !root.mutation_pending() {
                let dir = AccountDirectory::load_in(&root);
                match read {
                    Some(read) => AccountsPane::from_read(root, dir, read),
                    None => AccountsPane::recovered_in(root, None),
                }
            } else {
                // Nothing was read, or an interrupted mutation is recovered
                // first, which may rewrite the vault read: the vault is
                // opened again, off the UI thread.
                AccountsPane::recovered_in(root, read.map(|read| read.custodian))
            };
            if let Some(kind) = scene.and_then(|scene| scene.account_form) {
                model = model.with_open_form_for_shot(kind);
            }
            Rc::new(std::cell::RefCell::new(model.with_host(self.cx.clone())))
        })
    }

    /// Take in finished checks and sign-in steps, if the model exists.
    pub(crate) fn poll(&self) {
        self.remote.poll();
        let finished = self
            .removal
            .borrow_mut()
            .as_mut()
            .and_then(|removal| removal.poll(&self.remote));
        if let Some(result) = finished {
            self.removal.borrow_mut().take();
            let mut model = self.model().borrow_mut();
            model.browser_removal_pending = false;
            model.browser_removal_error = result.err();
            model.changed = true;
        }
        if let Some(model) = self.model.get() {
            let mut pane = model.borrow_mut();
            // Background completion must leave the visible PaneView's rebuild
            // demand intact; that view consumes it during its own sync.
            let changed = pane.poll();
            pane.changed |= changed;
        }
    }

    /// Whether the area has read what its rows show: its pane was shown,
    /// an effect or a row lookup reached it, or the window read it after
    /// its first frame ([`crate::effects::LIVE_AREAS`]).
    pub(crate) fn is_open(&self) -> bool {
        self.model.get().is_some()
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
                if let Some(action) = model.files_handoff(&view) {
                    row = row.operation(action);
                }
                if !view.kind.is_cloud_storage() {
                    row = row.operation(offer(AccountsEffect::EditAccount {
                        account: id.clone(),
                    }));
                }
                if view.kind == AccountKind::Nextcloud {
                    row = row.operation(offer(AccountsEffect::ConnectNextcloudCalendars {
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
                } else if model.can_recover_cloud_mode(&view) {
                    row = row.operation(offer(AccountsEffect::RecoverCloudMode {
                        account: id.clone(),
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
        published.extend(model.thunderbird_import_rows());
        published.extend(model.calendar_choice_rows());
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
        for account in model.browser_service_sessions() {
            let id = account.id.as_str().to_owned();
            published.push(
                LiveRow::new(
                    format!("settings.browser-service-session:{id}"),
                    "settings.browser-service-session",
                    account.display_name,
                )
                .operation(offer(AccountsEffect::RemoveBrowserServiceSession {
                    session_id: id,
                })),
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

fn sync_detail(
    runtime: Option<&accounts::AccountSyncStatus>,
    import_only: bool,
    granted_mail: bool,
) -> String {
    if import_only && runtime.is_none_or(|status| status.message.is_none()) {
        return t("accounts-import-no-sync");
    }
    let Some(status) = runtime else {
        return t(if granted_mail {
            "accounts-mail-not-synced"
        } else {
            "accounts-sync-none"
        });
    };
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
    SubmitGoogleMail,
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

/// What takes in how a piece of account work in the lane ended, and says
/// what comes next.
type Settle = Box<dyn FnOnce(&mut AccountsPane, Result<(), String>) -> Step>;

/// What removing an account changed, to finish or roll back.
struct Removal {
    id: accounts::AccountId,
    original_store: accounts::AccountStore,
    original_statuses: accounts::AccountSyncStatusStore,
    credential_changed: bool,
    original_secret: Option<accounts::Secret>,
}

/// Why the sample's accounts offer no removal.
const SAMPLE_NOT_REMOVABLE: &str = "example accounts cannot be removed";

struct ProbeJob<T = String> {
    receiver: crate::channel::Receiver<Result<T, String>>,
    deadline: ui::time::Instant,
}

impl<T: Send + 'static> ProbeJob<T> {
    const DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

    fn new(receiver: crate::channel::Receiver<Result<T, String>>) -> Self {
        Self {
            receiver,
            deadline: ui::time::now() + Self::DEADLINE,
        }
    }

    fn poll(&self, stopped: &str) -> Option<Result<T, String>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(crate::channel::TryRecvError::Disconnected) => Some(Err(stopped.into())),
            Err(crate::channel::TryRecvError::Empty) if ui::time::now() >= self.deadline => {
                Some(Err(t("accounts-timeout")))
            }
            Err(crate::channel::TryRecvError::Empty) => None,
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent state of the pane's forms and vault work, never a parameter"
)]
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
    /// The credential vault as last opened, for where it keeps
    /// credentials; `None` until it first opened.
    vault: Option<accounts::SecretVault>,
    /// The vault being opened again on a worker; a later opening replaces
    /// an earlier one.
    vault_opening: Option<crate::device_read::Landing>,
    /// The vault work queued and running, in order.
    lane: vault_lane::VaultLane,
    /// A reload waits in the lane.
    reload_queued: bool,
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
    form_probe: Option<ProbeJob<Vec<tilo_dav::discovery::CalendarCollection>>>,
    calendar_choices: Vec<(tilo_dav::discovery::CalendarCollection, bool)>,
    /// Canonical Nextcloud metadata only; its credential stays under this ID.
    calendar_source: Option<accounts::Account>,
    // Private discovery baseline: never published into the resource tree.
    calendar_baseline: Option<(Option<accounts::Account>, Option<accounts::Secret>)>,
    form_candidate: Option<AccountDraft>,
    /// The form's draft is read against the vault or saved to it.
    form_saving: bool,
    /// Which opening of the form the work under way serves: work for a
    /// form since cancelled or replaced shows nothing in the new one.
    form_round: u64,
    reauth_probe: Option<ProbeJob>,
    reauth_candidate: Option<(accounts::AccountId, accounts::Secret)>,
    /// The sign-in-again password is read against the vault or saved to it.
    reauth_saving: bool,
    /// As `form_round`, for the sign-in-again form.
    reauth_round: u64,
    /// A running browser or Apple ID sign-in.
    sign_in: Option<cloud_signin::SignInFlow>,
    /// A completed sign-in is being saved.
    sign_in_saving: bool,
    /// As `form_round`, for the sign-in.
    sign_in_round: u64,
    custodian: account_custody::Custodian,
    opener: std::sync::Arc<dyn BrowserOpener>,
    /// The file accounts the cloud service has mounted.
    cloud: CloudMounts,
    browser_removal_error: Option<String>,
    browser_removal_pending: bool,
    import_review: import_review::ImportReview,
    changed: bool,
}

impl AccountsPane {
    /// The live pane over the person's account stores.
    #[must_use]
    pub fn new() -> Self {
        Self::in_root(AccountRoot::live())
    }

    /// The live pane over the account stores under `account_root`, its
    /// vault opened here: never on the UI thread, where the window builds
    /// its pane from what the area read off it.
    pub fn in_root(account_root: AccountRoot) -> Self {
        // Loading the directory first recovers an interrupted mutation,
        // which may rewrite the vault the read then opens.
        let dir = AccountDirectory::load_in(&account_root);
        let read = AccountsRead::of(&account_root);
        Self::from_read(account_root, dir, read)
    }

    /// The live pane over the account stores under `account_root`, showing
    /// `dir` and holding what `read` opened.
    fn from_read(account_root: AccountRoot, dir: AccountDirectory, read: AccountsRead) -> Self {
        // The live pane shows the *real* stores — empty until the user adds an
        // account. It never substitutes illustrative sample accounts: showing
        // fabricated `ada@example.com` rows would present made-up data on an
        // empty state. The deterministic `--shots`/tests use
        // `from_dir(sample_directory(), true)` for a rich still instead.
        Self {
            sync_statuses: read.statuses,
            vault: Some(read.vault),
            cloud: CloudMounts::live(),
            ..Self::over(account_root, dir, false, read.custodian)
        }
    }

    /// The live pane over the account stores under `account_root`, an
    /// interrupted mutation recovered first: the vault opens off the UI
    /// thread, and `custodian`, when one was opened, is used.
    fn recovered_in(
        account_root: AccountRoot,
        custodian: Option<account_custody::Custodian>,
    ) -> Self {
        let dir = AccountDirectory::load_in(&account_root);
        let custodian = custodian.unwrap_or_else(|| account_root.custodian());
        let mut pane = Self {
            sync_statuses: account_root.statuses(),
            cloud: CloudMounts::live(),
            ..Self::over(account_root, dir, false, custodian)
        };
        pane.reopen_vault();
        pane
    }

    /// The pane showing `dir` over `account_root`, holding nothing it read
    /// of the device: no sync statuses, no vault, the service asked for no
    /// mounts.
    fn over(
        account_root: AccountRoot,
        dir: AccountDirectory,
        is_sample: bool,
        custodian: account_custody::Custodian,
    ) -> Self {
        Self {
            dir,
            sync_statuses: accounts::AccountSyncStatusStore::default(),
            vault: None,
            vault_opening: None,
            lane: vault_lane::VaultLane::default(),
            reload_queued: false,
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
            form_saving: false,
            form_round: 0,
            reauth_probe: None,
            reauth_candidate: None,
            reauth_saving: false,
            reauth_round: 0,
            import_review: import_review::ImportReview::default(),
            sign_in: None,
            sign_in_saving: false,
            sign_in_round: 0,
            custodian,
            opener: std::sync::Arc::new(PortalOpener),
            cloud: CloudMounts::at(None),
            calendar_choices: Vec::new(),
            calendar_source: None,
            calendar_baseline: None,
            browser_removal_error: None,
            browser_removal_pending: false,
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

    /// [`Self::from_dir`] whose edits persist under `account_root`. The
    /// vault, which only says where credentials are kept here, opens off
    /// the UI thread.
    pub fn from_dir_in(account_root: AccountRoot, dir: AccountDirectory, is_sample: bool) -> Self {
        let custodian = account_root.custodian();
        let mut pane = Self::over(account_root, dir, is_sample, custodian);
        pane.reopen_vault();
        pane
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

    /// Drive the live host's nonblocking polling until the pending probe
    /// finishes: either calendar selection is ready or the account is saved.
    /// Returns false on failure; a ready calendar review is not a saved account.
    /// Bounded by the probe's production deadline; unlike a fixed number of
    /// scheduler yields this remains reliable when the test suite is busy.
    #[doc(hidden)]
    pub fn finish_form_submission_for_test(&mut self) -> bool {
        let deadline = ui::time::now() + ProbeJob::<String>::DEADLINE;
        while self.form_open() && ui::time::now() < deadline {
            let _ = self.poll();
            if !self.calendar_choices.is_empty() && !self.form_saving && self.lane.is_idle() {
                return self.form_error.is_none();
            }
            if self.form_probe.is_none()
                && self.sign_in.is_none()
                && !self.form_saving
                && !self.sign_in_saving
                && self.lane.is_idle()
            {
                return !self.form_open();
            }
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
        // Row effects are published on the existing account resource, not
        // the Settings root. Bind the painted control to that same contract.
        let account = match &effect {
            AccountsEffect::EditAccount { account }
            | AccountsEffect::SignInAgain { account }
            | AccountsEffect::TestAccount { account }
            | AccountsEffect::RemoveAccount { account }
            | AccountsEffect::SyncNow { account }
            | AccountsEffect::PauseSync { account }
            | AccountsEffect::ResumeSync { account }
            | AccountsEffect::RecoverCloudMode { account } => Some(account),
            _ => None,
        };
        if let Some(cx) = &self.cx
            && let Some(account) = account
        {
            let spec = SettingsEffect::from(effect.clone()).bound();
            let resource = ui::tilo_actions::Resource::new(
                format!("settings.account:{account}"),
                "settings.account",
                account,
            )
            .with_action(spec);
            let invocation =
                ui::tilo_resource::ActionRef::<SettingsEffect, ()>::new(effect.id(), &[]);
            return match cx.row_binding(&resource, invocation) {
                Some(binding) => button.bind_action(binding),
                None => button.unavailable(),
            };
        }
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
    /// as `sync` would, and wait for a completed sign-in to be saved.
    /// Returns whether a step reported before `timeout`.
    #[doc(hidden)]
    pub fn finish_sign_in_step_for_test(&mut self, timeout: std::time::Duration) -> bool {
        let reported = if let Some(flow) = self.sign_in.as_mut()
            && flow.busy()
        {
            let reported = flow.wait_for_test(timeout);
            self.poll_sign_in();
            reported
        } else {
            // No step runs: the sync that started it already took it in.
            true
        };
        // A completed sign-in is saved through the vault on a worker.
        self.settle_lane();
        reported
    }

    fn open_sign_in(&mut self, kind: AccountKind, existing: Option<&accounts::Account>) {
        if self.sign_in_saving {
            return;
        }
        self.form_kind = None;
        self.reauth_id = None;
        self.sign_in_round = self.sign_in_round.wrapping_add(1);
        self.sign_in_saving = false;
        let mut flow = cloud_signin::SignInFlow::new(
            kind,
            existing,
            self.custodian.clone(),
            std::sync::Arc::clone(&self.opener),
        );
        if let Some(mail_id) = existing
            .and_then(|account| {
                account
                    .config
                    .get(accounts::cloud_types::MAIL_ACCOUNT_ID_KEY)
            })
            .and_then(serde_json::Value::as_str)
        {
            let store = self.account_root.store();
            let email = store
                .get(&accounts::AccountId::from(mail_id))
                .and_then(|mail| mail.config.get("email"))
                .and_then(serde_json::Value::as_str);
            if let Some(email) = email {
                flow.expect_mailbox(Some(email.to_string()));
            }
        }
        self.sign_in = Some(flow);
        self.changed = true;
    }

    fn close_sign_in(&mut self) {
        // Keep the daemon's Import preparation alive until metadata is durable.
        if self.sign_in_saving {
            return;
        }
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
        if let Some(id) = flow.take_saved() {
            self.publish_signed_in(id);
            return;
        }
        if changed {
            self.changed = true;
        }
        let Some(completed) = completed else {
            return;
        };
        self.sign_in_saving = true;
        let round = self.sign_in_round;
        self.run_in_lane(move |pane| {
            match Self::signed_in_accounts(&pane.account_root, &completed) {
                Ok((stored, mail)) => {
                    let id = stored.id.clone();
                    let secret = completed.secret;
                    let accounts = std::iter::once(stored)
                        .chain(mail)
                        .map(|account| (account, Some(secret.clone())))
                        .collect();
                    pane.commit_then(
                        accounts,
                        Box::new(move |pane: &mut Self, result: Result<(), String>| {
                            pane.sign_in_saved(round, result.map(|()| id))
                        }),
                    )
                }
                Err(error) => pane.sign_in_saved(round, Err(error)),
            }
        });
        self.changed = true;
    }

    /// Take in how saving the sign-in of `round` ended.
    fn sign_in_saved(&mut self, round: u64, saved: Result<accounts::AccountId, String>) -> Step {
        let current = round == self.sign_in_round;
        if current {
            self.sign_in_saving = false;
        }
        match saved {
            Ok(id) => {
                if current && let Some(flow) = self.sign_in.as_mut() {
                    flow.finish_async(id);
                    // A non-mode-changing sign-in finishes immediately.
                    if let Some(id) = flow.take_saved() {
                        self.publish_signed_in(id);
                    }
                } else {
                    self.publish_signed_in(id);
                }
            }
            Err(error) => {
                if current && let Some(flow) = self.sign_in.as_mut() {
                    flow.save_failed(&error);
                }
            }
        }
        self.changed = true;
        Step::Done
    }

    fn publish_signed_in(&mut self, id: accounts::AccountId) {
        let custodian = self.custodian.clone();
        // Credential publication may renew a session over the network.
        let _ = crate::device_read::thread(
            "settings-accounts-worker",
            Box::new(move || {
                if let Err(error) = custodian.signed_in(&id) {
                    warn!(target: "tilo::settings", %error, "could not publish the first credential for a new sign-in");
                }
            }),
        );
        self.sign_in = None;
        self.queue_reload();
        self.changed = true;
    }

    /// Update the open, idle connection form's service choices without starting sign-in.
    ///
    /// Uses the same validation as the `select-account-services` action; `mode`
    /// must be `sync` or `import`.
    ///
    /// # Errors
    /// Returns an error if the mode is invalid, the form is closed or busy,
    /// or the requested services are unavailable for the account.
    pub fn select_account_services(
        &mut self,
        files: bool,
        mail: bool,
        mode: &str,
    ) -> Result<(), String> {
        let mode = serde_json::from_value(serde_json::Value::String(mode.to_string()))
            .map_err(|_| "Files mode must be sync or import".to_string())?;
        self.sign_in
            .as_ref()
            .ok_or_else(|| "Open an account connection form first".to_string())?
            .select_services(files, mail, mode)?;
        self.changed = true;
        Ok(())
    }

    fn connect_google_mail(&mut self) -> Result<(), String> {
        if !self
            .sign_in
            .as_ref()
            .is_some_and(cloud_signin::SignInFlow::can_request_google_mail)
        {
            return Err(
                "Open the Google account sign-in form before requesting Gmail access".into(),
            );
        }
        self.command(AccountViewCommand::SubmitGoogleMail);
        Ok(())
    }

    /// The accounts a completed sign-in saves, the signed-in one first: its
    /// metadata over the stored envelope it signs in again, or a new one,
    /// and the Gmail account granted with it, which shares its credential.
    fn signed_in_accounts(
        account_root: &AccountRoot,
        completed: &cloud_signin::Completed,
    ) -> Result<(accounts::Account, Option<accounts::Account>), String> {
        let type_id = completed.kind.type_id();
        let shared = account_root.store();
        let mailbox_identity = completed
            .config
            .get("email")
            .and_then(serde_json::Value::as_str);
        let existing_mail =
            if completed.kind == AccountKind::Mail && completed.existing_id.is_none() {
                mailbox_identity.and_then(|email| find_mailbox(&shared, email))
            } else {
                None
            };
        if existing_mail.is_some() {
            return Err("This mailbox is already connected. Manage its existing Mail account instead of replacing its grant".into());
        }
        let existing_id = completed
            .existing_id
            .clone()
            .or_else(|| existing_mail.map(|account| account.id.to_string()));
        let id = existing_id.clone().map_or_else(
            || {
                accounts::AccountId::from(format!(
                    "{}-{}",
                    completed.kind.slug(),
                    uuid::Uuid::new_v4()
                ))
            },
            accounts::AccountId::from,
        );
        let mut stored = match (existing_id.is_some(), shared.get(&id)) {
            (true, Some(stored)) if stored.type_id.as_str() == type_id => stored.clone(),
            (true, Some(_)) => {
                return Err("account type changed; refresh Settings and try again".into());
            }
            (true, None) => return Err("account no longer exists".into()),
            (false, Some(_)) => return Err("this account is already configured".into()),
            (false, None) => accounts::Account::new(id.clone(), type_id),
        };
        let mut config = completed.config.clone();
        if completed.kind == AccountKind::Mail && existing_id.is_some() {
            if let Some(name) = stored.config.get("display_name") {
                merge_config(&mut config, serde_json::json!({"display_name": name}));
            }
        } else {
            stored.display_name.clone_from(&completed.display_name);
        }
        merge_config(&mut stored.config, config);
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
        let mut linked_mail = completed.mail.clone();
        if let Some(mail) = linked_mail.as_mut() {
            if let Some(email) = mail.config.get("email").and_then(serde_json::Value::as_str)
                && let Some(existing) = find_mailbox(&shared, email)
            {
                let linked = completed
                    .config
                    .get(accounts::cloud_types::MAIL_ACCOUNT_ID_KEY)
                    .and_then(serde_json::Value::as_str);
                if linked != Some(existing.id.as_str()) {
                    return Err("This mailbox is already connected separately. Manage or disconnect its Mail account before linking it to Files".into());
                }
                mail.id = existing.id.clone();
            }
            merge_config(
                &mut stored.config,
                serde_json::json!({
                    accounts::cloud_types::MAIL_ACCOUNT_ID_KEY: mail.id.to_string(),
                }),
            );
        }
        let mut granted = None;
        if let Some(mail) = linked_mail {
            let mut mail = mail;

            if let Some(existing) = shared.get(&mail.id) {
                if existing.type_id.as_str() != "email" {
                    return Err("the Gmail identity belongs to another account type".into());
                }
                let mut config = mail.config.clone();
                if let Some(name) = existing.config.get("display_name") {
                    merge_config(&mut config, serde_json::json!({"display_name": name}));
                }
                mail = existing.clone();
                merge_config(&mut mail.config, config);
            }
            granted = Some(mail);
        }
        Ok((stored, granted))
    }

    /// Exercise the ordinary sign-in transaction synchronously in a test.
    #[cfg(test)]
    fn save_signed_in(
        account_root: &AccountRoot,
        completed: &cloud_signin::Completed,
    ) -> Result<accounts::AccountId, String> {
        let (stored, mail) = Self::signed_in_accounts(account_root, completed)?;
        let id = stored.id.clone();
        let accounts = std::iter::once(stored)
            .chain(mail)
            .map(|account| (account, Some(&completed.secret)))
            .collect();
        Self::commit_accounts(account_root, accounts)?;
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
            }
            self.form_user.set(
                stored
                    .config
                    .get("username")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .into(),
            );
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
        if shared
            .get(&accounts::AccountId::from(view.id.clone()))
            .is_some_and(|account| {
                account.type_id.as_str() == accounts::thunderbird::PENDING_TYPE
                    && AccountConfig::from_value(account.config.clone())
                        .is_ok_and(|config| config.auth == accounts::email::AuthMethod::OAuth)
            })
        {
            self.import_review.status = Some(t("accounts-import-oauth-step"));
            self.changed = true;
            return;
        }
        self.next_form_round();
        self.form_kind = Some(kind);
        self.form_account_id = Some(view.id);
        self.form_error = None;
        self.form_password.set(String::new());
        self.changed = true;
    }

    /// A new opening of the account form: work still under way for an
    /// earlier one shows nothing in it.
    fn next_form_round(&mut self) {
        self.form_round = self.form_round.wrapping_add(1);
        self.form_saving = false;
        self.form_probe = None;
        self.form_candidate = None;
    }

    /// Take `command`, as the pane's buttons and their effects ask.
    pub fn command(&mut self, command: AccountViewCommand) {
        if matches!(
            &command,
            AccountViewCommand::Add(_) | AccountViewCommand::Edit(_)
        ) {
            self.cancel_form();
        }
        match Some(command) {
            Some(AccountViewCommand::Add(kind)) if kind.signs_in() => {
                self.open_sign_in(kind, None);
            }
            Some(AccountViewCommand::Add(kind)) => {
                self.sign_in = None;
                self.next_form_round();
                self.form_kind = Some(kind);
                self.form_account_id = None;
                self.form_error = None;
                self.form_password.set(String::new());
                self.clear_form_fields();
                self.changed = true;
            }
            Some(AccountViewCommand::Edit(id)) => self.open_edit(&id),
            Some(AccountViewCommand::Reauthenticate(id)) => self.open_reauthentication(id),
            Some(AccountViewCommand::SubmitGoogleMail) => {
                if let Some(flow) = self.sign_in.as_mut() {
                    flow.submit_with_google_mail();
                }
                self.changed = true;
            }
            Some(AccountViewCommand::Submit) if self.sign_in.is_some() => {
                // A completed sign-in being saved is not started again.
                if !self.sign_in_saving
                    && let Some(flow) = self.sign_in.as_mut()
                {
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
        if let Some(account) = stored.as_ref()
            && account.type_id.as_str() == accounts::thunderbird::PENDING_TYPE
        {
            if AccountConfig::from_value(account.config.clone())
                .is_ok_and(|config| config.auth == accounts::email::AuthMethod::OAuth)
            {
                self.import_review.status = Some(t("accounts-import-oauth-step"));
                self.changed = true;
            } else {
                self.open_edit(&id);
            }
            return;
        }
        if let Some(account) = stored.as_ref()
            && account.type_id.as_str() == "email"
            && AccountConfig::from_value(account.config.clone()).is_ok_and(|config| {
                config.auth == accounts::email::AuthMethod::OAuth
                    && config.imap_host == "imap.gmail.com"
            })
        {
            self.open_sign_in(AccountKind::Mail, Some(account));
            return;
        }
        if let Some(account) = stored
            && let Some(kind) = AccountKind::from_type_id(account.type_id.as_str())
            && kind.signs_in()
        {
            self.open_sign_in(kind, Some(&account));
            return;
        }
        self.sign_in = None;
        self.next_reauth_round();
        self.reauth_id = Some(id);
        self.reauth_error = None;
        self.reauth_password.set(String::new());
        self.changed = true;
    }

    /// A new opening of the sign-in-again form, as [`Self::next_form_round`].
    fn next_reauth_round(&mut self) {
        self.reauth_round = self.reauth_round.wrapping_add(1);
        self.reauth_saving = false;
        self.reauth_probe = None;
        self.reauth_candidate = None;
    }

    fn clear_form_fields(&self) {
        self.form_name.set(String::new());
        self.form_email.set(String::new());
        self.form_user.set(String::new());
        self.form_server.set(String::new());
        self.form_password.set(String::new());
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let mut panel: Vec<SettingsSection> = Vec::new();

        let mut intro: Vec<Item> = vec![note(&t("accounts-intro"))];
        if self
            .vault
            .as_ref()
            .is_some_and(|vault| vault.custody() == accounts::SecretCustody::LocalFile)
        {
            intro.push(note(&t("accounts-vault-local")));
        }
        if self.is_sample {
            // Be honest that the visible rows are an illustration, not real
            // connected accounts — the store is empty until you add one.
            intro.push(note(&t("accounts-empty-sample")));
        } else if self.dir.is_empty() {
            // The live empty state: honest, no fabricated rows.
            intro.push(note(&t("accounts-empty")));
        }
        panel.push(untitled(rows(intro)));

        if let Some(flow) = &self.sign_in {
            panel.push(flow.card(&|button, effect| self.bound(button, effect)));
        } else if let Some(kind) = self.form_kind {
            panel.push(self.account_form(kind));
        }
        if let Some(id) = &self.reauth_id {
            panel.push(self.reauth_form(id));
        }
        panel.push(self.browser_service_sessions_card());
        // Connected services lead. Empty providers belong to the one chooser,
        // not to an inventory of accounts the person has not connected.
        for kind in AccountKind::ALL {
            if kind == AccountKind::Mail {
                if let Some(mail) = self.mail_card() {
                    panel.push(mail);
                }
            } else if kind == AccountKind::GoogleDrive {
                for view in self.dir.views_of(kind) {
                    let title = kind_shown(kind);
                    panel.push(card(&title, self.kind_body(kind, &[view])));
                }
            } else if !self.dir.views_of(kind).is_empty() {
                panel.push(self.kind_card(kind));
            }
        }
        panel.push(self.add_account_card());
        panel.push(self.thunderbird_import_card());
        if !self
            .account_root
            .store()
            .list(&accounts::person::TILO_IDENTITY_TYPE.into())
            .is_empty()
        {
            panel.push(self.browser_identity_card());
        }

        crate::widgets::cards(panel)
    }

    fn browser_service_sessions(&self) -> Vec<accounts::Account> {
        // These are device-bound selections, not public identity-picker records.
        if self.is_sample {
            return Vec::new();
        }
        self.account_root
            .path()
            .map(|root| {
                accounts::AccountStore::load_in(&root)
                    .list(&browser_sessions::TYPE.into())
                    .into_iter()
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn browser_service_sessions_card(&self) -> SettingsSection {
        let mut body = vec![note(&t("accounts-browser-sessions-about"))];
        if self.browser_removal_pending {
            body.push(note(&t("accounts-browser-sessions-removing")));
        }
        if let Some(error) = &self.browser_removal_error {
            body.push(note(error));
        }
        for account in self.browser_service_sessions() {
            let id = account.id.as_str();
            body.push(setting_row(
                &account.display_name,
                Box::new(
                    self.bound(
                        Button::new(
                            &format!("settings.browser-session.remove:{id}"),
                            &t("accounts-browser-sessions-remove"),
                        )
                        .with_enabled(!self.browser_removal_pending),
                        AccountsEffect::RemoveBrowserServiceSession {
                            session_id: id.to_owned(),
                        },
                    ),
                ),
            ));
        }
        card(&t("accounts-browser-sessions-title"), rows(body))
    }

    fn browser_identity_card(&self) -> SettingsSection {
        let store = self.account_root.store();
        let mut body = vec![note(&t("accounts-browser-identity-about"))];
        for account in store.list(&accounts::person::TILO_IDENTITY_TYPE.into()) {
            let id = account.id.as_str().to_owned();
            let toggle = if account.enabled {
                "accounts-browser-identity-hide"
            } else {
                "accounts-browser-identity-show"
            };
            let toggle = self.bound(
                Button::new(
                    &format!("settings.browser-identity.toggle:{id}"),
                    &t(toggle),
                ),
                AccountsEffect::SetBrowserIdentityEnabled {
                    account: id.clone(),
                    enabled: !account.enabled,
                },
            );
            let remove = self.bound(
                Button::new(
                    &format!("settings.browser-identity.remove:{id}"),
                    &t("accounts-browser-identity-remove"),
                ),
                AccountsEffect::RemoveBrowserIdentity { account: id },
            );
            let actions: El = Box::new(
                Row::new()
                    .no_wrap()
                    .with_rhythm(ui::tokens::Rhythm::Related)
                    .push(Box::new(toggle))
                    .push(Box::new(remove)),
            );
            let origin = account
                .config
                .get("origin")
                .and_then(serde_json::Value::as_str);
            body.push(match origin {
                Some(origin) => setting_row_described(&account.display_name, origin, actions),
                None => setting_row(&account.display_name, actions),
            });
        }
        card(&t("accounts-browser-identity-title"), rows(body))
    }

    fn add_account_card(&self) -> SettingsSection {
        let mut providers = Row::new().with_rhythm(ui::tokens::Rhythm::Related);
        for kind in AccountKind::ALL {
            providers = providers.push(Box::new(self.bound(
                Button::new(
                    &format!("settings.accounts.add.{}", kind.slug()),
                    &t_args(
                        "accounts-add-provider",
                        &[("kind", kind_shown(kind).into())],
                    ),
                ),
                AccountsEffect::AddAccount {
                    kind: kind.slug().into(),
                },
            )));
        }
        card(&t("accounts-add"), rows(vec![row(Box::new(providers))]))
    }

    fn mail_card(&self) -> Option<SettingsSection> {
        let title = kind_shown(AccountKind::Mail);
        let all = self.dir.views_of(AccountKind::Mail);
        let views: Vec<_> = all
            .iter()
            .filter(|mail| {
                !self
                    .dir
                    .views_of(AccountKind::GoogleDrive)
                    .iter()
                    .any(|google| {
                        self.linked_google_mail(google)
                            .is_some_and(|linked| linked.id == mail.id)
                    })
            })
            .cloned()
            .collect();
        (!views.is_empty()).then(|| card(&title, self.kind_body(AccountKind::Mail, &views)))
    }

    /// Group only the explicit persisted service link, never an email/name match.
    fn linked_google_mail(&self, view: &AccountView) -> Option<AccountView> {
        if view.kind != AccountKind::GoogleDrive {
            return None;
        }
        let store = self.account_root.store();
        let account = store.get(&view.id.clone().into())?;
        let id = account
            .config
            .get(accounts::cloud_types::MAIL_ACCOUNT_ID_KEY)?
            .as_str()?;
        self.dir
            .views_of(AccountKind::Mail)
            .into_iter()
            .find(|mail| mail.id == id)
    }

    fn kind_card(&self, kind: AccountKind) -> SettingsSection {
        let views = self.dir.views_of(kind);
        card(&kind_shown(kind), self.kind_body(kind, &views))
    }

    fn kind_body(&self, kind: AccountKind, views: &[AccountView]) -> Body {
        let mut body: Vec<Item> = if views.is_empty() && kind == AccountKind::GoogleDrive {
            vec![note(&t("accounts-google-empty-services"))]
        } else if views.is_empty() {
            vec![note(&t_args(
                "accounts-kind-empty",
                &[("kind", kind_shown(kind).into())],
            ))]
        } else {
            views
                .iter()
                .flat_map(|view| {
                    let mut items = vec![self.account_row(view)];
                    if let Some(mail) = self.linked_google_mail(view) {
                        items.push(self.account_row(&mail));
                    }
                    items
                })
                .collect()
        };
        if kind.is_cloud_storage()
            && let Some(reason) = cloud_signin::unavailable_reason(kind, &self.custodian)
        {
            body.push(note(&reason));
        } else if kind.best_effort() {
            body.push(note(&t("accounts-best-effort-icloud")));
        }
        rows(body)
    }

    /// One broker-routed contract shared by the painted button and account row.
    fn files_handoff(&self, view: &AccountView) -> Option<ui::tilo_actions::ActionSpec> {
        use accounts::cloud_types::FilesMode;
        if self.is_sample
            || !view.enabled
            || !(view.kind.is_cloud_storage()
                || matches!(view.kind, AccountKind::WebDav | AccountKind::Nextcloud))
        {
            return None;
        }
        let account = self
            .account_root
            .store()
            .get(&view.id.clone().into())?
            .clone();
        if account
            .config
            .get("use_files")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        {
            return None;
        }
        let label = match accounts::cloud_types::files_mode(&account)? {
            FilesMode::Sync => t("accounts-browse-files"),
            FilesMode::Import => t("accounts-choose-import-files"),
        };
        Some(
            ui::tilo_actions::remote::ApplicationLaunch::with_action(
                "so.tilo.file-explorer",
                format!("files.remote-account:{}", hex::encode(&view.id)),
                "open-remote-account",
            )
            .spec("open-files", &label, &t("accounts-open-files-description")),
        )
    }

    fn files_button(&self, view: &AccountView, details: &mut Vec<String>) -> Option<Button> {
        let spec = self.files_handoff(view)?;
        let label = spec.label.clone();
        let resource = ui::tilo_actions::Resource::new(
            format!("settings.account:{}", view.id),
            "settings.account",
            &view.label,
        )
        .with_action(spec);
        let binding = ui::tilo_actions::ActionBinding::unit_for(&resource, "open-files").ok()?;
        let store = self.account_root.store();
        if let Some(account) = store.get(&view.id.clone().into()) {
            details.push(t(match accounts::cloud_types::files_mode(account) {
                Some(accounts::cloud_types::FilesMode::Import) => {
                    "accounts-files-configured-import"
                }
                _ => "accounts-files-configured-sync",
            }));
        }
        Some(
            Button::new(&format!("settings.accounts.files.{}", view.id), &label)
                .role(ui::widget::ButtonRole::Primary)
                .on_action(binding),
        )
    }

    /// One account: label + connection summary, status, and a Remove button
    /// (only for real accounts — sample rows are illustrative).
    ///
    /// Sample rows report an "Example" status; they do not represent a live
    /// connection.
    fn account_row(&self, view: &AccountView) -> Item {
        let id = accounts::AccountId::from(view.id.clone());
        let runtime = self.sync_statuses.get(&id);
        let mount = self.cloud.mount(&view.id);
        let pending = self
            .dir
            .mail
            .accounts
            .iter()
            .any(|account| account.id == view.id && account.config.import_pending);
        let import_only = self.account_root.store().get(&id).is_some_and(|account| {
            accounts::cloud_types::files_mode(account)
                == Some(accounts::cloud_types::FilesMode::Import)
                && view.kind != AccountKind::Mail
        });
        // An OAuth grant is not a successful first sync. Only replace the
        // unknown state, never a recorded error, offline or sign-in requirement.
        let not_synced =
            runtime.is_none_or(|status| status.health == accounts::AccountSyncHealth::Never);
        let granted_mail =
            view.kind == AccountKind::Mail && self.custodian.oauth_scopes(&id).is_ok();
        let (status, status_tone) =
            if !self.is_sample && view.enabled && !pending && not_synced && import_only {
                (t("accounts-status-import-only"), StatusTone::Neutral)
            } else if !self.is_sample && view.enabled && !pending && not_synced && granted_mail {
                (t("accounts-status-mail-granted"), StatusTone::Neutral)
            } else if pending {
                (t("accounts-status-reauth"), StatusTone::Warning)
            } else if mount.is_some_and(|mount| mount.paused) {
                (t("accounts-status-paused"), StatusTone::Neutral)
            } else {
                account_status(
                    self.is_sample,
                    view.enabled,
                    runtime.map(|status| status.health),
                )
            };

        // Identity and health lead; what is worth knowing hugs the name and
        // the maintenance actions sit under the text, so narrow layouts stay
        // legible and touch targets never crush the connection summary.
        let mut marks: Vec<El> = Vec::new();
        if view.kind.best_effort() {
            marks.push(semantic_status_pill(
                &t("accounts-best-effort"),
                StatusTone::Neutral,
            ));
        }
        marks.push(semantic_status_pill(&status, status_tone));

        let mut details = vec![view.summary.clone()];
        let mut actions: Vec<Button> = self.files_button(view, &mut details).into_iter().collect();
        if !self.is_sample {
            details.push(sync_detail(runtime, import_only, granted_mail));
            actions.extend(self.account_maintenance_actions(
                view,
                runtime.is_some_and(|status| {
                    status.health == accounts::AccountSyncHealth::NeedsAuthentication
                }),
            ));
            if view.kind == AccountKind::Nextcloud {
                actions.push(self.bound(
                    Button::new(
                        &format!("settings.accounts.calendars.{}", view.id),
                        &t("accounts-nextcloud-calendars"),
                    ),
                    AccountsEffect::ConnectNextcloudCalendars {
                        account: view.id.clone(),
                    },
                ));
            }
            if let Some(mount) = mount {
                actions.extend(self.sync_actions(&view.id, mount.paused));
            } else if self.can_recover_cloud_mode(view) {
                details.push(t("accounts-recover-mode-description"));
                actions.push(self.bound(
                    Button::new(
                        &format!("settings.accounts.recover-mode.{}", view.id),
                        &t("accounts-recover-mode"),
                    ),
                    AccountsEffect::RecoverCloudMode {
                        account: view.id.clone(),
                    },
                ));
            }
        }
        let label = self.account_row_label(view, &mut details);
        item_row(&label, marks, &details, actions)
    }

    fn account_row_label(&self, view: &AccountView, details: &mut Vec<String>) -> String {
        // Keep the service name on the primary line and the account identity
        // on a secondary line, rather than joining metadata into the label.
        // The configured service identifies Gmail, never an email-domain guess.
        let service = if view.kind == AccountKind::GoogleDrive {
            Some(t("accounts-google-files-service"))
        } else if view.kind == AccountKind::Mail
            && self.dir.mail.accounts.iter().any(|mail| {
                mail.id == view.id
                    && mail.config.auth == accounts::email::AuthMethod::OAuth
                    && mail.config.imap_host == "imap.gmail.com"
            })
        {
            Some(t("accounts-google-mail-service"))
        } else {
            None
        };
        if let Some(service) = service {
            if view.label != view.summary
                && view.label != "Google"
                && view.label != AccountKind::GoogleDrive.label()
            {
                details.insert(0, view.label.clone());
            }
            service
        } else {
            view.label.clone()
        }
    }

    fn account_maintenance_actions(&self, view: &AccountView, needs_sign_in: bool) -> Vec<Button> {
        // Shared MenuItem currently accepts view callbacks, not ActionBinding.
        // Keep these native, service-labelled action controls rather than route
        // maintenance through a private menu callback that bypasses admission.
        let mut actions = Vec::new();
        let service_label = |action: String| {
            let service = if view.kind == AccountKind::Mail {
                Some(t("accounts-service-mail"))
            } else if view.kind.is_cloud_storage()
                || matches!(view.kind, AccountKind::WebDav | AccountKind::Nextcloud)
            {
                Some(t("accounts-service-files"))
            } else {
                None
            };
            service.map_or_else(
                || action.clone(),
                |service| {
                    t_args(
                        "accounts-service-action",
                        &[
                            ("action", action.clone().into()),
                            ("service", service.into()),
                        ],
                    )
                },
            )
        };
        let reauth_label = if view.kind == AccountKind::ICloudDrive && needs_sign_in {
            t("accounts-reauth-icloud")
        } else {
            t("accounts-reauth")
        };
        let reauth_label = service_label(reauth_label);
        let edit = service_label(t("accounts-edit"));
        let check = if view.kind == AccountKind::Mail {
            t("accounts-check-server")
        } else {
            t("accounts-verify")
        };
        let check = service_label(check);
        let remove = t(if view.kind == AccountKind::Mail {
            "accounts-disconnect-mail"
        } else if view.kind.is_cloud_storage()
            || matches!(view.kind, AccountKind::WebDav | AccountKind::Nextcloud)
        {
            "accounts-remove-files"
        } else {
            "accounts-remove"
        });
        if !view.kind.is_cloud_storage() {
            actions.push(self.bound(
                Button::new(&format!("settings.accounts.edit.{}", view.id), &edit),
                AccountsEffect::EditAccount {
                    account: view.id.clone(),
                },
            ));
        }
        actions.push(self.bound(
            Button::new(
                &format!("settings.accounts.reauth.{}", view.id),
                &reauth_label,
            ),
            AccountsEffect::SignInAgain {
                account: view.id.clone(),
            },
        ));
        actions.push(self.bound(
            Button::new(&format!("settings.accounts.test.{}", view.id), &check),
            AccountsEffect::TestAccount {
                account: view.id.clone(),
            },
        ));
        actions.push(
            self.bound(
                Button::new(&format!("settings.accounts.remove.{}", view.id), &remove)
                    .role(ui::widget::ButtonRole::Destructive),
                AccountsEffect::RemoveAccount {
                    account: view.id.clone(),
                },
            ),
        );
        actions
    }

    fn can_recover_cloud_mode(&self, view: &AccountView) -> bool {
        (view.kind.is_cloud_storage()
            || matches!(view.kind, AccountKind::Nextcloud | AccountKind::WebDav))
            && self.cloud.mount(&view.id).is_none()
            && self
                .sync_statuses
                .get(&accounts::AccountId::from(view.id.as_str()))
                .is_some_and(|status| status.health == accounts::AccountSyncHealth::Error)
    }

    /// Sync now, and pause or resume, for a mounted file account.
    fn sync_actions(&self, id: &str, paused: bool) -> [Button; 2] {
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
        [
            self.bound(
                Button::new(
                    &format!("settings.accounts.sync-now.{id}"),
                    &t("accounts-sync-now"),
                ),
                AccountsEffect::SyncNow {
                    account: id.to_owned(),
                },
            ),
            self.bound(Button::new(&toggle_id, &toggle_label), toggle),
        ]
    }

    /// The inline add-email form: fields bound to CRDT keys plus Save/Cancel.
    #[allow(
        clippy::too_many_lines,
        reason = "the form declares every credential field and accessibility label in one reviewable surface"
    )]
    fn account_form(&self, kind: AccountKind) -> SettingsSection {
        if !self.calendar_choices.is_empty() || self.calendar_source.is_some() {
            return self.calendar_selection_form();
        }
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
        let save = t(if kind == AccountKind::CalDav {
            "accounts-calendar-discover"
        } else {
            "accounts-save"
        });
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
        let buttons = row(Box::new(buttons));
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
                    field_row(&display_name, Box::new(name)),
                    field_row(&identity_name, Box::new(identity)),
                ];
                if kind == AccountKind::Mail {
                    fields.push(field_row(&login_name, Box::new(user)));
                } else {
                    fields.push(field_row(&server_label, Box::new(server)));
                }
                if kind != AccountKind::Nextcloud {
                    fields.push(field_row(&password_label, Box::new(password)));
                }
                if let Some(error) = &self.form_error {
                    fields.push(if self.form_probe.is_some() {
                        note(error)
                    } else {
                        note(&t_args(
                            "accounts-save-error",
                            &[("error", error.as_str().into())],
                        ))
                    });
                }
                fields.push(note(&t("accounts-form-caption")));
                fields.push(buttons);
                fields
            }),
        )
    }

    fn reauth_form(&self, id: &str) -> SettingsSection {
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
            field_row(&t("accounts-reauth-credential"), Box::new(password)),
            note(&t("accounts-reauth-caption")),
        ];
        if let Some(error) = &self.reauth_error {
            fields.push(note(&t_args(
                "accounts-reauth-error",
                &[("error", error.as_str().into())],
            )));
        }
        fields.push(row(Box::new(
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
        )));
        card(&t("accounts-reauth-title"), rows(fields))
    }

    /// Reload the real directory after a mutation, in turn with the vault
    /// work queued before it. The live pane never substitutes the sample,
    /// so `is_sample` stays as constructed (always `false` for the live
    /// pane).
    fn queue_reload(&mut self) {
        if std::mem::replace(&mut self.reload_queued, true) {
            return;
        }
        self.run_in_lane(|pane| {
            pane.reload_queued = false;
            pane.reload();
            Step::Done
        });
    }

    /// Reload the directory and the sync statuses here, and the vault on a
    /// worker. Loading the directory recovers an interrupted mutation, so
    /// this runs only in the lane, never while a mutation is under way.
    fn reload(&mut self) {
        self.dir = AccountDirectory::load_in(&self.account_root);
        self.sync_statuses = self.account_root.statuses();
        self.changed = true;
        self.reopen_vault();
    }

    /// Open the vault again on a worker. What the pane shows of it, where
    /// it keeps credentials, changes nothing, so this waits for no other
    /// vault work; [`Self::poll`] takes it in.
    fn reopen_vault(&mut self) {
        let vault = self.account_root.vault_opener();
        self.vault_opening = Some(crate::device_read::Landing::start(move || vault.open()));
    }

    /// Take in the vault a worker opened again.
    fn poll_vault_opening(&mut self) {
        let Some(opening) = &self.vault_opening else {
            return;
        };
        match opening.poll::<accounts::SecretVault>() {
            crate::device_read::Landed::Done(vault) => {
                self.vault = Some(vault);
                self.vault_opening = None;
                self.changed = true;
            }
            crate::device_read::Landed::Pending => {}
            crate::device_read::Landed::Lost => self.vault_opening = None,
        }
    }

    fn account_type(kind: AccountKind) -> &'static str {
        kind.type_id()
    }

    /// Build the exact non-secret metadata that will be checked and, only
    /// after that check succeeds, committed; its credential follows from
    /// the vault ([`Self::prepare_secret`]). Existing envelopes are cloned
    /// so fields contributed by another app or a newer provider survive an
    /// edit.
    #[allow(
        clippy::too_many_lines,
        reason = "one canonical conversion validates the complete set of account kinds and credential fields"
    )]
    fn prepare_metadata(
        account_root: &AccountRoot,
        draft: &AccountDraft,
    ) -> Result<accounts::Account, String> {
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
        let server = if kind == AccountKind::CalDav {
            draft.server.trim()
        } else {
            draft.server.trim().trim_end_matches('/')
        };
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
            (Some(_), Some(stored))
                if stored.type_id.as_str() == expected_type
                    || (kind == AccountKind::Mail
                        && stored.type_id.as_str() == accounts::thunderbird::PENDING_TYPE) =>
            {
                stored.clone()
            }
            (Some(_), Some(_)) => {
                return Err("account type changed; refresh Settings and try again".into());
            }
            (Some(_), None) => return Err("account no longer exists".into()),
            (None, Some(_)) => return Err("this account is already configured".into()),
            (None, None) => accounts::Account::new(id.clone(), expected_type),
        };
        if stored.type_id.as_str() == accounts::thunderbird::PENDING_TYPE {
            let config = AccountConfig::from_value(stored.config.clone())
                .map_err(|_| t("accounts-import-malformed"))?;
            if config.auth == accounts::email::AuthMethod::OAuth {
                return Err(t("accounts-import-oauth-step"));
            }
            if password.trim().is_empty() {
                return Err(t("accounts-import-password-step"));
            }
            stored.type_id = "email".into();
            merge_config(
                &mut stored.config,
                serde_json::json!({"import_pending": false}),
            );
        }
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
                    config
                        .to_value()
                        .map_err(|_| "could not prepare email settings")?,
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
        Ok(stored)
    }

    /// The credential account `id` is checked and saved with: the one
    /// `vault` holds, its password replaced by a typed `password`.
    fn prepare_secret(
        vault: &accounts::SecretVault,
        id: &accounts::AccountId,
        password: &str,
    ) -> Result<accounts::Secret, String> {
        let mut secret = vault.get(id).unwrap_or_default();
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
        Ok(secret)
    }

    /// Commit a draft after its protocol probe succeeds: rebuilt over the
    /// latest canonical envelope to preserve concurrent/newer fields, its
    /// credential read from the vault on a worker, then journalled and
    /// saved ([`Self::commit_then`]). A typed `WebDAV` or Nextcloud
    /// password reaches the storage daemon before `settle` takes how it
    /// ended. Runs in the lane.
    fn save_draft_then(&mut self, draft: AccountDraft, settle: Settle) -> Step {
        let stored = match Self::prepare_metadata(&self.account_root, &draft) {
            Ok(stored) => stored,
            Err(error) => return settle(self, Err(error)),
        };
        let id = stored.id.clone();
        let password = draft.password.clone();
        Step::vault(
            move |vault| Self::prepare_secret(&vault.open(), &id, &password),
            move |pane, secret| {
                let secret = match secret {
                    Ok(secret) => secret,
                    Err(error) => return settle(pane, Err(error)),
                };
                let typed = !draft.password.trim().is_empty();
                let publishes =
                    typed && matches!(draft.kind, AccountKind::WebDav | AccountKind::Nextcloud);
                let id = stored.id.clone();
                let after: Settle = Box::new(
                    move |pane: &mut Self, committed: Result<(), String>| {
                        if committed.is_err() || !publishes {
                            return settle(pane, committed);
                        }
                        // Replace the daemon's Basic credential right away rather
                        // than after its next rejected request.
                        Step::vault(
                            move |vault| {
                                if let Err(error) = vault.custodian().publish(&id, false) {
                                    warn!(target: "tilo::settings", %error, "could not publish the updated credential");
                                }
                            },
                            move |pane, ()| settle(pane, Ok(())),
                        )
                    },
                );
                pane.commit_then(vec![(stored, typed.then_some(secret))], after)
            },
        )
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
        let draft = AccountDraft {
            kind,
            existing_id: existing_id.map(str::to_string),
            assigned_id: None,
            name: name.to_string(),
            identity: identity.to_string(),
            username: username.to_string(),
            server: server.to_string(),
            password: password.to_string(),
        };
        let mut pane = Self::from_dir_in(account_root.clone(), AccountDirectory::default(), false);
        let outcome = Rc::new(std::cell::RefCell::new(None));
        let landed = Rc::clone(&outcome);
        pane.run_in_lane(move |pane| {
            pane.save_draft_then(
                draft,
                Box::new(move |_: &mut Self, result: Result<(), String>| {
                    *landed.borrow_mut() = Some(result);
                    Step::Done
                }),
            )
        });
        pane.settle_lane();
        outcome
            .take()
            .unwrap_or_else(|| Err("the save stopped unexpectedly".into()))
    }

    /// Journal and persist `accounts` with their credentials (where given)
    /// in one mutation: the vault first, on a worker, so a metadata failure
    /// never leaves an account whose credential is missing. `settle` takes
    /// how it ended.
    fn commit_then(
        &mut self,
        accounts: Vec<(accounts::Account, Option<accounts::Secret>)>,
        settle: Settle,
    ) -> Step {
        let root = &self.account_root;
        if let Err(error) = root.begin_mutation(&root.store(), &root.statuses()) {
            return settle(self, Err(error));
        }
        let (stored, secrets): (Vec<_>, Vec<_>) = accounts
            .into_iter()
            .map(|(account, secret)| {
                let id = account.id.clone();
                (account, secret.map(|secret| (id, secret)))
            })
            .unzip();
        let secrets: Vec<(accounts::AccountId, accounts::Secret)> =
            secrets.into_iter().flatten().collect();
        // Persist credential custody first. If metadata then fails, the only
        // residue is an unreachable encrypted vault entry, never an account
        // that applications try to use without its credential.
        if secrets.is_empty() {
            return self.commit_metadata(stored, Vec::new(), settle);
        }
        Step::vault(
            move |vault| {
                let mut vault = vault.open();
                let previous: Vec<_> = secrets
                    .iter()
                    .map(|(id, secret)| {
                        let previous = vault.get(id);
                        vault.put(id, secret);
                        (id.clone(), previous)
                    })
                    .collect();
                vault.try_save().map(|()| previous)
            },
            move |pane, sealed| match sealed {
                Ok(previous) => pane.commit_metadata(stored, previous, settle),
                Err(error) => {
                    let _ = pane.account_root.finish_mutation();
                    settle(pane, Err(format!("could not save credential: {error}")))
                }
            },
        )
    }

    /// The metadata half of [`Self::commit_then`]; `previous` is what the
    /// vault held for each account whose credential was written.
    fn commit_metadata(
        &mut self,
        stored: Vec<accounts::Account>,
        previous: PreviousSecrets,
        settle: Settle,
    ) -> Step {
        // Over the accounts as they are now: an edit that reached the
        // person's record while the vault was written is kept.
        let before = self.account_root.store();
        let mut shared = before.clone();
        for account in stored {
            shared.upsert(account);
        }
        let Err(error) = self.account_root.save_store(&shared) else {
            let finished = finish_or_recover(&self.account_root, "account save");
            return settle(self, finished);
        };
        // A live record publishes each edit before projecting it to disk:
        // put its metadata back too, and keep the journal if that fails.
        let metadata_rollback = self.account_root.save_store(&before);
        warn!(target: "tilo::settings", op = "account_store_save", %error, "failed to persist account metadata");
        let failed = format!("could not persist account: {error}");
        // Once the credentials are back as they were (or that failed),
        // the mutation ends; the journal stays when a rollback failed, so
        // the next load rolls the rest back.
        let rolled_back = move |pane: &mut Self, credentials: Result<(), String>| {
            if let Err(rollback) = credentials {
                return settle(
                    pane,
                    Err(format!(
                        "{failed}; credential rollback also failed: {rollback}"
                    )),
                );
            }
            if let Err(rollback) = metadata_rollback {
                return settle(
                    pane,
                    Err(format!(
                        "{failed}; metadata rollback also failed: {rollback}"
                    )),
                );
            }
            let _ = pane.account_root.finish_mutation();
            settle(pane, Err(failed))
        };
        if previous.is_empty() {
            return rolled_back(self, Ok(()));
        }
        Step::vault(
            move |vault| {
                let mut vault = vault.open();
                for (id, secret) in &previous {
                    match secret {
                        Some(secret) => vault.put(id, secret),
                        None => {
                            vault.remove(id);
                        }
                    }
                }
                vault.try_save()
            },
            rolled_back,
        )
    }

    /// Commit `accounts` with their credentials as a sign-in does, waiting
    /// for the vault work: for a test, off the UI thread.
    #[cfg(test)]
    fn commit_accounts(
        account_root: &AccountRoot,
        accounts: Vec<(accounts::Account, Option<&accounts::Secret>)>,
    ) -> Result<(), String> {
        let accounts = accounts
            .into_iter()
            .map(|(account, secret)| (account, secret.cloned()))
            .collect();
        let mut pane = Self::from_dir_in(account_root.clone(), AccountDirectory::default(), false);
        let outcome = Rc::new(std::cell::RefCell::new(None));
        let landed = Rc::clone(&outcome);
        pane.run_in_lane(move |pane| {
            pane.commit_then(
                accounts,
                Box::new(move |_: &mut Self, result: Result<(), String>| {
                    *landed.borrow_mut() = Some(result);
                    Step::Done
                }),
            )
        });
        pane.settle_lane();
        outcome
            .take()
            .unwrap_or_else(|| Err("the save stopped unexpectedly".into()))
    }

    /// Remove the account `id`, its credential and its sync record, in turn
    /// with the vault work under way; `done` takes how it ended.
    pub(crate) fn remove_then(
        &mut self,
        id: &str,
        done: impl FnOnce(Result<(), String>) + 'static,
    ) {
        if self.is_sample {
            done(Err(SAMPLE_NOT_REMOVABLE.into()));
            return;
        }
        let id = accounts::AccountId::from(id.to_owned());
        self.run_in_lane(move |pane| {
            pane.removal(
                id,
                Box::new(move |pane: &mut Self, result: Result<(), String>| {
                    if result.is_ok() {
                        pane.queue_reload();
                        pane.changed = true;
                    }
                    done(result);
                    Step::Done
                }),
            )
        });
    }

    /// Journal the removal of `account_id`, then drop its credential on a
    /// worker. Runs in the lane.
    fn removal(&mut self, account_id: accounts::AccountId, settle: Settle) -> Step {
        let root = &self.account_root;
        let original_store = root.store();
        let Some(stored) = original_store.get(&account_id) else {
            return settle(self, Err(t("accounts-gone")));
        };
        if AccountKind::from_type_id(stored.type_id.as_str()).is_none() {
            return settle(self, Err("unsupported account type".into()));
        }
        let original_statuses = root.statuses();
        if let Err(error) = root.begin_mutation(&original_store, &original_statuses) {
            return settle(self, Err(error));
        }
        // Credential first prevents a metadata-only account from becoming
        // visible if vault persistence fails. Every later failure restores all
        // earlier writes; compensation failures are appended to the result.
        let id = account_id.clone();
        Step::vault(
            move |vault| {
                let mut vault = vault.open();
                let original = vault.get(&id);
                let changed = vault.remove(&id);
                if changed && let Err(error) = vault.try_save() {
                    return Err(error);
                }
                Ok((changed, original))
            },
            move |pane, removed| match removed {
                Ok((credential_changed, original_secret)) => pane.removal_metadata(
                    Removal {
                        id: account_id,
                        original_store,
                        original_statuses,
                        credential_changed,
                        original_secret,
                    },
                    settle,
                ),
                Err(error) => {
                    let _ = pane.account_root.finish_mutation();
                    settle(
                        pane,
                        Err(format!("could not remove saved credential: {error}")),
                    )
                }
            },
        )
    }

    /// The metadata half of [`Self::removal`], once the credential is gone.
    fn removal_metadata(&mut self, removal: Removal, settle: Settle) -> Step {
        let root = &self.account_root;
        // Over the accounts as they are now: an edit that reached the
        // person's record while the vault was written is kept.
        let mut updated_store = root.store();
        updated_store.remove(&removal.id);
        // Removing Mail is an explicit disconnect, not an implicit grant rewrite.
        let linked: Vec<_> = updated_store
            .all()
            .iter()
            .filter(|account| {
                account
                    .config
                    .get(accounts::cloud_types::MAIL_ACCOUNT_ID_KEY)
                    .and_then(serde_json::Value::as_str)
                    == Some(removal.id.as_str())
            })
            .cloned()
            .collect();
        for mut account in linked {
            if let Some(config) = account.config.as_object_mut() {
                config.remove(accounts::cloud_types::MAIL_ACCOUNT_ID_KEY);
            }
            updated_store.upsert(account);
        }
        if let Err(error) = root.save_store(&updated_store) {
            let failed = format!("could not remove account metadata: {error}");
            return self.roll_back_removal(removal, failed, Ok(()), settle);
        }
        let mut updated_statuses = removal.original_statuses.clone();
        updated_statuses.remove(&removal.id);
        if let Err(error) = root.save_statuses(&updated_statuses) {
            let metadata_rollback = root
                .save_store(&removal.original_store)
                .map_err(|e| e.to_string());
            let failed = format!("could not remove account sync status: {error}");
            return self.roll_back_removal(removal, failed, metadata_rollback, settle);
        }
        if let Err(error) = finish_or_recover(root, "account removal") {
            return settle(self, Err(error));
        }
        // Give the grant back to the provider and withdraw the daemon's
        // handoff credential. Best effort, off the UI thread: the account is
        // already gone locally.
        if let (Some(account), Some(secret)) = (
            removal.original_store.get(&removal.id),
            removal.original_secret,
        ) && account_custody::is_cloud_type(account.type_id.as_str())
        {
            let account = account.clone();
            let custodian = self.custodian.clone();
            let _ = crate::device_read::thread(
                "settings-accounts-worker",
                Box::new(move || {
                    custodian.revoke(&account, &secret);
                }),
            );
        }
        settle(self, Ok(()))
    }

    /// Restore the credential a failed removal dropped (on a worker), then
    /// fail with `failed` and whatever could not be restored. The journal
    /// is cleared only when everything was.
    fn roll_back_removal(
        &mut self,
        removal: Removal,
        failed: String,
        metadata_rollback: Result<(), String>,
        settle: Settle,
    ) -> Step {
        let finish = move |pane: &mut Self, secret_rollback: Result<(), String>| {
            let rollback = combine_rollbacks(metadata_rollback, secret_rollback);
            let rolled_back = rollback.is_ok();
            let result = with_rollback_error(failed, rollback);
            if rolled_back {
                let _ = pane.account_root.finish_mutation();
            }
            settle(pane, Err(result))
        };
        if !removal.credential_changed {
            return finish(self, Ok(()));
        }
        let Removal {
            id,
            original_secret,
            ..
        } = removal;
        Step::vault(
            move |vault| restore_secret(vault.open(), &id, original_secret.as_ref()),
            finish,
        )
    }

    fn cancel_form(&mut self) {
        {
            if self.sign_in.is_some() {
                self.close_sign_in();
                return;
            }
            if self.form_kind.is_some() {
                self.next_form_round();
                self.form_kind = None;
                self.form_error = None;
                self.form_account_id = None;
                self.form_probe = None;
                self.form_candidate = None;
                self.calendar_choices.clear();
                self.calendar_source = None;
                self.calendar_baseline = None;
                self.form_password.set(String::new());
                self.clear_form_fields();
                self.changed = true;
            }
        }
    }

    /// Save checks the draft, reads its credential from the vault on a
    /// worker, then starts a bounded protocol probe against the
    /// device-local draft. No metadata or credential is persisted until
    /// [`Self::poll_form_probe`] receives success.
    fn submit_form(&mut self) {
        {
            if let Some(flow) = self.sign_in.as_mut() {
                flow.submit();
                self.changed = true;
                return;
            }
            if !self.calendar_choices.is_empty() {
                self.save_calendar_selection();
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
            if self.form_probe.is_some() || self.form_saving {
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
            let account = match Self::prepare_metadata(&self.account_root, &draft) {
                Ok(account) => account,
                Err(error) => {
                    self.form_error = Some(error);
                    self.changed = true;
                    return;
                }
            };
            draft.assigned_id = Some(account.id.clone());
            self.form_saving = true;
            self.form_error = Some(t("accounts-checking"));
            self.changed = true;
            let round = self.form_round;
            let id = account.id.clone();
            let typed = draft.password.clone();
            let original_account = self.account_root.store().get(&id).cloned();
            self.run_in_lane(move |_| {
                Step::vault(
                    move |vault| {
                        let vault = vault.open();
                        let original = vault.get(&id);
                        Self::prepare_secret(&vault, &id, &typed).map(|secret| (secret, original))
                    },
                    move |pane, secret| {
                        if round == pane.form_round {
                            pane.form_saving = false;
                            match secret {
                                Ok((secret, original)) => {
                                    if account.type_id.as_str() == "caldav" {
                                        pane.calendar_baseline = Some((original_account, original));
                                    }
                                    pane.start_form_probe(draft, account, secret);
                                }
                                Err(error) => pane.form_error = Some(error),
                            }
                            pane.changed = true;
                        }
                        Step::Done
                    },
                )
            });
        }
    }

    /// Check `account` with `secret` on a worker; `draft` is saved once the
    /// server accepted it.
    fn start_form_probe(
        &mut self,
        draft: AccountDraft,
        account: accounts::Account,
        secret: accounts::Secret,
    ) {
        let tester = std::sync::Arc::clone(&self.account_tester);
        let (sender, receiver) = crate::channel::sync_channel(1);
        let _ = crate::device_read::thread(
            "settings-accounts-worker",
            Box::new(move || {
                let result = if account.type_id.as_str() == "caldav" {
                    tester.discover_calendars(&account, &secret)
                } else {
                    tester.test(&account, &secret).map(|_| Vec::new())
                };
                let _ = sender.send(result);
            }),
        );
        self.form_candidate = Some(draft);
        self.form_probe = Some(ProbeJob::new(receiver));
        self.form_error = Some(t("accounts-checking"));
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
        } else if self.form_kind == Some(AccountKind::CalDav) {
            let calendars = result.unwrap_or_default();
            if calendars.is_empty() {
                self.form_candidate = None;
                self.form_error = Some(t("accounts-calendars-empty"));
            } else {
                self.form_saving = true;
                let round = self.form_round;
                self.run_in_lane(move |pane| {
                    pane.check_calendar_credential_then(Box::new(move |pane, checked| {
                        if pane.form_round == round {
                            pane.form_saving = false;
                            pane.form_error = checked
                                .and_then(|()| pane.stage_calendar_choices(calendars))
                                .err();
                            pane.changed = true;
                        }
                        Step::Done
                    }))
                });
            }
        } else {
            let Some(candidate) = self.form_candidate.take() else {
                self.form_error = Some("account check result had no pending draft".into());
                self.changed = true;
                return;
            };
            self.form_saving = true;
            let round = self.form_round;
            self.run_in_lane(move |pane| {
                pane.save_draft_then(
                    candidate,
                    Box::new(move |pane: &mut Self, saved: Result<(), String>| {
                        pane.form_saved(round, saved)
                    }),
                )
            });
        }
        self.changed = true;
    }

    /// Take in how saving the form of `round` ended.
    fn form_saved(&mut self, round: u64, saved: Result<(), String>) -> Step {
        if saved.is_ok() {
            self.queue_reload();
        }
        if round == self.form_round {
            self.form_saving = false;
            match saved {
                Ok(()) => {
                    self.form_kind = None;
                    self.form_error = None;
                    self.form_account_id = None;
                    self.form_probe = None;
                    self.form_candidate = None;
                    self.calendar_choices.clear();
                    self.calendar_source = None;
                    self.calendar_baseline = None;
                    self.clear_form_fields();
                }
                Err(error) => self.form_error = Some(error),
            }
        }
        self.changed = true;
        Step::Done
    }

    fn cancel_reauth(&mut self) {
        {
            self.next_reauth_round();
            self.reauth_id = None;
            self.reauth_error = None;
            self.reauth_password.set(String::new());
            self.changed = true;
        }
    }

    /// Read the account's credential from the vault on a worker, then check
    /// it with the typed password on another; nothing is saved before the
    /// server accepted it.
    fn submit_reauth(&mut self) {
        {
            if self.reauth_probe.is_some() || self.reauth_saving {
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
                self.reauth_saving = true;
                self.reauth_error = Some(t("accounts-checking-reauth"));
                self.changed = true;
                let round = self.reauth_round;
                let stored_id = account_id.clone();
                self.run_in_lane(move |_| {
                    Step::vault(
                        move |vault| vault.open().get(&stored_id).unwrap_or_default(),
                        move |pane, mut secret: accounts::Secret| {
                            if round != pane.reauth_round {
                                return Step::Done;
                            }
                            pane.reauth_saving = false;
                            secret.password = password.trim().to_string();
                            let tester = std::sync::Arc::clone(&pane.account_tester);
                            let candidate = secret.clone();
                            let (sender, receiver) = crate::channel::sync_channel(1);
                            let _ = crate::device_read::thread(
                                "settings-accounts-worker",
                                Box::new(move || {
                                    let _ = sender.send(tester.test(&account, &candidate));
                                }),
                            );
                            pane.reauth_candidate = Some((account_id, secret));
                            pane.reauth_probe = Some(ProbeJob::new(receiver));
                            pane.changed = true;
                            Step::Done
                        },
                    )
                });
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
            self.reauth_saving = true;
            let round = self.reauth_round;
            self.run_in_lane(move |_| {
                Step::vault(
                    move |vault| {
                        let mut vault = vault.open();
                        vault.put(&account_id, &secret);
                        vault.try_save()
                    },
                    move |pane, saved| {
                        if round == pane.reauth_round {
                            pane.reauth_saving = false;
                            match saved {
                                Ok(()) => {
                                    pane.reauth_id = None;
                                    pane.reauth_error = None;
                                    pane.reauth_password.set(String::new());
                                }
                                Err(error) => pane.reauth_error = Some(error),
                            }
                        }
                        pane.changed = true;
                        Step::Done
                    },
                )
            });
        }
        self.changed = true;
    }
}

impl AccountsPane {
    /// Remove the account `id`, its credential and its sync record, waiting
    /// for the vault: for a caller off the UI thread. The window removes
    /// through [`AccountsEffect::RemoveAccount`], which waits for nothing.
    ///
    /// # Errors
    ///
    /// Why the account could not be removed.
    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        let outcome = Rc::new(std::cell::RefCell::new(None));
        let landed = Rc::clone(&outcome);
        self.remove_then(id, move |result| *landed.borrow_mut() = Some(result));
        self.settle_lane();
        outcome
            .take()
            .unwrap_or_else(|| Err("the removal stopped unexpectedly".into()))
    }

    /// The check of account `id`'s saved sign-in, to run on a worker: its
    /// credential read from the vault, then a protocol probe, or for a
    /// cloud drive a fresh token from the provider.
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
        let cloud = AccountKind::from_type_id(account.type_id.as_str())
            .is_some_and(AccountKind::is_cloud_storage);
        let vault = self.account_root.vault_opener();
        let custodian = self.custodian.clone();
        let tester = std::sync::Arc::clone(&self.account_tester);
        Ok(move || {
            let secret = vault.open().get(&account_id).ok_or_else(|| {
                "this account has no saved credential; sign in again first".to_string()
            })?;
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

    /// Take in finished checks, sign-in steps and vault work; whether
    /// anything shown changed.
    pub fn poll(&mut self) -> bool {
        let revision = self.account_root.revision();
        if !self.is_sample && revision != self.seen_accounts {
            if self.seen_accounts.is_some() {
                // After the vault work under way: a reload recovers a
                // journalled mutation, which must not be one still running.
                self.run_in_lane(|pane| {
                    pane.reload();
                    if let Err(error) = pane.account_root.project() {
                        tracing::warn!(%error, "the device's account file could not follow the person's accounts");
                    }
                    Step::Done
                });
                self.changed = true;
            }
            self.seen_accounts = revision;
        }
        self.advance_lane(false);
        self.poll_vault_opening();
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
            || !self.lane.is_idle()
            || self.vault_opening.is_some()
            || self
                .sign_in
                .as_ref()
                .is_some_and(cloud_signin::SignInFlow::busy)
    }
}

fn relative_sync_time(unix: i64) -> String {
    ui::format::relative(ui::time::unix_now_i64().saturating_sub(unix))
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "callers construct per-kind JSON values solely to merge them into the persisted opaque configuration"
)]
fn find_mailbox<'a>(
    store: &'a accounts::AccountStore,
    email: &str,
) -> Option<&'a accounts::Account> {
    store.all().iter().find(|account| {
        account.type_id.as_str() == "email"
            && account
                .config
                .get("email")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|existing| existing.eq_ignore_ascii_case(email))
    })
}

fn merge_config(target: &mut serde_json::Value, updates: serde_json::Value) {
    let serde_json::Value::Object(updates) = updates else {
        return;
    };
    if !target.is_object() {
        *target = serde_json::Value::Object(serde_json::Map::new());
    }
    if let Some(target) = target.as_object_mut() {
        for (key, value) in updates {
            target.insert(key, value);
        }
    }
}

/// Put back in `vault` what it held for `id` before a failed mutation.
fn restore_secret(
    mut vault: accounts::SecretVault,
    id: &accounts::AccountId,
    secret: Option<&accounts::Secret>,
) -> Result<(), String> {
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

/// What the vault held for each account a save wrote the credential of,
/// so a failed metadata write can put it back.
type PreviousSecrets = Vec<(accounts::AccountId, Option<accounts::Secret>)>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::sample_directory;
    use crate::host::testing::{count_named, labels_of};
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

    #[test]
    fn selected_browser_sessions_use_device_inventory_not_public_identity_records() {
        let (_scratch, root) = scratch_root();
        let mut store = accounts::AccountStore::new();
        store.upsert(accounts::Account::new("selected", browser_sessions::TYPE));
        store.upsert(accounts::Account::new("other", "email"));
        store.save_in(&root.path().unwrap()).unwrap();
        let pane = AccountsPane::from_dir_in(root.clone(), AccountDirectory::default(), false);
        let selections = pane.browser_service_sessions();
        assert_eq!(selections.len(), 1);
        assert_eq!(selections[0].id.as_str(), "selected");
        assert!(
            AccountsPane::from_dir_in(root, AccountDirectory::default(), true)
                .browser_service_sessions()
                .is_empty()
        );
    }

    /// Drive the host's polling until the pending account probe reports,
    /// however long the probe thread takes to be scheduled.
    fn settle_probes(pane: &mut AccountsPane) {
        let deadline = ui::time::now() + ProbeJob::<String>::DEADLINE;
        loop {
            let _ = pane.poll();
            if pane.form_probe.is_none()
                && pane.reauth_probe.is_none()
                && !pane.form_saving
                && !pane.reauth_saving
                && pane.lane.is_idle()
            {
                return;
            }
            assert!(ui::time::now() < deadline, "account probe never reported");
            std::thread::sleep(std::time::Duration::from_millis(1));
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

    pub(super) fn lay_out(pane: &AccountsPane) -> Box<dyn Widget> {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut composed = pane.build_composition();
        let _ = composed.build_layout(&mut taffy, &theme, &mut node_map);
        composed
    }

    /// Sample directories are read-only, real ones get labelled destructive remove buttons, and empty stores stay empty.
    #[test]
    fn import_only_rows_keep_recorded_errors_offline_and_signin_requirements() {
        fn texts(widget: &dyn Widget) -> Vec<String> {
            let mut result: Vec<_> = widget
                .accessibility()
                .and_then(|node| node.value)
                .into_iter()
                .collect();
            for child in widget.children() {
                result.extend(texts(child));
            }
            result
        }
        let (_scratch, root) = scratch_root();
        let id = accounts::AccountId::from("import-google");
        let mut account =
            accounts::Account::new(id.clone(), account_custody::providers::GOOGLE_DRIVE_TYPE);
        account.config = serde_json::json!({"files_mode": "import", "use_files": true});
        let mut store = accounts::AccountStore::new();
        store.upsert(account);
        root.save_store(&store).unwrap();
        let reporter = root.sync_reporter(id);
        for expected in [
            "accounts-status-error",
            "accounts-status-offline",
            "accounts-status-reauth",
        ] {
            match expected {
                "accounts-status-error" => reporter.failed(),
                "accounts-status-offline" => reporter.offline(),
                _ => reporter.needs_authentication(),
            }
            .unwrap();
            let pane = AccountsPane::in_root(root.clone());
            let composed = lay_out(&pane);
            let labels = texts(composed.as_ref());
            assert!(labels.contains(&t(expected)), "{labels:?}");
            assert!(
                !labels.contains(&t("accounts-status-import-only")),
                "a known failure is not an untested import source"
            );
        }
    }

    #[test]
    fn accounts_pane_renders_only_real_accounts_as_removable() {
        // Case `sample_directory_renders_read_only_without_remove_buttons`.
        {
            // The sample is illustration: one live Add button per provider kind,
            // metadata import, and no Remove buttons. A scratch root keeps whatever the runner's
            // own account stores hold (a browser identity, say) out of the count.
            let (_scratch, root) = scratch_root();
            let pane = AccountsPane::from_dir_in(root, sample_directory(), true);
            let composed = lay_out(&pane);
            assert_eq!(
                count_named(composed.as_ref(), "Button"),
                AccountKind::ALL.len() + 1,
                "every provider kind can be added and metadata imported over the sample"
            );
            let mut labels = Vec::new();
            labels_of(composed.as_ref(), "Button", &mut labels);
            assert_eq!(
                labels
                    .iter()
                    .filter(|label| AccountKind::ALL.into_iter().any(|kind| **label
                        == t_args(
                            "accounts-add-provider",
                            &[("kind", kind_shown(kind).into())]
                        )))
                    .count(),
                AccountKind::ALL.len(),
                "one Add action per provider kind"
            );
            assert_eq!(
                labels
                    .iter()
                    .filter(|label| **label == t("accounts-import-choose"))
                    .count(),
                1,
                "one metadata import action"
            );
            assert!(
                labels.iter().all(|label| *label != t("accounts-remove")),
                "sample accounts cannot be removed"
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
            // One Add per kind, metadata import, and four maintenance actions
            // on each of four accounts.
            assert_eq!(
                count_named(composed.as_ref(), "Button"),
                AccountKind::ALL.len() + 1 + 16
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
            labels_of(composed.as_ref(), "Button", &mut labels);
            assert_eq!(
                labels
                    .iter()
                    .filter(|label| [
                        "accounts-remove",
                        "accounts-remove-files",
                        "accounts-disconnect-mail"
                    ]
                    .into_iter()
                    .any(|key| **label == t(key)))
                    .count(),
                4,
                "one explicit Remove action per real account"
            );
            assert_eq!(
                labels
                    .iter()
                    .filter(|label| AccountKind::ALL.into_iter().any(|kind| **label
                        == t_args(
                            "accounts-add-provider",
                            &[("kind", kind_shown(kind).into())]
                        )))
                    .count(),
                AccountKind::ALL.len(),
                "the single chooser retains every provider's add action"
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
            labels_of(composed.as_ref(), "Button", &mut labels);
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

    /// Wait for a background read to end, as the warm-up's wake would.
    fn until_finished(finished: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !finished() {
            assert!(std::time::Instant::now() < deadline, "the read ends");
            std::thread::yield_now();
        }
    }

    /// The warm-up opens the vault off the UI thread once; the model holds
    /// what it opened, so a saved credential is there.
    #[test]
    fn the_area_builds_its_model_from_the_read_it_started() {
        let (dir, root) = scratch_root();
        let id = accounts::AccountId::from("warm@example.test");
        let mut store = accounts::AccountStore::new();
        store.upsert(accounts::Account::new(id.clone(), "email"));
        root.save_store(&store).expect("the store is saved");
        let mut vault = root.vault();
        vault.put(&id, &accounts::Secret::password("test-only-password"));
        vault.try_save().expect("the vault is saved");
        let cx = PaneCx::in_file(dir.path().join("settings.json"));
        let area = AccountsArea::new(&cx).with_account_root(root.clone());
        assert!(area.start_read(), "the first read starts");
        assert!(!area.start_read(), "one read at a time");
        assert!(!area.is_open(), "a running read publishes nothing yet");
        until_finished(|| area.read_finished());
        let model = area.model().borrow();
        assert!(area.is_open());
        assert!(vault.get(&id).is_some(), "the credential was saved");
        assert_eq!(
            model.vault.as_ref().and_then(|held| held.get(&id)),
            vault.get(&id),
            "the vault read is held"
        );
        assert_eq!(
            model.directory(),
            AccountsPane::in_root(root).directory(),
            "the directory is the one reading in place shows"
        );
        drop(model);
        assert!(!area.start_read(), "a built model is not read again");
    }

    /// A mutation whose intent is on disk is recovered before the vault is
    /// opened, so a vault read before it is not used.
    #[test]
    fn a_pending_mutation_is_seen() {
        let (_dir, root) = scratch_root();
        assert!(!root.mutation_pending());
        let config = root.path().expect("an explicit root");
        let accounts_dir = accounts::accounts_dir_in(&config);
        std::fs::create_dir_all(&accounts_dir).expect("the accounts directory");
        std::fs::write(accounts_dir.join("mutation-journal.json"), b"{}").expect("an intent");
        assert!(root.mutation_pending());
    }

    /// A Secret Service that does not answer holds no click: removing an
    /// account and checking its connection answer at once on the window's
    /// thread, the account stays shown until the vault took its removal,
    /// and the removal lands once the vault answers.
    #[test]
    fn a_stalled_vault_never_holds_the_window() {
        use std::sync::{Arc, Condvar, Mutex};
        use std::time::{Duration, Instant};

        let (_dir, root) = scratch_root();
        let id = accounts::AccountId::from("stalled@example.test");
        let mut store = accounts::AccountStore::new();
        store.upsert(accounts::Account::new(id.clone(), "email"));
        root.save_store(&store).expect("the store is saved");
        let mut vault = root.vault();
        vault.put(&id, &accounts::Secret::password("test-only-password"));
        vault.try_save().expect("the vault is saved");
        // What the window's warm-up opened off the UI thread.
        let read = AccountsRead::of(&root);
        let config = root.path().expect("an explicit root");
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let window = {
            let id = id.clone();
            std::thread::spawn(move || {
                ui::rt::mark_ui_thread();
                let stalled = Arc::clone(&gate);
                let root = AccountRoot::at(config).with_vault_gate(move || {
                    let (open, opened) = &*stalled;
                    // A safety deadline, so a failing test cannot hang.
                    let deadline = Instant::now() + Duration::from_secs(10);
                    let mut open = open.lock().unwrap();
                    while !*open && Instant::now() < deadline {
                        let left = deadline.saturating_duration_since(Instant::now());
                        open = opened.wait_timeout(open, left).unwrap().0;
                    }
                    drop(open);
                });
                let dir = AccountDirectory::load_in(&root);
                let mut pane = AccountsPane::from_read(root.clone(), dir, read);

                let clicked = Instant::now();
                let removed = Rc::new(std::cell::RefCell::new(None));
                let landed = Rc::clone(&removed);
                pane.remove_then(id.as_str(), move |result| {
                    *landed.borrow_mut() = Some(result);
                });
                let check = pane.connection_check(id.as_str());
                let _ = pane.poll();
                let answered = clicked.elapsed();
                // The vault answers nothing until the gate opens below (or
                // its ten-second safety deadline passes): clicks answered
                // before then did not wait for it. The journal they write
                // is a local file, slow only on a loaded test machine.
                assert!(
                    answered < Duration::from_secs(10),
                    "the clicks waited {answered:?} on the vault"
                );
                assert!(check.is_ok(), "the check reads the vault on its worker");
                assert!(
                    removed.borrow().is_none(),
                    "the removal waits for the vault"
                );
                assert!(
                    PaneModel::busy(&pane),
                    "the pane keeps polling while the vault is out"
                );
                assert!(
                    root.store().get(&id).is_some(),
                    "the account is kept until the vault took its removal"
                );

                *gate.0.lock().unwrap() = true;
                gate.1.notify_all();
                let deadline = Instant::now() + Duration::from_secs(30);
                while removed.borrow().is_none() || !pane.lane.is_idle() {
                    assert!(Instant::now() < deadline, "the removal lands");
                    let _ = pane.poll();
                    std::thread::yield_now();
                }
                assert_eq!(removed.take(), Some(Ok(())));
                assert!(root.store().get(&id).is_none(), "the metadata is gone");
            })
        };
        if let Err(panic) = window.join() {
            std::panic::resume_unwind(panic);
        }
        assert!(
            root.vault().get(&id).is_none(),
            "the credential went with the account"
        );
        assert!(!root.mutation_pending(), "the removal's journal is cleared");
    }
}
