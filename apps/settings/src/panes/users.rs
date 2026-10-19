//! Local login accounts and Tilo signing identities.
//!
//! A Unix login account owns a home/session; it may contain several Tilo
//! identities (personal, work, pseudonymous). Local-account mutations go
//! through `AccountsService` and polkit. The identity store is the host's:
//! Settings lists, activates, removes and protects identities through the
//! host's identity actions, so no record or secret enters this process.
//! Every change is a [`UsersEffect`] on the account or identity it names.

use std::cell::OnceCell;
use std::collections::HashSet;
use std::fs;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use runtime_identity::{IdentitySummary, KeyProtection};
use ui::framework::{EffectAnswer, EffectCx, EffectReply, Effected, LiveRow};
use ui::icons::Icon;
use ui::tilo_resource::{self, Effect};
use ui::widget::{
    Button, ButtonRole, ChipStyle, Column, DisclosureRow, DisclosureState, Row, RowMenu, TextInput,
    TextInputKind, Widget,
};
use zeroize::Zeroizing;

use crate::effects::{SettingsEffect, effect};
use crate::host::PaneCx;
use crate::i18n::{t, t_args};
use crate::panes::model::{PaneModel, Shared};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    Item, SettingsSection, StatusTone, block, error_caption, field_row, note, row, rows, section,
    thing_row, untitled,
};

/// Changes to this device's login accounts and the person's identities.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum UsersEffect {
    /// Create a login account, with the password typed into the form.
    #[effect(job, label = "Create account")]
    CreateUser {
        #[param("The login name")]
        username: String,
        #[param("The person's full name")]
        full_name: String,
    },
    /// Make an account an administrator, or a standard account.
    #[effect(job, row, idempotent)]
    SetAdministrator {
        #[param("The account's login name")]
        user: String,
        #[param("Whether the account administers this device")]
        admin: bool,
    },
    /// Sign an account in without its password when the device starts.
    #[effect(job, row, idempotent)]
    SetAutomaticLogin {
        #[param("The account's login name")]
        user: String,
        on: bool,
    },
    /// Remove an account, keeping its files or removing them too.
    #[effect(job, row, destructive, label = "Remove account")]
    RemoveUser {
        #[param("The account's login name")]
        user: String,
        #[param("Whether the account's home folder stays")]
        keep_files: bool,
    },
    /// Sign new apps in as one of the person's identities.
    #[effect(job, row, idempotent, label = "Use identity")]
    UseIdentity {
        #[param("The identity's id")]
        identity: String,
    },
    /// Remove an identity from this device.
    #[effect(job, row, destructive, label = "Remove identity")]
    RemoveIdentity {
        #[param("The identity's id")]
        identity: String,
    },
    /// Move an identity's key from a plain file into protected storage.
    #[effect(job, row, idempotent, label = "Protect identity")]
    ProtectIdentity {
        #[param("The identity's id")]
        identity: String,
    },
}

/// Answer an account change on a worker, then read the accounts again on
/// it: both may wait on `AccountsService`, so the window takes in the
/// outcome and the fresh accounts together when the job ends. An identity
/// change answers once the host has made it.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: UsersEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let model = app.users().model();
    // Identity changes are the host's: they answer once it has.
    let change = model.borrow().start_identity_change(&effect);
    if let Some(call) = change {
        model
            .borrow_mut()
            .identity_changes
            .push((call?, cx.reply()));
        return Ok(Effected::Pending);
    }
    let model = model.borrow();
    let backends = model.backends.clone();
    let users = model.users.clone();
    let password = match &effect {
        UsersEffect::CreateUser { .. } => Zeroizing::new(
            model
                .form
                .password
                .lock()
                .map(|mut value| std::mem::take(&mut *value))
                .unwrap_or_default(),
        ),
        _ => Zeroizing::new(String::new()),
    };
    let created = matches!(effect, UsersEffect::CreateUser { .. });
    Ok(crate::device_read::job(
        cx,
        move || {
            let outcome = carry_out(&backends, &users, &effect, &password);
            (outcome, Reread::of(&backends))
        },
        move |app: &mut SettingsApp, (outcome, reread): (Result<(), String>, Option<Reread>), _| {
            let model = app.users().model();
            let mut model = model.borrow_mut();
            if created && outcome.is_ok() {
                model.form.clear();
            }
            model.error = outcome.as_ref().err().cloned();
            model.adopt(reread);
            outcome.map(|()| Effected::done())
        },
    ))
}

/// The service that changes accounts.
#[derive(Clone)]
struct Backends {
    users: Arc<dyn UserAdmin>,
}

