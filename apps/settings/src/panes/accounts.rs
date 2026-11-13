//! External Accounts: one inventory over the canonical account stores that
//! installed clients consume. Accounts are added and edited in one device-local
//! form; removing one clears its metadata, its sealed credential and its sync
//! record together.

use std::rc::Rc;

use accounts::email::AccountConfig;
use tracing::warn;
use ui::LocalState;
use ui::framework::{ActionControl, EffectAnswer, EffectCx, Effected, LiveRow, ViewCx};
use ui::tilo_resource::{self, Effect};
use ui::widget::{Button, Element, Row, RowMenu, Spacer, TextInput, TextInputKind, Widget};

use crate::accounts::{AccountDirectory, AccountKind, AccountRoot, AccountView};
use crate::area::AreaContext;
use crate::effects::SettingsEffect;
use crate::i18n::{t, t_args};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    Body, El, Item, SettingsSection, StatusTone, field_row, note, row, rows, section, setting_row,
    setting_row_described, thing_row, untitled, value_row,
};

mod add_email;
mod browser_sessions;
mod calendar_selection;
mod cloud_control;
mod cloud_signin;
mod import_review;
mod row_view;
mod thunderbird_import;
mod vault_lane;
pub(crate) use cloud_control::CloudMounts;
pub use cloud_signin::{BrowserOpener, PortalOpener};
use row_view::{AccountEntry, account_status, kind_icon, kind_shown, sync_detail, sync_entries};
use runtime_fs::cloud::CloudRequest;
use vault_lane::Step;

crate::effects::effect_family!(AccountsEffect => answer);

crate::area::pane_area! {
    AccountsArea;
    fn poll(&self) {
        AccountsArea::poll(self);
    }
    fn busy(&self) -> bool {
        AccountsArea::busy(self)
    }
    fn take_collab_requests(&self) -> Vec<ui::collaboration::CollabRequest> {
        ui::widget::Collaborative::take_collab_requests(&mut self.remote.clone())
    }
    fn handle_collab_event(&self, event: &ui::collaboration::CollabEvent) {
        ui::widget::Collaborative::handle_collab_event(&mut self.remote.clone(), event);
    }
}

pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&AccountsArea> {
    app.areas().find()
}

pub(crate) fn view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    let held = area(app)?;
    let model = Rc::clone(held.model());
    let _ = model.borrow_mut().poll();
    Some(cx.switch(&held.shown, move |cx, _| model.borrow().page(cx)))
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod cloud_import_acceptance;
#[cfg(test)]
mod signin_tests;

#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum AccountsEffect {
    /// Forget one selected website account through the browser, including its local lease.
    #[effect(row, job, returns, destructive)]
    RemoveBrowserServiceSession {
        session_id: String,
    },
    /// Discover only ordinary account metadata from a granted prefs.js path.
    #[effect(job, returns, kind = "read")]
    DiscoverThunderbirdAccounts {
        prefs: String,
    },
    /// Choose ordinary Thunderbird prefs through the existing file portal.
    #[effect(attended, job, returns, kind = "read")]
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
    #[effect(job, returns, idempotent)]
    CommitThunderbirdImport {
        revision: u64,
    },
    /// Discard the metadata review and fence unfinished discovery callbacks.
    #[effect(view)]
    CancelThunderbirdImport,
    /// Show or hide public metadata in the browser identity picker.
    #[effect(row, idempotent)]
    SetBrowserIdentityEnabled {
        account: String,
        enabled: bool,
    },
    /// Remove browser picker metadata; this does not revoke sign-in or access.
    #[effect(row, destructive)]
    RemoveBrowserIdentity {
        account: String,
    },
    /// Publish the signed-in Tilo identity's public browser connection metadata.
    #[effect(idempotent)]
    PublishBrowserIdentity {
        #[param("Canonical HTTPS origin of the existing Tilo host")]
        origin: String,
    },
    /// Add an account: mail, caldav, webdav, nextcloud, google-drive,
    /// onedrive, dropbox or icloud-drive.
    #[effect(view)]
    #[effect(label = "accounts-add")]
    AddAccount {
        #[param("The kind of account, by its slug")]
        kind: String,
    },
    /// Add an email account with its password. The password is sealed by
    /// the session's account custody and never shown or read back; Mail,
    /// which keeps email accounts, is released it to sign in.
    #[effect(job, returns)]
    AddEmailAccount {
        #[param("The name shown for it")]
        display_name: String,
        #[param("The email address")]
        email: String,
        #[param("The name it signs in with; the address when omitted")]
        username: Option<String>,
        #[param("The IMAP server's host")]
        imap_host: String,
        #[param("The IMAP server's port")]
        imap_port: u16,
        #[param("The SMTP server's host")]
        smtp_host: String,
        #[param("The SMTP server's port")]
        smtp_port: u16,
        #[param(secret, "The account's password or app password")]
        password: String,
    },
    /// Edit an account's details.
    #[effect(view, row)]
    EditAccount {
        #[param("The account's id")]
        account: String,
    },
    /// Sign an account in again.
    #[effect(view, row, label = "accounts-status-reauth")]
    SignInAgain {
        #[param("The account's id")]
        account: String,
    },
    /// Check and save the account form, or take the sign-in's next step.
    #[effect(label = "accounts-save")]
    SaveAccountForm,
    /// Choose calendars on an existing Nextcloud account, reusing its sign-in.
    #[effect(row, label = "accounts-nextcloud-calendars")]
    ConnectNextcloudCalendars {
        account: String,
    },
    /// Read the discovered, secret-free calendar choices for the current form.
    #[effect(returns, kind = "read")]
    InspectCalendarChoices,
    /// Select a discovered collection before saving this account.
    #[effect(view)]
    SelectCalendar {
        href: String,
        selected: bool,
    },
    /// Select the services on the open provider connection form.
    #[effect(view)]
    SelectAccountServices {
        files: bool,
        mail: bool,
        #[param("Files mode: sync or import")]
        files_mode: String,
    },
    ConnectGoogleMail,
    /// Close the account form or sign-in without saving.
    #[effect(view)]
    CancelAccountForm,
    SaveNewPassword,
    /// Close the sign-in-again form without saving.
    #[effect(view)]
    CancelNewPassword,
    /// Check that an account's server accepts its saved sign-in.
    #[effect(job, returns, row, idempotent)]
    TestAccount {
        #[param("The account's id")]
        account: String,
    },
    /// Remove an account, its saved credential and its sync record.
    #[effect(row, job, destructive)]
    RemoveAccount {
        #[param("The account's id")]
        account: String,
    },
    /// Sync a mounted file account now; one that could not start is tried
    /// again.
    #[effect(row, job, idempotent, label = "accounts-sync-now")]
    SyncNow {
        #[param("The account's id")]
        account: String,
    },
    /// Pause a mounted file account's background sync and uploads.
    #[effect(row, job, idempotent, label = "accounts-pause-sync")]
    PauseSync {
        #[param("The account's id")]
        account: String,
    },
    /// Resume a paused file account's sync.
    #[effect(row, job, idempotent, label = "accounts-resume-sync")]
    ResumeSync {
        #[param("The account's id")]
        account: String,
    },
    /// Recover an interrupted Files mode change after closing the connection form.
    #[effect(row, job, idempotent, label = "accounts-recover-mode")]
    RecoverCloudMode {
        account: String,
    },
    LinkService {
        #[param("The connector's app id")]
        app: String,
    },
    /// Unlink your account with a service this workspace is connected to.
    #[effect(destructive)]
    UnlinkService {
        #[param("The connector's app id")]
        app: String,
    },
    AcceptService {
        #[param("The connector's app id")]
        app: String,
    },
    DeclineService {
        #[param("The connector's app id")]
        app: String,
    },
}