/// Carry out the account change `effect`. Never called on the UI thread:
/// `AccountsService` may wait on polkit. Identity changes are the host's
/// ([`UsersModel::start_identity_change`]).
fn carry_out(
    backends: &Backends,
    users: &[DeviceUser],
    effect: &UsersEffect,
    password: &str,
) -> Result<(), String> {
    let other = |name: &str| {
        users
            .iter()
            .find(|user| user.username == name && !user.current)
            .ok_or_else(|| t("settings-users-account-gone"))
    };
    match effect {
        UsersEffect::CreateUser {
            username,
            full_name,
        } => {
            let mut bytes = Zeroizing::new(password.as_bytes().to_vec());
            backends
                .users
                .create(username.trim(), full_name.trim(), &mut bytes)
        }
        UsersEffect::SetAdministrator { user, admin } => {
            let user = other(user)?;
            if user.admin == *admin {
                return Ok(());
            }
            backends.users.set_admin(&user.username, *admin)
        }
        UsersEffect::SetAutomaticLogin { user, on } => {
            let user = other(user)?;
            if user.automatic_login == *on {
                return Ok(());
            }
            backends.users.set_autologin(&user.username, *on)
        }
        UsersEffect::RemoveUser { user, keep_files } => {
            backends.users.delete(other(user)?, !keep_files)
        }
        UsersEffect::UseIdentity { .. }
        | UsersEffect::RemoveIdentity { .. }
        | UsersEffect::ProtectIdentity { .. } => {
            Err(t("settings-users-identity-store-unavailable"))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceUser {
    pub username: String,
    pub full_name: String,
    pub uid: u64,
    pub admin: bool,
    pub current: bool,
    pub automatic_login: bool,
    /// Why `automatic_login` could not be read; it is then not offered.
    pub automatic_login_error: Option<String>,
}

pub trait UserAdmin: Send + Sync {
    /// # Errors
    ///
    /// Returns the backend's error message if account creation is rejected
    /// (e.g. the username already exists or polkit authorization is denied).
    fn create(&self, username: &str, full_name: &str, password: &mut [u8]) -> Result<(), String>;

    /// # Errors
    ///
    /// Returns the backend's error message if the account (and its files,
    /// when `remove_files` is set) cannot be deleted.
    fn delete(&self, user: &DeviceUser, remove_files: bool) -> Result<(), String>;

    /// # Errors
    ///
    /// Returns the backend's error message if changing the account's
    /// administrator role is rejected.
    fn set_admin(&self, username: &str, admin: bool) -> Result<(), String>;

    /// # Errors
    ///
    /// Returns the backend's error message if enabling or disabling
    /// automatic login for the account is rejected.
    fn set_autologin(&self, username: &str, enabled: bool) -> Result<(), String>;

    /// The device's login accounts as they are now, read again after a
    /// change, or why they could not be read. `None` when the accounts
    /// shown are not the device's (a screenshot's samples): nothing is
    /// read.
    fn accounts(&self) -> Option<Result<Vec<DeviceUser>, String>>;
}

struct AccountsServiceAdmin;

impl UserAdmin for AccountsServiceAdmin {
    fn create(&self, username: &str, full_name: &str, password: &mut [u8]) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        os_actions::users::create_user(username, full_name, os_actions::AccountRole::Standard)?;
        // Never destructively roll back a newly created home if credential
        // setup fails; it may already contain PAM/AccountsService work.
        os_actions::users::set_password(username, password)
    }

    fn delete(&self, user: &DeviceUser, remove_files: bool) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        os_actions::users::delete_user(&user.username, user.uid, remove_files)
    }

    fn set_admin(&self, username: &str, admin: bool) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        os_actions::users::set_role(
            username,
            if admin {
                os_actions::AccountRole::Administrator
            } else {
                os_actions::AccountRole::Standard
            },
        )
    }

    fn set_autologin(&self, username: &str, enabled: bool) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        os_actions::users::set_automatic_login(username, enabled)
    }

    fn accounts(&self) -> Option<Result<Vec<DeviceUser>, String>> {
        Some(read_users())
    }
}

/// A call to the host's identity store, answering on its state.
type IdentityCall = ui::LocalState<ui::InvocationCall>;

/// What lists and changes the person's identities. Each call answers on
/// the state it returns; nothing here waits.
trait IdentityAdmin {
    fn list(&self) -> IdentityCall;
    fn activate(&self, id: &str) -> IdentityCall;
    fn remove(&self, id: &str) -> IdentityCall;
    fn protect(&self, id: &str) -> IdentityCall;
}

/// The host's identity actions, invoked through the action door as the
/// person using Settings. The host checks that they administer the
/// identity; each change carries their confirmation, given by choosing its
/// row or button.
struct RemoteIdentityAdmin {
    remote: ui::RemoteActions,
}

impl RemoteIdentityAdmin {
    fn invoke(&self, step: &str, id: Option<&str>) -> IdentityCall {
        use ui::tilo_actions::remote::{Confirmation, InvokeArgument, identity_admin};
        let mut arguments = serde_json::Map::new();
        if let Some(id) = id {
            arguments.insert("id".into(), id.into());
        }
        // Choosing the row or button is the person's confirmation of a
        // change; listing asks nothing.
        let confirmation = id.is_some().then(|| {
            arguments.insert("confirm".into(), true.into());
            Confirmation::Confirmed
        });
        self.remote.invoke(
            identity_admin::target(step),
            if arguments.is_empty() {
                InvokeArgument::None
            } else {
                InvokeArgument::Json(arguments)
            },
            confirmation,
        )
    }
}

impl IdentityAdmin for RemoteIdentityAdmin {
    fn list(&self) -> IdentityCall {
        self.invoke(ui::tilo_actions::remote::identity_admin::LIST, None)
    }
    fn activate(&self, id: &str) -> IdentityCall {
        self.invoke(ui::tilo_actions::remote::identity_admin::ACTIVATE, Some(id))
    }
    fn remove(&self, id: &str) -> IdentityCall {
        self.invoke(ui::tilo_actions::remote::identity_admin::REMOVE, Some(id))
    }
    fn protect(&self, id: &str) -> IdentityCall {
        self.invoke(ui::tilo_actions::remote::identity_admin::PROTECT, Some(id))
    }
}

/// The identities the host's list answered.
fn identities_from(value: Option<serde_json::Value>) -> Result<Vec<IdentitySummary>, String> {
    serde_json::from_value(value.unwrap_or(serde_json::Value::Array(Vec::new())))
        .map_err(|error| format!("the identity list the host answered is not readable: {error}"))
}

pub fn admins_from_group(contents: &str) -> HashSet<String> {
    contents
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() < 4 || !matches!(f.first(), Some(&("wheel" | "sudo"))) {
                return None;
            }
            f.get(3).copied()
        })
        .flat_map(|members| members.split(','))
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .collect()
}

fn read_passwd_users(
    contents: &str,
    current_user: Option<&str>,
    admins: &HashSet<String>,
) -> Vec<DeviceUser> {
    contents
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() < 7 {
                return None;
            }
            let username = *f.first()?;
            let uid = f.get(2)?.parse::<u64>().ok()?;
            let gecos = *f.get(4)?;
            let shell = *f.get(6)?;
            if !(1000..60000).contains(&uid)
                || shell.ends_with("nologin")
                || shell.ends_with("false")
            {
                return None;
            }
            let full = gecos.split(',').next().unwrap_or("").trim();
            Some(DeviceUser {
                username: username.to_string(),
                full_name: if full.is_empty() { username } else { full }.to_string(),
                uid,
                admin: admins.contains(username),
                current: current_user == Some(username),
                automatic_login: false,
                automatic_login_error: None,
            })
        })
        .collect()
}

/// The device's login accounts, or why they could not be read: one
/// `AccountsService` call per person, so never on the UI thread.
fn read_users() -> Result<Vec<DeviceUser>, String> {
    crate::device_read::off_ui_thread();
    let current = std::env::var("USER").ok();
    let admins = fs::read_to_string("/etc/group")
        .map(|s| admins_from_group(&s))
        .map_err(|error| format!("/etc/group: {error}"))?;
    let mut users = fs::read_to_string("/etc/passwd")
        .map(|s| read_passwd_users(&s, current.as_deref(), &admins))
        .map_err(|error| format!("/etc/passwd: {error}"))?;
    for user in &mut users {
        match os_actions::users::automatic_login(&user.username) {
            Ok(enabled) => user.automatic_login = enabled,
            Err(error) => user.automatic_login_error = Some(error),
        }
    }
    Ok(users)
}

/// Split a read into the accounts it found and why it found none.
fn users_and_error(read: Result<Vec<DeviceUser>, String>) -> (Vec<DeviceUser>, Option<String>) {
    match read {
        Ok(users) => (users, None),
        Err(error) => (Vec::new(), Some(error)),
    }
}

/// What the users area keeps for the window: the model, read when first
/// needed.
pub struct UsersArea {
    cx: PaneCx,
    /// The window's client for the action door, shared with the other
    /// areas that act through the host.
    remote: ui::RemoteActions,
    model: OnceCell<Shared<UsersModel>>,
    /// The device's accounts, read off the UI thread after the window's
    /// first frame ([`Self::start_read`]); `None` stands for the samples a
    /// screenshot shows.
    read: crate::device_read::DeviceRead<Option<UsersRead>>,
}

impl UsersArea {
    pub fn new(cx: &PaneCx, remote: ui::RemoteActions) -> Self {
        Self {
            cx: cx.clone(),
            remote,
            model: OnceCell::new(),
            read: crate::device_read::DeviceRead::default(),
        }
    }

    /// The model: this device's accounts, or sample ones for a screenshot.
    pub fn model(&self) -> &Shared<UsersModel> {
        self.model.get_or_init(|| {
            let read = self
                .read
                .take()
                .unwrap_or_else(|| self.cx.reaches_devices().then(UsersRead::of_device));
            let model = match read {
                Some(read) => UsersModel::from_read(
                    self.cx.clone(),
                    read,
                    Rc::new(RemoteIdentityAdmin {
                        remote: self.remote.clone(),
                    }),
                ),
                None => UsersModel::sample(self.cx.clone()),
            };
            Rc::new(std::cell::RefCell::new(model))
        })
    }

    /// Take in what the host answered about the person's identities, if
    /// the model exists.
    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get() {
            model.borrow_mut().poll_identities();
        }
    }

    /// Read the device's accounts off the UI thread; `false` when they have
    /// been read or are being read.
    pub(crate) fn start_read(&self) -> bool {
        if self.is_open() {
            return false;
        }
        let device = self.cx.reaches_devices();
        self.read.start(move || device.then(UsersRead::of_device))
    }

    /// Whether the read [`Self::start_read`] started has ended.
    pub(crate) fn read_finished(&self) -> bool {
        self.read.is_finished()
    }

    /// Whether the area has read what its rows show: its pane was shown,
    /// an effect or a row lookup reached it, or the window read it after
    /// its first frame ([`crate::effects::LIVE_AREAS`]).
    pub(crate) fn is_open(&self) -> bool {
        self.model.get().is_some()
    }

    /// One row per account other than the signed-in one, and per identity.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let model = self.model().borrow();
        let accounts = model
            .users
            .iter()
            .filter(|user| !user.current)
            .map(user_live_row);
        let identities = model.identities.iter().map(identity_live_row);
        accounts.chain(identities).collect()
    }
}

/// An account other than the signed-in one as a row of the published
/// surface, offering what can be done to it now.
fn user_live_row(user: &DeviceUser) -> LiveRow {
    let name = user.username.clone();
    let mut row = LiveRow::new(
        format!("settings.user:{name}"),
        "settings.user",
        user.full_name.clone(),
    )
    .operation(
        SettingsEffect::from(UsersEffect::SetAdministrator {
            user: name.clone(),
            admin: !user.admin,
        })
        .bound(),
    );
    // An unread automatic-login state offers no switch to flip.
    if user.automatic_login_error.is_none() {
        row = row.operation(
            SettingsEffect::from(UsersEffect::SetAutomaticLogin {
                user: name.clone(),
                on: !user.automatic_login,
            })
            .bound(),
        );
    }
    row.operation(
        SettingsEffect::from(UsersEffect::RemoveUser {
            user: name,
            keep_files: true,
        })
        .bound(),
    )
}

/// An identity as a row of the published surface.
fn identity_live_row(identity: &IdentitySummary) -> LiveRow {
    let id = identity.id.clone();
    let mut row = LiveRow::new(
        format!("settings.identity:{id}"),
        "settings.identity",
        identity_name(identity).to_owned(),
    );
    if !identity.active {
        row = row
            .operation(
                SettingsEffect::from(UsersEffect::UseIdentity {
                    identity: id.clone(),
                })
                .bound(),
            )
            .operation(
                SettingsEffect::from(UsersEffect::RemoveIdentity {
                    identity: id.clone(),
                })
                .bound(),
            );
    }
    if identity.protection == KeyProtection::LegacyPlaintext {
        row = row
            .operation(SettingsEffect::from(UsersEffect::ProtectIdentity { identity: id }).bound());
    }
    row
}

/// How an identity's key is kept, in the person's words.
fn protection_label(protection: KeyProtection) -> String {
    t(match protection {
        KeyProtection::SecretService => "settings-users-protection-service",
        KeyProtection::BrokerFile => "settings-users-protection-file",
        KeyProtection::HardwareBacked => "settings-users-protection-hardware-backed",
        KeyProtection::LegacyPlaintext => "settings-users-protection-legacy",
        KeyProtection::DeviceCertificate => "settings-users-protection-device-certificate",
        KeyProtection::Unavailable => "settings-users-protection-unavailable",
    })
}

/// What an identity is called: its display name, else its id.
fn identity_name(identity: &IdentitySummary) -> &str {
    if identity.display_name.is_empty() {
        &identity.id
    } else {
        &identity.display_name
    }
}

/// What the new-account form holds. Process-local only: none of it enters
/// the settings, presence, undo history or diagnostics.
#[derive(Default)]
struct AccountForm {
    full_name: Arc<Mutex<String>>,
    username: Arc<Mutex<String>>,
    password: Arc<Mutex<String>>,
}