pub(crate) fn answer(
    app: &SettingsApp,
    effect: AccountsEffect,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    match effect {
        AccountsEffect::RemoveBrowserServiceSession { session_id } => {
            let area = crate::panes::accounts::area(app).ok_or_else(crate::area::missing)?;
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
        add @ AccountsEffect::AddEmailAccount { .. } => return add_email::answer(app, add, cx),
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
        AccountsEffect::LinkService { app: service } => {
            crate::panes::accounts::area(app)
                .ok_or_else(crate::area::missing)?
                .linked()
                .link(&service);
            return Ok(Effected::done());
        }
        AccountsEffect::UnlinkService { app: service } => {
            crate::panes::accounts::area(app)
                .ok_or_else(crate::area::missing)?
                .linked()
                .unlink(&service);
            return Ok(Effected::done());
        }
        AccountsEffect::AcceptService { app: service } => {
            crate::panes::accounts::area(app)
                .ok_or_else(crate::area::missing)?
                .linked()
                .accept(&service);
            return Ok(Effected::done());
        }
        AccountsEffect::DeclineService { app: service } => {
            crate::panes::accounts::area(app)
                .ok_or_else(crate::area::missing)?
                .linked()
                .decline(&service);
            return Ok(Effected::done());
        }
        _ => {}
    }
    answer_model(app, effect, cx)
}

fn answer_import(
    model: &mut AccountsPane,
    effect: AccountsEffect,
    cx: &mut EffectCx<Settings>,
) -> Result<EffectAnswer, Box<AccountsEffect>> {
    Ok(match effect {
        AccountsEffect::DiscoverThunderbirdAccounts { .. }
        | AccountsEffect::ChooseThunderbirdMetadata => Err(t("accounts-import-read-failed")),
        AccountsEffect::InspectThunderbirdImport => {
            model.import_review.inspect().map(Effected::value)
        }
        AccountsEffect::SelectThunderbirdAccount {
            source,
            authentication,
            selected,
            revision,
        } => model
            .import_review
            .select(&source, &authentication, selected, revision)
            .map(|()| {
                model.changed = true;
                Effected::done()
            }),
        AccountsEffect::CommitThunderbirdImport { revision } => {
            commit_thunderbird_import(model, revision, cx)
        }
        AccountsEffect::CancelThunderbirdImport => model.import_review.cancel().map(|()| {
            model.changed = true;
            Effected::done()
        }),
        other => return Err(Box::new(other)),
    })
}

fn answer_model(
    app: &SettingsApp,
    effect: AccountsEffect,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    let shared = crate::panes::accounts::area(app)
        .ok_or_else(crate::area::missing)?
        .model();
    let mut model = shared.borrow_mut();
    let effect = match answer_import(&mut model, effect, cx) {
        Ok(answer) => return answer,
        Err(effect) => *effect,
    };
    match effect {
        AccountsEffect::DiscoverThunderbirdAccounts { .. }
        | AccountsEffect::ChooseThunderbirdMetadata
        | AccountsEffect::InspectThunderbirdImport
        | AccountsEffect::SelectThunderbirdAccount { .. }
        | AccountsEffect::CommitThunderbirdImport { .. }
        | AccountsEffect::CancelThunderbirdImport => {
            return Err(t("accounts-import-read-failed"));
        }
        AccountsEffect::RemoveBrowserServiceSession { .. }
        | AccountsEffect::AddEmailAccount { .. }
        | AccountsEffect::PublishBrowserIdentity { .. }
        | AccountsEffect::SetBrowserIdentityEnabled { .. }
        | AccountsEffect::RemoveBrowserIdentity { .. }
        | AccountsEffect::LinkService { .. }
        | AccountsEffect::UnlinkService { .. }
        | AccountsEffect::AcceptService { .. }
        | AccountsEffect::DeclineService { .. } => {
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
        AccountsEffect::SaveAccountForm => submit_account_form(&mut model)?,
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
            return cloud(app, model, CloudRequest::SyncNow { account }, cx);
        }
        AccountsEffect::PauseSync { account } => {
            return cloud(app, model, CloudRequest::Pause { account }, cx);
        }
        AccountsEffect::ResumeSync { account } => {
            return cloud(app, model, CloudRequest::Resume { account }, cx);
        }
        AccountsEffect::RecoverCloudMode { account } => {
            return recover_mode(app, model, account, cx);
        }
        AccountsEffect::TestAccount { account } => return test_account(app, model, &account, cx),
    }
    Ok(Effected::done())
}

fn submit_account_form(model: &mut AccountsPane) -> Result<(), String> {
    if !model.calendar_choices.is_empty()
        && !model.calendar_choices.iter().any(|(_, selected)| *selected)
    {
        return Err(t("accounts-calendars-empty"));
    }
    if !model.calendar_choices.is_empty() {
        model.validate_calendar_review()?;
    }
    model.command(AccountViewCommand::Submit);
    Ok(())
}

fn commit_thunderbird_import(
    model: &mut AccountsPane,
    revision: u64,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
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
    app: &SettingsApp,
    model: std::cell::RefMut<'_, AccountsPane>,
    account: String,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    if model.sign_in.is_some() {
        return Err("Close the account connection form before recovering its mode change".into());
    }
    cloud(app, model, CloudRequest::RecoverImport { account }, cx)
}

fn test_account(
    app: &SettingsApp,
    model: std::cell::RefMut<'_, AccountsPane>,
    account: &str,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    let check = model.connection_check(account)?;
    drop(model);
    Ok(app.job(
        cx,
        check,
        |_: &SettingsApp, outcome: Result<String, String>, _| complete_account_check(outcome),
    ))
}

fn complete_account_check(outcome: Result<String, String>) -> EffectAnswer {
    outcome.map(Effected::value)
}

fn cloud(
    app: &SettingsApp,
    model: std::cell::RefMut<'_, AccountsPane>,
    request: CloudRequest,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    let send = model.cloud.request(request)?;
    drop(model);
    Ok(app.job(
        cx,
        send,
        |app: &SettingsApp, outcome: Result<(), String>, _| {
            crate::panes::accounts::area(app)
                .ok_or_else(crate::area::missing)?
                .model()
                .borrow_mut()
                .cloud
                .invalidate();
            outcome.map(|()| Effected::done())
        },
    ))
}

pub struct AccountsArea {
    scene: Option<Rc<crate::scenarios::CaptureScene>>,
    linked: ui::linked_accounts::LinkedAccounts,
    pub(crate) remote: ui::RemoteActions,
    removal: std::cell::RefCell<Option<browser_sessions::Removal>>,
    custody: Option<ui::collaboration::browser_service_sessions::Client>,
    adding: std::cell::RefCell<Option<add_email::AddingEmail>>,
    model: std::cell::OnceCell<Rc<std::cell::RefCell<AccountsPane>>>,
    account_root: Option<AccountRoot>,
    pub(crate) read: crate::device_read::DeviceRead<AccountsRead>,
    linked_seen: std::cell::Cell<u64>,
    shown: LocalState<u64>,
}

pub(crate) struct AccountsRead {
    statuses: accounts::AccountSyncStatusStore,
    vault: accounts::SecretVault,
    custodian: account_custody::Custodian,
}

impl AccountsRead {
    fn of(root: &AccountRoot) -> Self {
        Self {
            statuses: root.statuses(),
            vault: root.vault(),
            custodian: root.custodian(),
        }
    }

    fn under(explicit: Option<std::path::PathBuf>) -> Self {
        Self::of(&explicit.map_or_else(AccountRoot::live, AccountRoot::at))
    }
}

impl AccountsArea {
    pub fn new(cx: &AreaContext) -> Self {
        Self {
            scene: cx.scene.clone(),
            linked: cx.linked.clone(),
            remote: ui::RemoteActions::current(),
            removal: std::cell::RefCell::default(),
            custody: ui::collaboration::browser_service_sessions::Client::capture(),
            adding: std::cell::RefCell::default(),
            model: std::cell::OnceCell::new(),
            account_root: None,
            read: crate::device_read::DeviceRead::default(),
            linked_seen: std::cell::Cell::new(0),
            shown: LocalState::new(0),
        }
    }

    #[cfg(test)]
    pub(crate) fn holding(cx: &AreaContext, mut pane: AccountsPane) -> Self {
        let area = Self::new(cx);
        if pane.linked.is_none() {
            pane.linked = Some(cx.linked.clone());
        }
        let _ = area.model.set(Rc::new(std::cell::RefCell::new(pane)));
        area
    }

    pub(crate) fn start_read(&self) -> bool {
        if self.is_open() || self.scene.is_some() {
            return false;
        }
        let explicit = self
            .account_root
            .as_ref()
            .and_then(|root| root.explicit().map(std::path::Path::to_path_buf));
        self.read.start(move || AccountsRead::under(explicit))
    }

    pub(crate) fn with_account_root(mut self, root: AccountRoot) -> Self {
        self.account_root = Some(root);
        self
    }

    pub fn model(&self) -> &Rc<std::cell::RefCell<AccountsPane>> {
        self.model.get_or_init(|| {
            let scene = self.scene.as_deref();
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
                AccountsPane::recovered_in(root, read.map(|read| read.custodian))
            };
            if let Some(kind) = scene.and_then(|scene| scene.account_form) {
                model = model.with_open_form_for_shot(kind);
            }
            self.linked.refresh();
            Rc::new(std::cell::RefCell::new(
                model.with_linked(self.linked.clone()),
            ))
        })
    }

    pub(crate) fn poll(&self) {
        self.remote.poll();
        self.follow_linked();
        self.poll_adding();
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
        if let Some(model) = self.model.get()
            && let Ok(mut pane) = model.try_borrow_mut()
            && pane.poll()
        {
            self.shown.set(self.shown.get().wrapping_add(1));
        }
    }

    fn busy(&self) -> bool {
        let pane = |model: &Rc<std::cell::RefCell<AccountsPane>>| {
            model.try_borrow().is_ok_and(|pane| pane.busy())
        };
        self.adding.borrow().is_some() || self.model.get().is_some_and(pane)
    }

    pub(crate) fn linked(&self) -> &ui::linked_accounts::LinkedAccounts {
        &self.linked
    }

    fn follow_linked(&self) {
        let revision = self.linked.revision();
        if self.linked_seen.replace(revision) != revision
            && let Some(model) = self.model.get()
        {
            model.borrow_mut().changed = true;
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.model.get().is_some()
    }

    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let model = self.model().borrow();
        if model.is_sample {
            return Vec::new();
        }
        let offer = |effect: AccountsEffect| SettingsEffect::from(effect).bound();
        let store = model.account_root.store();
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
                if let Some(action) = model.files_handoff_in(&store, &view) {
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
        for account in store.list(&accounts::person::TILO_IDENTITY_TYPE.into()) {
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

type SessionsRead = Option<(Option<std::time::SystemTime>, Vec<accounts::Account>)>;

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

struct AccountDraft {
    kind: AccountKind,
    existing_id: Option<String>,
    assigned_id: Option<accounts::AccountId>,
    name: String,
    identity: String,
    username: String,
    server: String,
    password: String,
}

type Settle = Box<dyn FnOnce(&mut AccountsPane, Result<(), String>) -> Step>;

struct Removal {
    id: accounts::AccountId,
    original_store: accounts::AccountStore,
    original_statuses: accounts::AccountSyncStatusStore,
    credential_changed: bool,
    original_secret: Option<accounts::Secret>,
}

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

#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent state of the pane's forms and vault work, never a parameter"
)]
pub struct AccountsPane {
    linked: Option<ui::linked_accounts::LinkedAccounts>,
    account_root: AccountRoot,
    seen_accounts: Option<u64>,
    sessions_read: std::cell::RefCell<SessionsRead>,
    dir: AccountDirectory,
    sync_statuses: accounts::AccountSyncStatusStore,
    vault: Option<accounts::SecretVault>,
    vault_opening: Option<crate::device_read::Landing>,
    mail_grants: MailGrants,
    lane: vault_lane::VaultLane,
    reload_queued: bool,
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
    calendar_source: Option<accounts::Account>,
    // Private discovery baseline: never published into the resource tree.
    calendar_baseline: Option<(Option<accounts::Account>, Option<accounts::Secret>)>,
    form_candidate: Option<AccountDraft>,
    form_saving: bool,
    form_round: u64,
    reauth_probe: Option<ProbeJob>,
    reauth_candidate: Option<(accounts::AccountId, accounts::Secret)>,
    reauth_saving: bool,
    reauth_round: u64,
    sign_in: Option<cloud_signin::SignInFlow>,
    sign_in_saving: bool,
    sign_in_round: u64,
    custodian: account_custody::Custodian,
    opener: std::sync::Arc<dyn BrowserOpener>,
    cloud: CloudMounts,
    browser_removal_error: Option<String>,
    browser_removal_pending: bool,
    import_review: import_review::ImportReview,
    changed: bool,
}

impl AccountsPane {
    #[must_use]
    pub fn new() -> Self {
        Self::in_root(AccountRoot::live())
    }

    /// The live pane over the account stores under `account_root`, its
    /// vault opened here: never on the UI thread, where the window builds
    /// its pane from what the area read off it.
    pub fn in_root(account_root: AccountRoot) -> Self {
        let dir = AccountDirectory::load_in(&account_root);
        let read = AccountsRead::of(&account_root);
        Self::from_read(account_root, dir, read)
    }

    fn from_read(account_root: AccountRoot, dir: AccountDirectory, read: AccountsRead) -> Self {
        Self {
            sync_statuses: read.statuses,
            vault: Some(read.vault),
            cloud: CloudMounts::live(),
            ..Self::over(account_root, dir, false, read.custodian)
        }
    }

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
            mail_grants: MailGrants::default(),
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
            linked: None,
            account_root,
            seen_accounts: None,
            sessions_read: std::cell::RefCell::new(None),
        }
    }

    pub fn from_dir(dir: AccountDirectory, is_sample: bool) -> Self {
        Self::from_dir_in(AccountRoot::live(), dir, is_sample)
    }

    pub fn from_dir_in(account_root: AccountRoot, dir: AccountDirectory, is_sample: bool) -> Self {
        let custodian = account_root.custodian();
        let mut pane = Self::over(account_root, dir, is_sample, custodian);
        pane.reopen_vault();
        pane
    }

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

    /// Drive the live host's nonblocking polling until the pending
    /// submission settles: either calendar selection is ready or the account
    /// is saved. Returns false on failure; a ready calendar review is not a
    /// saved account.
    #[doc(hidden)]
    pub fn finish_form_submission_for_test(&mut self, bound: std::time::Duration) -> bool {
        let deadline = ui::time::now() + bound;
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

    #[must_use]
    pub(crate) fn with_linked(mut self, linked: ui::linked_accounts::LinkedAccounts) -> Self {
        self.linked = Some(linked);
        self
    }

    fn bound(cx: &ViewCx<Settings>, button: Button, effect: AccountsEffect) -> Button {
        match Self::row_effect_binding(cx, effect) {
            Some(binding) => button.bind_action(binding),
            None => button.unavailable(),
        }
    }

    fn row_account(effect: &AccountsEffect) -> Option<&String> {
        match effect {
            AccountsEffect::EditAccount { account }
            | AccountsEffect::SignInAgain { account }
            | AccountsEffect::TestAccount { account }
            | AccountsEffect::RemoveAccount { account }
            | AccountsEffect::SyncNow { account }
            | AccountsEffect::PauseSync { account }
            | AccountsEffect::ResumeSync { account }
            | AccountsEffect::RecoverCloudMode { account } => Some(account),
            _ => None,
        }
    }

    /// `effect`'s binding: on the account's own resource for a row effect
    /// (its contract is published there, not on the Settings root), else on
    /// the root. `None` where it is not offered.
    fn row_effect_binding(
        cx: &ViewCx<Settings>,
        effect: AccountsEffect,
    ) -> Option<ui::tilo_actions::ActionBinding<()>> {
        let Some(account) = Self::row_account(&effect).cloned() else {
            return cx.actions().effect(&SettingsEffect::from(effect));
        };
        let id = effect.id();
        let resource = LiveRow::new(
            format!("settings.account:{account}"),
            "settings.account",
            &account,
        )
        .effect(&SettingsEffect::from(effect))
        .into_resource();
        let invocation = ui::tilo_resource::ActionRef::<SettingsEffect, ()>::new(
            id,
            &[],
            SettingsEffect::kind(),
        );
        crate::on_row::binding(&cx.dispatcher(), &resource, invocation)
    }

    #[must_use]
    pub fn with_account_tester(
        mut self,
        tester: std::sync::Arc<dyn crate::account_probe::AccountTester>,
    ) -> Self {
        self.account_tester = tester;
        self
    }

    #[must_use]
    pub fn with_custodian(mut self, custodian: account_custody::Custodian) -> Self {
        self.custodian = custodian;
        self
    }

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

    #[doc(hidden)]
    pub fn finish_sign_in_step_for_test(&mut self, timeout: std::time::Duration) -> bool {
        let reported = if let Some(flow) = self.sign_in.as_mut()
            && flow.busy()
        {
            let reported = flow.wait_for_test(timeout);
            self.poll_sign_in();
            reported
        } else {
            true
        };
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

    fn sign_in_saved(&mut self, round: u64, saved: Result<accounts::AccountId, String>) -> Step {
        let current = round == self.sign_in_round;
        if current {
            self.sign_in_saving = false;
        }
        match saved {
            Ok(id) => {
                if current && let Some(flow) = self.sign_in.as_mut() {
                    flow.finish_async(id);
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

    fn next_form_round(&mut self) {
        self.form_round = self.form_round.wrapping_add(1);
        self.form_saving = false;
        self.form_probe = None;
        self.form_candidate = None;
    }

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

    fn page(&self, cx: &ViewCx<Settings>) -> Element {
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
            intro.push(note(&t("accounts-empty-sample")));
        } else if self.dir.is_empty() {
            intro.push(note(&t("accounts-empty")));
        }
        panel.push(untitled(rows(intro)));

        let adding = match &self.sign_in {
            Some(flow) if !flow.renews_an_account() => {
                Some(flow.section(&|button, effect| Self::bound(cx, button, effect)))
            }
            Some(_) => None,
            None => self.form_kind.map(|kind| self.account_form(cx, kind)),
        };
        if let Some(flow) = self
            .sign_in
            .as_ref()
            .filter(|flow| flow.renews_an_account())
        {
            panel.push(flow.section(&|button, effect| Self::bound(cx, button, effect)));
        }
        if let Some(id) = &self.reauth_id {
            panel.push(self.reauth_form(cx, id));
        }
        // The services this workspace is connected to come first: they belong
        // to this workspace, and show only when it has one.
        if let Some(linked) = self.linked_accounts_card(cx) {
            panel.push(linked);
        }
        panel.push(self.browser_service_sessions_card(cx));
        // Connected services lead. Empty providers belong to the one chooser,
        // not to an inventory of accounts the person has not connected.
        for kind in AccountKind::ALL {
            if kind == AccountKind::Mail {
                if let Some(mail) = self.mail_card(cx) {
                    panel.push(mail);
                }
            } else if kind == AccountKind::GoogleDrive {
                for view in self.dir.views_of(kind) {
                    let title = kind_shown(kind);
                    panel.push(section(&title, self.kind_body(cx, kind, &[view])));
                }
            } else if !self.dir.views_of(kind).is_empty() {
                panel.push(self.kind_card(cx, kind));
            }
        }
        panel.push(adding.unwrap_or_else(|| Self::add_account_card(cx)));
        panel.push(self.thunderbird_import_card(cx));
        if !self
            .account_root
            .store()
            .list(&accounts::person::TILO_IDENTITY_TYPE.into())
            .is_empty()
        {
            panel.push(self.browser_identity_card(cx));
        }

        provider_settings_ui::widgets::page(panel)
    }

    fn browser_service_sessions(&self) -> Vec<accounts::Account> {
        if self.is_sample {
            return Vec::new();
        }
        let Some(root) = self.account_root.path() else {
            return Vec::new();
        };
        // Every publication asks for them: read the file again only when it
        // changed, not on each click and keystroke.
        let stamp = std::fs::metadata(accounts::store_path_in(&root))
            .and_then(|meta| meta.modified())
            .ok();
        if let Some((seen, sessions)) = &*self.sessions_read.borrow()
            && stamp.is_some()
            && *seen == stamp
        {
            return sessions.clone();
        }
        let sessions: Vec<accounts::Account> = accounts::AccountStore::load_in(&root)
            .list(&browser_sessions::TYPE.into())
            .into_iter()
            .cloned()
            .collect();
        *self.sessions_read.borrow_mut() = Some((stamp, sessions.clone()));
        sessions
    }

    fn browser_service_sessions_card(&self, cx: &ViewCx<Settings>) -> SettingsSection {
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
                Box::new(Self::bound(
                    cx,
                    Button::new(
                        &format!("settings.browser-session.remove:{id}"),
                        &t("accounts-browser-sessions-remove"),
                    )
                    .with_enabled(!self.browser_removal_pending),
                    AccountsEffect::RemoveBrowserServiceSession {
                        session_id: id.to_owned(),
                    },
                )),
            ));
        }
        section(&t("accounts-browser-sessions-title"), rows(body))
    }

    fn linked_accounts_card(&self, cx: &ViewCx<Settings>) -> Option<SettingsSection> {
        let accounts = self.linked.as_ref()?.accounts();
        if accounts.is_empty() {
            return None;
        }
        let mut body = vec![note(&t("accounts-linked-about"))];
        for account in accounts {
            let service = || account.service.clone().into();
            let app = account.app.clone();
            body.push(match &account.state {
                ui::linked_accounts::LinkState::Unlinked { connect, elsewhere } => {
                    let button = connect.then(|| -> Box<dyn Widget> {
                        Box::new(Self::bound(
                            cx,
                            Button::new(
                                &format!("settings.linked.link:{app}"),
                                &t_args("accounts-linked-link", &[("service", service())]),
                            )
                            .with_icon(ui::linked_accounts::service_icon(&app))
                            .role(ui::widget::ButtonRole::Primary),
                            AccountsEffect::LinkService { app: app.clone() },
                        ))
                    });
                    match (button, elsewhere) {
                        (Some(button), Some(elsewhere)) => {
                            setting_row_described(&account.service, elsewhere, button)
                        }
                        (Some(button), None) => setting_row(&account.service, button),
                        (None, elsewhere) => note(elsewhere.as_deref().unwrap_or_default()),
                    }
                }
                ui::linked_accounts::LinkState::Connecting => note(&t_args(
                    "accounts-linked-connecting",
                    &[("service", service())],
                )),
                ui::linked_accounts::LinkState::Looking => note(&t_args(
                    "accounts-linked-looking",
                    &[("service", service())],
                )),
                ui::linked_accounts::LinkState::Offered(offer) => setting_row_described(
                    &account.service,
                    &t_args(
                        "accounts-linked-offer",
                        &[
                            ("service", service()),
                            ("account", offer.account.label().into()),
                        ],
                    ),
                    Box::new(
                        Row::new()
                            .no_wrap()
                            .with_rhythm(ui::tokens::Rhythm::Sibling)
                            .with_align_items(taffy::AlignItems::Center)
                            .with_justify(taffy::JustifyContent::FlexEnd)
                            .push(Box::new(Self::bound(
                                cx,
                                Button::new(
                                    &format!("settings.linked.decline:{app}"),
                                    &t("accounts-linked-decline"),
                                ),
                                AccountsEffect::DeclineService { app: app.clone() },
                            )))
                            .push(Box::new(Self::bound(
                                cx,
                                Button::new(
                                    &format!("settings.linked.accept:{app}"),
                                    &t("accounts-linked-accept"),
                                )
                                .with_icon(ui::linked_accounts::service_icon(&app))
                                .role(ui::widget::ButtonRole::Primary),
                                AccountsEffect::AcceptService { app: app.clone() },
                            ))),
                    ),
                ),
                ui::linked_accounts::LinkState::Linked(name) => setting_row_described(
                    &account.service,
                    &t_args("accounts-linked-as", &[("account", name.label().into())]),
                    Box::new(Self::bound(
                        cx,
                        Button::new(
                            &format!("settings.linked.unlink:{app}"),
                            &t("accounts-linked-unlink"),
                        ),
                        AccountsEffect::UnlinkService { app },
                    )),
                ),
            });
        }
        Some(section(&t("accounts-linked-title"), rows(body)))
    }

    fn browser_identity_card(&self, cx: &ViewCx<Settings>) -> SettingsSection {
        let store = self.account_root.store();
        let mut body = vec![note(&t("accounts-browser-identity-about"))];
        for account in store.list(&accounts::person::TILO_IDENTITY_TYPE.into()) {
            let id = account.id.as_str().to_owned();
            let toggle = if account.enabled {
                "accounts-browser-identity-hide"
            } else {
                "accounts-browser-identity-show"
            };
            let menu = RowMenu::new(
                format!("settings.browser-identity.more:{id}"),
                &t_args(
                    "settings-row-more",
                    &[("name", account.display_name.clone().into())],
                ),
            )
            .action(
                t(toggle),
                Self::row_effect_binding(
                    cx,
                    AccountsEffect::SetBrowserIdentityEnabled {
                        account: id.clone(),
                        enabled: !account.enabled,
                    },
                ),
            )
            .destructive(
                t("accounts-browser-identity-remove"),
                Self::row_effect_binding(
                    cx,
                    AccountsEffect::RemoveBrowserIdentity {
                        account: id.clone(),
                    },
                ),
            );
            let origin: Vec<String> = account
                .config
                .get("origin")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .into_iter()
                .collect();
            body.push(row(Box::new(
                thing_row(
                    &format!("settings.browser-identity:{id}"),
                    &account.display_name,
                    &origin,
                    Some(Box::new(menu)),
                )
                .icon(ui::icons::Icon::Globe),
            )));
        }
        section(&t("accounts-browser-identity-title"), rows(body))
    }

    fn add_account_card(cx: &ViewCx<Settings>) -> SettingsSection {
        let providers = AccountKind::ALL
            .into_iter()
            .map(|kind| {
                let effect = AccountsEffect::AddAccount {
                    kind: kind.slug().into(),
                };
                let mut provider = ui::widget::ListRow::new(
                    format!("settings.accounts.add.{}", kind.slug()),
                    t_args(
                        "accounts-add-provider",
                        &[("kind", kind_shown(kind).into())],
                    ),
                )
                .icon(kind_icon(kind))
                .disclosure();
                provider = match Self::row_effect_binding(cx, effect) {
                    Some(binding) => provider.on_action(binding),
                    None => provider.with_enabled(false),
                };
                row(Box::new(provider))
            })
            .collect();
        section(&t("accounts-add"), rows(providers))
    }

    fn mail_card(&self, cx: &ViewCx<Settings>) -> Option<SettingsSection> {
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
        (!views.is_empty()).then(|| section(&title, self.kind_body(cx, AccountKind::Mail, &views)))
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

    fn kind_card(&self, cx: &ViewCx<Settings>, kind: AccountKind) -> SettingsSection {
        let views = self.dir.views_of(kind);
        section(&kind_shown(kind), self.kind_body(cx, kind, &views))
    }

    fn kind_body(&self, cx: &ViewCx<Settings>, kind: AccountKind, views: &[AccountView]) -> Body {
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
                    let mut items = vec![self.account_row(cx, view)];
                    if let Some(mail) = self.linked_google_mail(view) {
                        items.push(self.account_row(cx, &mail));
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

    fn files_handoff(&self, view: &AccountView) -> Option<ui::tilo_actions::ActionSpec> {
        if !self.may_hand_off_files(view) {
            return None;
        }
        self.files_handoff_in(&self.account_root.store(), view)
    }

    fn may_hand_off_files(&self, view: &AccountView) -> bool {
        !self.is_sample
            && view.enabled
            && (view.kind.is_cloud_storage()
                || matches!(view.kind, AccountKind::WebDav | AccountKind::Nextcloud))
    }

    fn files_handoff_in(
        &self,
        store: &accounts::AccountStore,
        view: &AccountView,
    ) -> Option<ui::tilo_actions::ActionSpec> {
        use accounts::cloud_types::FilesMode;
        if !self.may_hand_off_files(view) {
            return None;
        }
        let account = store.get(&view.id.clone().into())?;
        if account
            .config
            .get("use_files")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
            || account.config.get("pending_reconnect") == Some(&serde_json::Value::Bool(true))
        {
            return None;
        }
        let label = match accounts::cloud_types::files_mode(account)? {
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
        let resource = LiveRow::new(
            format!("settings.account:{}", view.id),
            "settings.account",
            &view.label,
        )
        .operation(spec)
        .into_resource();
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

    fn account_row(&self, cx: &ViewCx<Settings>, view: &AccountView) -> Item {
        let id = accounts::AccountId::from(view.id.clone());
        let runtime = self.sync_statuses.get(&id);
        let mount = self.cloud.mount(&view.id);
        let pending = self.dir.of(AccountKind::Mail).any(|account| {
            account.id.as_str() == view.id
                && (account.type_id.as_str() == accounts::thunderbird::PENDING_TYPE
                    || account.config.get("import_pending") == Some(&serde_json::Value::Bool(true)))
        }) || self.account_root.store().get(&id).is_some_and(|account| {
            account.config.get("pending_reconnect") == Some(&serde_json::Value::Bool(true))
        });
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
            view.kind == AccountKind::Mail && self.mail_grants.granted.contains(&view.id);
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

        let mut tags: Vec<(String, StatusTone)> = Vec::new();
        if view.kind.best_effort() {
            tags.push((t("accounts-best-effort"), StatusTone::Neutral));
        }
        tags.push((status, status_tone));

        let mut details = vec![view.summary.clone()];
        let files = self.files_button(view, &mut details);
        let mut entries: Vec<AccountEntry> = Vec::new();
        if !self.is_sample {
            details.push(sync_detail(runtime, import_only, granted_mail));
            entries.extend(self.cloud_row_entries(
                view,
                mount.map(|mount| mount.paused),
                &mut details,
            ));
            entries.extend(Self::account_maintenance_entries(view));
        }
        let label = self.account_row_label(view, &mut details);
        let menu = (!entries.is_empty()).then(|| {
            entries.into_iter().fold(
                RowMenu::new(
                    format!("settings.accounts.more.{}", view.id),
                    &t_args("settings-row-more", &[("name", label.clone().into())]),
                ),
                |menu, entry| {
                    let binding = Self::row_effect_binding(cx, entry.effect);
                    if entry.destructive {
                        menu.destructive(entry.label, binding)
                    } else {
                        menu.action(entry.label, binding)
                    }
                },
            )
        });
        let trailing = account_trailing(files, menu);
        let tags: Vec<(&str, ui::widget::ChipStyle)> = tags
            .iter()
            .map(|(text, tone)| (text.as_str(), ui::widget::ChipStyle::Tinted(*tone)))
            .collect();
        row(Box::new(
            thing_row(
                &format!("settings.accounts.account.{}", view.id),
                &label,
                &details,
                trailing,
            )
            .icon(kind_icon(view.kind))
            .tags(&tags),
        ))
    }

    /// What a row offers for the account's calendars and synced files:
    /// Nextcloud's calendars, then pausing or resuming a mounted account's
    /// sync (`paused` is `None` without a mount), or recovering an account
    /// whose files mode was lost, which says so in `details`.
    fn cloud_row_entries(
        &self,
        view: &AccountView,
        paused: Option<bool>,
        details: &mut Vec<String>,
    ) -> Vec<AccountEntry> {
        let mut entries = Vec::new();
        if view.kind == AccountKind::Nextcloud {
            entries.push(AccountEntry::new(
                t("accounts-nextcloud-calendars"),
                AccountsEffect::ConnectNextcloudCalendars {
                    account: view.id.clone(),
                },
            ));
        }
        if let Some(paused) = paused {
            entries.extend(sync_entries(&view.id, paused));
        } else if self.can_recover_cloud_mode(view) {
            details.push(t("accounts-recover-mode-description"));
            entries.push(AccountEntry::new(
                t("accounts-recover-mode"),
                AccountsEffect::RecoverCloudMode {
                    account: view.id.clone(),
                },
            ));
        }
        entries
    }

    fn account_row_label(&self, view: &AccountView, details: &mut Vec<String>) -> String {
        // Keep the service name on the primary line and the account identity
        // on a secondary line, rather than joining metadata into the label.
        // The configured service identifies Gmail, never an email-domain guess.
        let service = if view.kind == AccountKind::GoogleDrive {
            Some(t("accounts-google-files-service"))
        } else if view.kind == AccountKind::Mail
            && self.dir.mail_config(&view.id).is_some_and(|mail| {
                mail.auth == accounts::email::AuthMethod::OAuth
                    && mail.imap_host == "imap.gmail.com"
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

    /// What the account's menu offers to keep it working: edit it, sign in
    /// again, check it, and remove it. Each entry runs the account row's own
    /// action, so the host admits it as it does a button.
    fn account_maintenance_entries(view: &AccountView) -> Vec<AccountEntry> {
        let mut actions = Vec::new();
        let reauth_label = t("accounts-reauth");
        let edit = t("accounts-edit");
        let check = if view.kind == AccountKind::Mail {
            t("accounts-check-server")
        } else {
            t("accounts-verify")
        };
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
            actions.push(AccountEntry::new(
                edit,
                AccountsEffect::EditAccount {
                    account: view.id.clone(),
                },
            ));
        }
        actions.push(AccountEntry::new(
            reauth_label,
            AccountsEffect::SignInAgain {
                account: view.id.clone(),
            },
        ));
        actions.push(AccountEntry::new(
            check,
            AccountsEffect::TestAccount {
                account: view.id.clone(),
            },
        ));
        actions.push(AccountEntry::destructive(
            remove,
            AccountsEffect::RemoveAccount {
                account: view.id.clone(),
            },
        ));
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

    #[allow(
        clippy::too_many_lines,
        reason = "the form declares every credential field and accessibility label in one reviewable surface"
    )]
    fn account_form(&self, cx: &ViewCx<Settings>, kind: AccountKind) -> SettingsSection {
        if !self.calendar_choices.is_empty() || self.calendar_source.is_some() {
            return self.calendar_selection_form(cx);
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
            .push(Box::new(Self::bound(
                cx,
                Button::new("settings.accounts.form.save", &save),
                AccountsEffect::SaveAccountForm,
            )))
            .push(Box::new(Self::bound(
                cx,
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
        section(
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
                fields.push(buttons);
                fields
            }),
        )
    }

    fn reauth_form(&self, cx: &ViewCx<Settings>, id: &str) -> SettingsSection {
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
                .push(Box::new(Self::bound(
                    cx,
                    Button::new("settings.accounts.reauth.save", &save),
                    AccountsEffect::SaveNewPassword,
                )))
                .push(Box::new(Self::bound(
                    cx,
                    Button::new("settings.accounts.reauth.cancel", &cancel),
                    AccountsEffect::CancelNewPassword,
                )))
                .push(Box::new(Spacer::new())),
        )));
        section(&t("accounts-reauth-title"), rows(fields))
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

    fn reopen_vault(&mut self) {
        let vault = self.account_root.vault_opener();
        self.vault_opening = Some(crate::device_read::Landing::start(move || vault.open()));
    }

    fn poll_vault_opening(&mut self) {
        let Some(opening) = &self.vault_opening else {
            return;
        };
        match opening.poll::<accounts::SecretVault>() {
            crate::device_read::Landed::Done(vault) => {
                self.vault = Some(vault);
                self.vault_opening = None;
                self.mail_grants.asked = None;
                self.changed = true;
            }
            crate::device_read::Landed::Pending => {}
            crate::device_read::Landed::Lost => self.vault_opening = None,
        }
    }

    fn poll_mail_grants(&mut self) {
        if let Some(reading) = &self.mail_grants.reading {
            match reading.poll::<std::collections::BTreeSet<String>>() {
                crate::device_read::Landed::Done(granted) => {
                    self.changed |= granted != self.mail_grants.granted;
                    self.mail_grants.granted = granted;
                    self.mail_grants.reading = None;
                }
                crate::device_read::Landed::Pending => return,
                crate::device_read::Landed::Lost => self.mail_grants.reading = None,
            }
        }
        if self.is_sample {
            return;
        }
        let ids: Vec<String> = self
            .dir
            .views_of(AccountKind::Mail)
            .into_iter()
            .map(|view| view.id)
            .collect();
        if self.mail_grants.asked.as_ref() == Some(&ids) {
            return;
        }
        if ids.is_empty() {
            self.changed |= !self.mail_grants.granted.is_empty();
            self.mail_grants.granted.clear();
            self.mail_grants.asked = Some(ids);
            return;
        }
        self.mail_grants.asked = Some(ids.clone());
        let vault = self.account_root.vault_opener();
        self.mail_grants.reading = Some(crate::device_read::Landing::start(move || {
            vault.oauth_granted(ids)
        }));
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
            if parsed.scheme() != "https" && !crate::account_probe::local_http_allowed(&parsed) {
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

    fn commit_metadata(
        &mut self,
        stored: Vec<accounts::Account>,
        previous: PreviousSecrets,
        settle: Settle,
    ) -> Step {
        let before = self.account_root.store();
        let mut shared = before.clone();
        for account in stored {
            shared.upsert(account);
        }
        let Err(error) = self.account_root.save_store(&shared) else {
            let finished = finish_or_recover(&self.account_root, "account save");
            return settle(self, finished);
        };
        let metadata_rollback = self.account_root.save_store(&before);
        warn!(target: "tilo::settings", op = "account_store_save", %error, "failed to persist account metadata");
        let failed = format!("could not persist account: {error}");
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

    fn removal_metadata(&mut self, removal: Removal, settle: Settle) -> Step {
        let root = &self.account_root;
        let mut updated_store = root.store();
        updated_store.remove(&removal.id);
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
    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        let outcome = Rc::new(std::cell::RefCell::new(None));
        let landed = Rc::clone(&outcome);
        self.remove_then(id, move |result| *landed.borrow_mut() = Some(result));
        self.settle_lane();
        outcome
            .take()
            .unwrap_or_else(|| Err("the removal stopped unexpectedly".into()))
    }

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
        self.poll_mail_grants();
        self.poll_form_probe();
        self.poll_sign_in();
        self.poll_reauth_probe();
        if !self.is_sample && self.cloud.poll() {
            self.changed = true;
        }
        std::mem::take(&mut self.changed)
    }
}

impl AccountsPane {
    pub fn busy(&self) -> bool {
        self.form_probe.is_some()
            || self.reauth_probe.is_some()
            || !self.lane.is_idle()
            || self.vault_opening.is_some()
            || self.mail_grants.reading.is_some()
            || self
                .sign_in
                .as_ref()
                .is_some_and(cloud_signin::SignInFlow::busy)
    }
}

#[derive(Default)]
struct MailGrants {
    granted: std::collections::BTreeSet<String>,
    asked: Option<Vec<String>>,
    reading: Option<crate::device_read::Landing>,
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

fn account_trailing(files: Option<Button>, menu: Option<RowMenu>) -> Option<El> {
    match (files, menu) {
        (Some(files), Some(menu)) => Some(Box::new(
            Row::new()
                .no_wrap()
                .with_rhythm(ui::tokens::Rhythm::Related)
                .with_align_items(taffy::AlignItems::Center)
                .push(Box::new(files.control_size(ui::widget::ControlSize::Small)))
                .push(Box::new(menu)),
        )),
        (Some(files), None) => Some(Box::new(files.control_size(ui::widget::ControlSize::Small))),
        (None, Some(menu)) => Some(Box::new(menu)),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests;