impl AccountForm {
    fn clear(&self) {
        for field in [&self.full_name, &self.username, &self.password] {
            if let Ok(mut value) = field.lock() {
                zeroize::Zeroize::zeroize(&mut *value);
            }
        }
    }
}

pub struct UsersModel {
    cx: PaneCx,
    users: Vec<DeviceUser>,
    /// Why the accounts could not be read; the list is then not "empty".
    users_error: Option<String>,
    identities: Vec<IdentitySummary>,
    /// Why the host could not list the identities.
    identities_error: Option<String>,
    backends: Backends,
    /// The host's identity store, which lists and changes the identities.
    identity_admin: Option<Rc<dyn IdentityAdmin>>,
    /// The identity list the host is answering.
    identity_list: Option<IdentityCall>,
    /// The identity changes the host is making, and their callers.
    identity_changes: Vec<(IdentityCall, EffectReply)>,
    /// Whether the accounts are this device's, read again after a change.
    of_device: bool,
    form: AccountForm,
    /// Whether the new-account form is open, kept across rebuilds.
    add_open: DisclosureState,
    error: Option<String>,
    changed: bool,
}

/// What the users area reads of the device: its login accounts (one
/// `AccountsService` call per person for automatic login). Plain data, so
/// it is read off the UI thread; the identities are the host's to answer.
struct UsersRead {
    users: Result<Vec<DeviceUser>, String>,
}

impl UsersRead {
    fn of_device() -> Self {
        Self {
            users: read_users(),
        }
    }
}

/// The accounts read again after a change, on the job that made it. The
/// identities are the host's: they are asked for again once it answers.
struct Reread {
    users: Result<Vec<DeviceUser>, String>,
}

impl Reread {
    /// Read the accounts `backends` change; `None` when the accounts shown
    /// are not the device's.
    fn of(backends: &Backends) -> Option<Self> {
        let users = backends.users.accounts()?;
        Some(Self { users })
    }
}

impl UsersModel {
    /// The model over what `read` found on this device, asking
    /// `identity_admin` for the person's identities.
    fn from_read(cx: PaneCx, read: UsersRead, identity_admin: Rc<dyn IdentityAdmin>) -> Self {
        let (users, users_error) = users_and_error(read.users);
        Self {
            cx,
            users,
            users_error,
            identities: Vec::new(),
            identities_error: None,
            backends: Backends {
                users: Arc::new(AccountsServiceAdmin),
            },
            identity_list: Some(identity_admin.list()),
            identity_admin: Some(identity_admin),
            identity_changes: Vec::new(),
            of_device: true,
            form: AccountForm::default(),
            add_open: DisclosureState::new(),
            error: None,
            changed: false,
        }
    }

    /// Two accounts and one identity, changed by nothing.
    fn sample(cx: PaneCx) -> Self {
        let user = |username: &str, full_name: &str, uid, current| DeviceUser {
            username: username.into(),
            full_name: full_name.into(),
            uid,
            admin: current,
            current,
            automatic_login: false,
            automatic_login_error: None,
        };
        Self {
            cx,
            users: vec![
                user("ada", "Ada Lovelace", 1000, true),
                user("grace", "Grace Hopper", 1001, false),
            ],
            users_error: None,
            identities: vec![IdentitySummary {
                id: "personal".into(),
                display_name: "Personal".into(),
                public_key: [7; 32],
                threshold: 1,
                total_shares: 1,
                active: true,
                protection: KeyProtection::BrokerFile,
            }],
            identities_error: None,
            backends: Backends {
                users: Arc::new(SampleAdmin),
            },
            identity_admin: Some(Rc::new(SampleAdmin)),
            identity_list: None,
            identity_changes: Vec::new(),
            of_device: false,
            form: AccountForm::default(),
            add_open: DisclosureState::new(),
            error: None,
            changed: false,
        }
    }

    /// Take in what a read after a change found; `None` (a screenshot's
    /// samples) keeps what is shown.
    fn adopt(&mut self, reread: Option<Reread>) {
        self.changed = true;
        let Some(reread) = reread else {
            return;
        };
        (self.users, self.users_error) = users_and_error(reread.users);
        self.reload_identities();
    }

    /// Ask the host for the identities again, unless it is answering.
    fn reload_identities(&mut self) {
        if self.of_device && self.identity_list.is_none() {
            self.identity_list = self.identity_admin.as_ref().map(|admin| admin.list());
        }
    }

    /// Ask the host to make the identity change `effect`; `None` for an
    /// account change, an error when no identity store answers here.
    fn start_identity_change(&self, effect: &UsersEffect) -> Option<Result<IdentityCall, String>> {
        let admin = self.identity_admin.as_deref();
        let call = |start: &dyn Fn(&dyn IdentityAdmin) -> IdentityCall| {
            Some(
                admin
                    .map(start)
                    .ok_or_else(|| t("settings-users-identity-store-unavailable")),
            )
        };
        match effect {
            UsersEffect::UseIdentity { identity } => call(&|admin| admin.activate(identity)),
            UsersEffect::RemoveIdentity { identity } => call(&|admin| admin.remove(identity)),
            UsersEffect::ProtectIdentity { identity } => call(&|admin| admin.protect(identity)),
            _ => None,
        }
    }

    /// Take in the host's answers: the identity list, and each identity
    /// change, whose caller hears how it ended before the list is read
    /// again.
    fn poll_identities(&mut self) {
        if let Some(answer) = self
            .identity_list
            .as_ref()
            .and_then(crate::device_fleet::settled)
        {
            self.identity_list = None;
            match answer.and_then(|output| identities_from(output.value)) {
                Ok(identities) => {
                    self.identities = identities;
                    self.identities_error = None;
                }
                Err(error) => self.identities_error = Some(error),
            }
            self.changed = true;
        }
        let mut answered = false;
        for (call, reply) in std::mem::take(&mut self.identity_changes) {
            match crate::device_fleet::settled(&call) {
                None => self.identity_changes.push((call, reply)),
                Some(answer) => {
                    self.error = answer.as_ref().err().cloned();
                    reply.finish(answer);
                    answered = true;
                }
            }
        }
        if answered {
            self.changed = true;
            self.reload_identities();
        }
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let mut sections: Vec<SettingsSection> = Vec::new();
        if let Some(error) = &self.error {
            sections.push(section(
                &t("settings-users-error-title"),
                rows(vec![note(error)]),
            ));
        }
        let mut people: Vec<Item> = if let Some(error) = &self.users_error {
            vec![block(error_caption(&t_args(
                "settings-users-read-error",
                &[("error", error.as_str().into())],
            )))]
        } else if self.users.is_empty() {
            vec![note(&t("settings-users-empty-accounts"))]
        } else {
            self.users.iter().flat_map(|u| self.user_row(u)).collect()
        };
        people.push(row(self.add_form()));
        sections.push(section(&t("settings-users-login-accounts"), rows(people)));

        let identity_rows = if let Some(error) = &self.identities_error {
            rows(vec![block(error_caption(&t_args(
                "settings-users-identities-read-error",
                &[("error", error.as_str().into())],
            )))])
        } else if self.identities.is_empty() {
            rows(vec![note(&t("settings-users-empty-identities"))])
        } else {
            rows(
                self.identities
                    .iter()
                    .map(|i| self.identity_row(i))
                    .collect(),
            )
        };
        let mut identity_rows =
            identity_rows.push(note(&t("settings-users-protection-secret-service")));
        if self
            .identities
            .iter()
            .any(|identity| identity.protection == KeyProtection::BrokerFile)
        {
            identity_rows = identity_rows.push(note(&t("settings-users-protection-broker-file")));
        }
        if self
            .identities
            .iter()
            .any(|identity| identity.protection == KeyProtection::HardwareBacked)
        {
            identity_rows = identity_rows.push(note(&t("settings-users-protection-hardware")));
        }
        identity_rows = identity_rows.push(note(&t("settings-users-boundary-body")));
        sections.push(section(
            &t("settings-users-identities-title"),
            identity_rows,
        ));
        crate::widgets::page(sections)
    }

    /// Adding a login account: a row that opens its form in place.
    fn add_form(&self) -> Box<dyn Widget> {
        let full = TextInput::local()
            .with_kind(TextInputKind::Name)
            .with_id("users.form.full_name")
            .with_external_buffer(Arc::clone(&self.form.full_name))
            .with_placeholder(t("settings-users-placeholder-full-name"));
        let user = TextInput::local()
            .with_kind(TextInputKind::Username)
            .with_id("users.form.username")
            .with_external_buffer(Arc::clone(&self.form.username))
            .with_placeholder(t("settings-users-placeholder-username"));
        let password = TextInput::local()
            .with_kind(TextInputKind::Password)
            .with_id("users.form.password")
            .with_external_buffer(Arc::clone(&self.form.password))
            .with_secret(true)
            .with_placeholder(t("settings-users-placeholder-password"));
        let username = Arc::clone(&self.form.username);
        let full_name = Arc::clone(&self.form.full_name);
        let create = self.cx.bind(
            Button::new("settings.users.save", &t("settings-users-create"))
                .role(ButtonRole::Primary),
            effect(UsersEffect::CREATE_USER).map(move |()| {
                (
                    username
                        .lock()
                        .map(|name| name.trim().to_owned())
                        .unwrap_or_default(),
                    full_name
                        .lock()
                        .map(|name| name.trim().to_owned())
                        .unwrap_or_default(),
                )
            }),
        );
        let form = crate::widgets::page(vec![untitled(rows(vec![
            field_row(&t("settings-users-full-name"), Box::new(full)),
            field_row(&t("settings-users-username"), Box::new(user)),
            field_row(&t("settings-users-password"), Box::new(password)),
            note(&t("settings-users-password-note")),
            block(Box::new(Row::new().push(Box::new(create)))),
        ]))]);
        Box::new(
            DisclosureRow::new(
                t("settings-users-add-form-title"),
                Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .with_vertical_padding(crate::widgets::spacing().rhythm.sibling)
                        .push(form),
                ),
            )
            .icon(Icon::UserPlus)
            .with_state(&self.add_open),
        )
    }

    /// An account: who it is, its role, and its menu (role, automatic
    /// login, removing it); the signed-in account offers none.
    fn user_row(&self, user: &DeviceUser) -> Vec<Item> {
        let role = if user.admin {
            t("settings-users-role-administrator")
        } else {
            t("settings-users-role-standard")
        };
        let mut tags = vec![(role, ChipStyle::Tinted(StatusTone::Neutral))];
        if user.current {
            tags.push((
                t("settings-users-signed-in"),
                ChipStyle::Tinted(StatusTone::Success),
            ));
        }
        let tags: Vec<(&str, ChipStyle)> = tags
            .iter()
            .map(|(label, style)| (label.as_str(), *style))
            .collect();
        let menu: Option<Box<dyn Widget>> = (!user.current).then(|| {
            let name = &user.username;
            let live = user_live_row(user).into_resource();
            let mut menu = RowMenu::new(
                format!("settings.users.more.{name}"),
                &t_args(
                    "settings-row-more",
                    &[("name", user.full_name.clone().into())],
                ),
            )
            .action(
                t(if user.admin {
                    "settings-users-make-standard"
                } else {
                    "settings-users-make-administrator"
                }),
                self.cx.row_invocation(
                    &live,
                    UsersEffect::SetAdministrator {
                        user: name.clone(),
                        admin: !user.admin,
                    },
                ),
            );
            // An unread automatic-login state offers no switch to flip.
            if user.automatic_login_error.is_none() {
                menu = menu.action(
                    t(if user.automatic_login {
                        "settings-users-disable-autologin"
                    } else {
                        "settings-users-enable-autologin"
                    }),
                    self.cx.row_invocation(
                        &live,
                        UsersEffect::SetAutomaticLogin {
                            user: name.clone(),
                            on: !user.automatic_login,
                        },
                    ),
                );
            }
            Box::new(menu.destructive(
                t("settings-users-remove-ellipsis"),
                self.cx.row_invocation(
                    &live,
                    UsersEffect::RemoveUser {
                        user: name.clone(),
                        keep_files: true,
                    },
                ),
            )) as Box<dyn Widget>
        });
        let account = thing_row(
            &format!("settings.users.user.{}", user.username),
            &user.full_name,
            std::slice::from_ref(&user.username),
            menu,
        )
        .avatar(user.full_name.clone(), user.username.clone())
        .tags(&tags);
        let mut items = vec![row(Box::new(account))];
        if let Some(error) = &user.automatic_login_error {
            items.push(block(error_caption(&t_args(
                "settings-users-autologin-error",
                &[("error", error.as_str().into())],
            ))));
        }
        items
    }

    /// An identity: its name, how its key is protected, whether it is the
    /// one in use, and its menu (use, protect, remove).
    fn identity_row(&self, id: &IdentitySummary) -> Item {
        let live = identity_live_row(id).into_resource();
        let status = if id.active {
            t("settings-users-identity-active")
        } else {
            t("settings-users-identity-available")
        };
        let mut menu = RowMenu::new(
            format!("settings.users.identity.more.{}", id.id),
            &t_args(
                "settings-row-more",
                &[("name", identity_name(id).to_owned().into())],
            ),
        );
        if !id.active {
            menu = menu.action(
                t("settings-users-identity-use"),
                self.cx.row_invocation(
                    &live,
                    UsersEffect::UseIdentity {
                        identity: id.id.clone(),
                    },
                ),
            );
        }
        if id.protection == KeyProtection::LegacyPlaintext {
            menu = menu.action(
                t("settings-users-identity-protect"),
                self.cx.row_invocation(
                    &live,
                    UsersEffect::ProtectIdentity {
                        identity: id.id.clone(),
                    },
                ),
            );
        }
        if !id.active {
            menu = menu.destructive(
                t("settings-users-identity-remove"),
                self.cx.row_invocation(
                    &live,
                    UsersEffect::RemoveIdentity {
                        identity: id.id.clone(),
                    },
                ),
            );
        }
        let shares = t_args(
            "settings-users-identity-shares",
            &[
                ("threshold", id.threshold.into()),
                ("total", id.total_shares.into()),
            ],
        );
        let trailing: Option<Box<dyn Widget>> =
            (!menu.is_empty()).then(|| Box::new(menu) as Box<dyn Widget>);
        row(Box::new(
            thing_row(
                &format!("settings.users.identity.{}", id.id),
                identity_name(id),
                &[protection_label(id.protection), shares],
                trailing,
            )
            .icon(Icon::Fingerprint)
            .tags(&[(
                status.as_str(),
                ChipStyle::Tinted(if id.active {
                    StatusTone::Success
                } else {
                    StatusTone::Neutral
                }),
            )]),
        ))
    }
}

impl PaneModel for UsersModel {
    fn compose(&self) -> Box<dyn Widget> {
        self.build_composition()
    }

    fn poll(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    fn name(&self) -> &'static str {
        "UsersPane"
    }
}

/// The backends a screenshot and the crate's tests change: they accept
/// every change and keep nothing.
struct SampleAdmin;

impl UserAdmin for SampleAdmin {
    fn create(&self, _: &str, _: &str, password: &mut [u8]) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        password.fill(0);
        Ok(())
    }
    fn delete(&self, _: &DeviceUser, _: bool) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn set_admin(&self, _: &str, _: bool) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn set_autologin(&self, _: &str, _: bool) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        Ok(())
    }
    fn accounts(&self) -> Option<Result<Vec<DeviceUser>, String>> {
        crate::device_read::off_ui_thread();
        None
    }
}

/// A call the host already answered with `value`.
fn answered(value: serde_json::Value) -> IdentityCall {
    ui::LocalState::new(ui::RemoteCall::Done(
        ui::tilo_actions::InvocationUpdate::Succeeded {
            invocation: ui::tilo_actions::InvocationId::new(),
            output: ui::tilo_actions::ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
        },
    ))
}

impl IdentityAdmin for SampleAdmin {
    fn list(&self) -> IdentityCall {
        answered(serde_json::Value::Array(Vec::new()))
    }
    fn activate(&self, _: &str) -> IdentityCall {
        answered(serde_json::Value::Null)
    }
    fn remove(&self, _: &str) -> IdentityCall {
        answered(serde_json::Value::Null)
    }
    fn protect(&self, _: &str) -> IdentityCall {
        answered(serde_json::Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingAdmin(Mutex<Vec<String>>);
    impl UserAdmin for RecordingAdmin {
        fn create(&self, u: &str, n: &str, p: &mut [u8]) -> Result<(), String> {
            self.0
                .lock()
                .unwrap()
                .push(format!("create:{u}:{n}:{}", p.len()));
            p.fill(0);
            Ok(())
        }
        fn delete(&self, u: &DeviceUser, files: bool) -> Result<(), String> {
            self.0
                .lock()
                .unwrap()
                .push(format!("delete:{}:{files}", u.username));
            Ok(())
        }
        fn set_admin(&self, u: &str, a: bool) -> Result<(), String> {
            self.0.lock().unwrap().push(format!("admin:{u}:{a}"));
            Ok(())
        }
        fn set_autologin(&self, u: &str, a: bool) -> Result<(), String> {
            self.0.lock().unwrap().push(format!("autologin:{u}:{a}"));
            Ok(())
        }
        fn accounts(&self) -> Option<Result<Vec<DeviceUser>, String>> {
            None
        }
    }
    #[derive(Default)]
    struct RecordingIds(Mutex<Vec<String>>);
    impl IdentityAdmin for RecordingIds {
        fn list(&self) -> IdentityCall {
            self.0.lock().unwrap().push("list".into());
            answered(serde_json::json!([]))
        }
        fn activate(&self, id: &str) -> IdentityCall {
            self.0.lock().unwrap().push(format!("activate:{id}"));
            answered(serde_json::Value::Null)
        }
        fn remove(&self, id: &str) -> IdentityCall {
            self.0.lock().unwrap().push(format!("remove:{id}"));
            answered(serde_json::Value::Null)
        }
        fn protect(&self, id: &str) -> IdentityCall {
            self.0.lock().unwrap().push(format!("protect:{id}"));
            answered(serde_json::Value::Null)
        }
    }
    fn user(name: &str, current: bool) -> DeviceUser {
        DeviceUser {
            username: name.into(),
            full_name: name.into(),
            uid: if current { 1000 } else { 1001 },
            admin: current,
            current,
            automatic_login: false,
            automatic_login_error: None,
        }
    }
    /// Accounts that could not be read are not "no accounts", and an
    /// unread automatic-login state is reported rather than offered as off.
    #[test]
    fn unreadable_accounts_are_shown_as_errors() {
        let profile = tempfile::tempdir().unwrap();
        let mut pane = UsersModel::sample(PaneCx::in_file(profile.path().join("settings.json")));
        pane.users.clear();
        pane.users_error = Some("/etc/passwd: No such file or directory (os error 2)".into());
        let text = crate::widgets::announced_text(pane.build_composition().as_ref());
        assert!(
            text.iter().any(|line| line
                == "Can't read this device's accounts: /etc/passwd: No such file or directory (os error 2)"),
            "{text:#?}"
        );
        assert!(
            !text.contains(&t("settings-users-empty-accounts")),
            "{text:#?}"
        );

        let mut bob = user("bob", false);
        bob.automatic_login_error = Some("AccountsService is unreachable".into());
        pane.users_error = None;
        pane.users = vec![user("ada", true), bob];
        let text = crate::widgets::announced_text(pane.build_composition().as_ref());
        assert!(
            text.iter()
                .any(|line| line == "Can't read automatic login: AccountsService is unreachable"),
            "{text:#?}"
        );
        assert!(
            !text.contains(&t("settings-users-enable-autologin")),
            "{text:#?}"
        );
    }

    fn identity(id: &str, active: bool) -> IdentitySummary {
        IdentitySummary {
            id: id.into(),
            display_name: id.into(),
            public_key: [1; 32],
            threshold: 1,
            total_shares: 1,
            active,
            protection: KeyProtection::BrokerFile,
        }
    }

    fn backends(users: &Arc<RecordingAdmin>) -> Backends {
        Backends {
            users: Arc::clone(users) as Arc<dyn UserAdmin>,
        }
    }

    /// Login users are read from the device; every change reaches its narrow
    /// backend, and never targets the signed-in account.
    #[test]
    fn user_accounts_parse_and_change_through_narrow_backends() {
        let admins = admins_from_group("wheel:x:10:ada\n");
        let parsed = read_passwd_users(
            "root:x:0:0:root:/root:/bin/bash\nada:x:1000:1000:Ada Lovelace:/home/ada:/bin/bash\n",
            Some("ada"),
            &admins,
        );
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].admin && parsed[0].current);
        assert_eq!(parsed[0].uid, 1000);

        let users = Arc::new(RecordingAdmin::default());
        let ids = Rc::new(RecordingIds::default());
        let backends = backends(&users);
        let people = [user("ada", true), user("bob", false)];
        let none = || "";
        for effect in [
            UsersEffect::RemoveUser {
                user: "bob".into(),
                keep_files: false,
            },
            UsersEffect::SetAdministrator {
                user: "bob".into(),
                admin: true,
            },
        ] {
            carry_out(&backends, &people, &effect, none()).unwrap();
        }
        let profile = tempfile::tempdir().unwrap();
        let mut pane = UsersModel::sample(PaneCx::in_file(profile.path().join("settings.json")));
        pane.identity_admin = Some(Rc::clone(&ids) as Rc<dyn IdentityAdmin>);
        for effect in [
            UsersEffect::UseIdentity {
                identity: "work".into(),
            },
            UsersEffect::ProtectIdentity {
                identity: "personal".into(),
            },
        ] {
            pane.start_identity_change(&effect)
                .expect("an identity change")
                .expect("the store answers");
        }
        assert!(
            pane.start_identity_change(&UsersEffect::SetAdministrator {
                user: "bob".into(),
                admin: false,
            })
            .is_none(),
            "account changes never reach the identity store"
        );
        assert!(
            carry_out(
                &backends,
                &people,
                &UsersEffect::RemoveUser {
                    user: "ada".into(),
                    keep_files: true
                },
                none()
            )
            .is_err(),
            "the signed-in account is never removed"
        );
        carry_out(
            &backends,
            &people,
            &UsersEffect::CreateUser {
                username: "cy".into(),
                full_name: "Cy".into(),
            },
            "correct horse",
        )
        .unwrap();
        assert_eq!(
            users.0.lock().unwrap().as_slice(),
            ["delete:bob:true", "admin:bob:true", "create:cy:Cy:13"]
        );
        assert_eq!(
            ids.0.lock().unwrap().as_slice(),
            ["activate:work", "protect:personal"]
        );
        let _ = identity("work", false);
    }

    /// The host's identity actions, in process: it records what it was
    /// asked and answers the list with one identity.
    #[derive(Default)]
    struct IdentityHost {
        asked: Mutex<Vec<ui::tilo_actions::remote::InvokeRequest>>,
    }

    impl ui::tilo_actions::remote::ActionHost for IdentityHost {
        fn query(
            &self,
            _: &ui::tilo_actions::remote::Caller,
            _: ui::tilo_actions::remote::CatalogueQuery,
        ) -> ui::tilo_actions::remote::Pending<
            Result<
                ui::tilo_actions::remote::CataloguePage,
                ui::tilo_actions::remote::ActionHostError,
            >,
        > {
            ui::tilo_actions::remote::Pending::ready(Err(
                ui::tilo_actions::remote::ActionHostError::NotPermitted,
            ))
        }

        fn invoke(
            &self,
            _: &ui::tilo_actions::remote::Caller,
            request: ui::tilo_actions::remote::InvokeRequest,
        ) -> ui::tilo_actions::remote::Pending<
            Result<
                ui::tilo_actions::remote::InvocationReceipt,
                ui::tilo_actions::remote::ActionHostError,
            >,
        > {
            use ui::tilo_actions::remote::identity_admin;
            let invocation = request.invocation;
            let value = if request.target.address.action == identity_admin::LIST {
                serde_json::to_value(vec![identity("work", true)]).unwrap()
            } else {
                serde_json::json!({ "id": "old" })
            };
            self.asked.lock().unwrap().push(request);
            ui::tilo_actions::remote::Pending::ready(Ok(
                ui::tilo_actions::remote::InvocationReceipt {
                    invocation,
                    status: ui::tilo_actions::remote::ReceiptStatus::Completed {
                        output: ui::tilo_actions::ActionOutput {
                            value: Some(value),
                            resources: Vec::new(),
                        },
                    },
                },
            ))
        }

        fn observe(
            &self,
            _: &ui::tilo_actions::remote::Caller,
            _: &ui::tilo_actions::InvocationId,
        ) -> Option<ui::tilo_actions::remote::InvocationState> {
            None
        }

        fn cancel(
            &self,
            _: &ui::tilo_actions::remote::Caller,
            _: &ui::tilo_actions::InvocationId,
        ) -> Result<(), ui::tilo_actions::remote::ActionHostError> {
            Ok(())
        }
    }

    /// The pane reads and changes identities only through the host's
    /// identity actions: the list arrives as the host answers it, removal
    /// carries the person's confirmation, and nothing reads the store.
    #[test]
    fn identities_are_listed_and_changed_through_the_host_actions() {
        use ui::tilo_actions::remote::{Confirmation, InvokeArgument, identity_admin};
        let host = Arc::new(IdentityHost::default());
        let remote = ui::RemoteActions::with_host(
            host.clone(),
            ui::tilo_actions::remote::Caller::local_person("owner"),
        );
        let profile = tempfile::tempdir().unwrap();
        let mut pane = UsersModel::from_read(
            PaneCx::in_file(profile.path().join("settings.json")),
            UsersRead {
                users: Ok(Vec::new()),
            },
            Rc::new(RemoteIdentityAdmin { remote }),
        );
        pane.poll_identities();
        assert_eq!(pane.identities, [identity("work", true)]);
        assert!(pane.identity_list.is_none(), "the list has answered");

        for effect in [
            UsersEffect::RemoveIdentity {
                identity: "old".into(),
            },
            UsersEffect::UseIdentity {
                identity: "old".into(),
            },
        ] {
            let call = pane
                .start_identity_change(&effect)
                .expect("an identity change")
                .expect("the host answers");
            assert_eq!(
                crate::device_fleet::settled(&call).map(|answer| answer.is_ok()),
                Some(true)
            );
        }
        let asked = host.asked.lock().unwrap().clone();
        let targets: Vec<_> = asked
            .iter()
            .map(|request| request.target.address.action.as_str())
            .collect();
        assert_eq!(
            targets,
            [
                identity_admin::LIST,
                identity_admin::REMOVE,
                identity_admin::ACTIVATE
            ]
        );
        assert!(
            asked
                .iter()
                .all(|request| request.target.provider == identity_admin::PROVIDER)
        );
        assert_eq!(asked[1].confirmation, Some(Confirmation::Confirmed));
        assert_eq!(
            asked[1].argument,
            InvokeArgument::Json(
                serde_json::json!({ "id": "old", "confirm": true })
                    .as_object()
                    .unwrap()
                    .clone()
            )
        );
        assert_eq!(asked[0].confirmation, None, "listing asks nothing");
        assert_eq!(asked[2].confirmation, Some(Confirmation::Confirmed));
    }

    /// An `AccountsService` that takes changes at once but does not answer
    /// a read until it is let go (or ten seconds pass), as one waiting on a
    /// wedged polkit. Once let go it knows one more account.
    struct StalledAdmin(Arc<(Mutex<bool>, std::sync::Condvar)>);

    impl UserAdmin for StalledAdmin {
        fn create(&self, _: &str, _: &str, password: &mut [u8]) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            password.fill(0);
            Ok(())
        }
        fn delete(&self, _: &DeviceUser, _: bool) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            Ok(())
        }
        fn set_admin(&self, _: &str, _: bool) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            Ok(())
        }
        fn set_autologin(&self, _: &str, _: bool) -> Result<(), String> {
            crate::device_read::off_ui_thread();
            Ok(())
        }
        fn accounts(&self) -> Option<Result<Vec<DeviceUser>, String>> {
            crate::device_read::off_ui_thread();
            let (open, released) = &*self.0;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let mut open = open.lock().unwrap();
            while !*open {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    return Some(Err("AccountsService never answered".into()));
                }
                open = released.wait_timeout(open, left).unwrap().0;
            }
            drop(open);
            let mut barbara = user("barbara", false);
            barbara.full_name = "Barbara Liskov".into();
            Some(Ok(vec![user("ada", true), barbara]))
        }
    }

    /// A change is answered and the window keeps drawing while
    /// `AccountsService` takes its time to be read again: the accounts are
    /// re-read on the change's job, and land once it returns.
    #[test]
    fn a_stalled_accounts_service_never_holds_the_window() {
        use crate::resource::SettingsShell;
        use ui::prelude::TestApp;

        let mut app = TestApp::<SettingsShell>::new().unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::Users);
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let stalled = Arc::clone(&gate);
        app.with_widget::<SettingsApp, _>(move |settings| {
            settings.users().model().borrow_mut().backends.users = Arc::new(StalledAdmin(stalled));
        })
        .expect("the Settings window is mounted");

        let clicked = std::time::Instant::now();
        app.start_effect_on(
            "settings.user:grace",
            &SettingsEffect::from(UsersEffect::SetAdministrator {
                user: "grace".into(),
                admin: true,
            }),
        )
        .expect("the role change runs as a job");
        app.harness()
            .poll_frame()
            .expect("the window draws its next frame");
        let answered = clicked.elapsed();
        assert!(
            answered < std::time::Duration::from_secs(1),
            "the click waited {answered:?} on AccountsService"
        );
        let frame = app.harness().inspection_json().unwrap();
        assert!(
            frame.contains("Grace Hopper"),
            "the last read is shown: {frame}"
        );
        assert!(!frame.contains("Barbara Liskov"), "{frame}");

        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(30), |harness| harness
                    .inspection_json()
                    .is_ok_and(|frame| frame.contains("Barbara Liskov"))),
            "the accounts read again are shown"
        );
    }

    /// The password typed into the form is read once by the effect and
    /// never reaches its arguments or the settings.
    #[test]
    fn the_password_stays_in_the_form() {
        use crate::resource::SettingsShell;
        use ui::prelude::TestApp;

        let mut app = TestApp::<SettingsShell>::new().unwrap();
        app.effect(&SettingsEffect::from(UsersEffect::CreateUser {
            username: "cy".into(),
            full_name: "Cy".into(),
        }))
        .expect("the sample backend creates the account");
        let create = SettingsEffect::declarations()
            .into_iter()
            .find(|decl| decl.id == "create-user")
            .expect("creating an account is an effect");
        assert!(
            create.params.iter().all(|param| param.name != "password"),
            "{:?}",
            create.params
        );
    }
}
