//! Local login accounts and Tilo signing identities.
//!
//! A Unix login account owns a home/session; it may contain several Tilo
//! identities (personal, work, pseudonymous). Local-account mutations go
//! through `AccountsService` and polkit. Identity activation writes only a
//! non-secret selector; the launcher mounts one exact record into new apps.
//! Every change is a [`UsersEffect`] on the account or identity it names.

use std::cell::OnceCell;
use std::collections::HashSet;
use std::fs;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use runtime_identity::{IdentitySummary, KeyProtection};
use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow};
use ui::tilo_resource::{self, Effect};
use ui::widget::{Button, Column, Row, Spacer, Text, TextInput, TextInputKind, Widget};
use zeroize::Zeroizing;

use crate::effects::{SettingsEffect, effect};
use crate::host::PaneCx;
use crate::i18n::{t, t_args};
use crate::panes::model::{PaneModel, Shared};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{
    El, StatusTone, caption, card, error_caption, rows, semantic_status_pill, setting_row,
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

/// Answer `effect` on a worker, then read the accounts again.
#[expect(
    clippy::unnecessary_wraps,
    reason = "every area answers through the effect vocabulary's one signature"
)]
pub(crate) fn answer(
    app: &SettingsApp,
    effect: UsersEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let model = app.users().model();
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
    Ok(cx.thread(
        move |_| carry_out(&backends, &users, &effect, &password),
        move |app: &mut SettingsApp, outcome: Result<(), String>, _| {
            let model = app.users().model();
            let mut model = model.borrow_mut();
            if created && outcome.is_ok() {
                model.form.clear();
            }
            model.error = outcome.as_ref().err().cloned();
            model.reload();
            outcome.map(|()| Effected::done())
        },
    ))
}

/// The services that change accounts and identities.
#[derive(Clone)]
struct Backends {
    users: Arc<dyn UserAdmin>,
    identities: Option<Arc<dyn IdentityAdmin>>,
}

/// Carry out `effect`. Never called on the UI thread: `AccountsService`
/// may wait on polkit.
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
    let identities = || {
        backends
            .identities
            .as_ref()
            .ok_or_else(|| t("settings-users-identity-store-unavailable"))
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
        UsersEffect::UseIdentity { identity } => identities()?.activate(identity),
        UsersEffect::RemoveIdentity { identity } => identities()?.remove(identity),
        UsersEffect::ProtectIdentity { identity } => identities()?.protect(identity),
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
}

struct AccountsServiceAdmin;

impl UserAdmin for AccountsServiceAdmin {
    fn create(&self, username: &str, full_name: &str, password: &mut [u8]) -> Result<(), String> {
        os_actions::users::create_user(username, full_name, os_actions::AccountRole::Standard)?;
        // Never destructively roll back a newly created home if credential
        // setup fails; it may already contain PAM/AccountsService work.
        os_actions::users::set_password(username, password)
    }

    fn delete(&self, user: &DeviceUser, remove_files: bool) -> Result<(), String> {
        os_actions::users::delete_user(&user.username, user.uid, remove_files)
    }

    fn set_admin(&self, username: &str, admin: bool) -> Result<(), String> {
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
        os_actions::users::set_automatic_login(username, enabled)
    }
}

trait IdentityAdmin: Send + Sync {
    fn activate(&self, id: &str) -> Result<(), String>;
    fn remove(&self, id: &str) -> Result<(), String>;
    fn protect(&self, id: &str) -> Result<(), String>;
}

struct DiskIdentityAdmin {
    dir: std::path::PathBuf,
}

impl IdentityAdmin for DiskIdentityAdmin {
    fn activate(&self, id: &str) -> Result<(), String> {
        runtime_identity::set_active_identity(&self.dir, id).map_err(|e| e.to_string())
    }
    fn remove(&self, id: &str) -> Result<(), String> {
        runtime_identity::remove_identity(&self.dir, id).map_err(|e| e.to_string())
    }
    fn protect(&self, id: &str) -> Result<(), String> {
        runtime_identity::secure_identity(&self.dir, id)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
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

/// The device's login accounts, or why they could not be read.
fn read_users() -> Result<Vec<DeviceUser>, String> {
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

fn identities_for_display() -> (Vec<IdentitySummary>, Option<DiskIdentityAdmin>) {
    let Ok(dir) = runtime_identity::identity_dir_from_env() else {
        return (Vec::new(), None);
    };
    let ids = runtime_identity::list_identities(&dir).unwrap_or_default();
    (ids, Some(DiskIdentityAdmin { dir }))
}

/// What the users area keeps for the window: the model, read when first
/// needed.
pub struct UsersArea {
    cx: PaneCx,
    model: OnceCell<Shared<UsersModel>>,
    /// The device's accounts, read off the UI thread after the window's
    /// first frame ([`Self::start_read`]); `None` stands for the samples a
    /// screenshot shows.
    read: crate::device_read::DeviceRead<Option<UsersRead>>,
}

impl UsersArea {
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            cx: cx.clone(),
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
                Some(read) => UsersModel::from_read(self.cx.clone(), read),
                None => UsersModel::sample(self.cx.clone()),
            };
            Rc::new(std::cell::RefCell::new(model))
        })
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
        let accounts = model.users.iter().filter(|user| !user.current).map(|user| {
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
        });
        let identities = model.identities.iter().map(|identity| {
            let id = identity.id.clone();
            let mut row = LiveRow::new(
                format!("settings.identity:{id}"),
                "settings.identity",
                if identity.display_name.is_empty() {
                    id.clone()
                } else {
                    identity.display_name.clone()
                },
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
                row = row.operation(
                    SettingsEffect::from(UsersEffect::ProtectIdentity { identity: id }).bound(),
                );
            }
            row
        });
        accounts.chain(identities).collect()
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
    backends: Backends,
    /// Whether the accounts are this device's, read again after a change.
    of_device: bool,
    form: AccountForm,
    error: Option<String>,
    changed: bool,
}

/// What the users area reads of the device: its login accounts (one
/// `AccountsService` call per person for automatic login) and the person's
/// identities. Plain data, so it is read off the UI thread.
struct UsersRead {
    identities: Vec<IdentitySummary>,
    identity_admin: Option<DiskIdentityAdmin>,
    users: Result<Vec<DeviceUser>, String>,
}

impl UsersRead {
    fn of_device() -> Self {
        let (identities, identity_admin) = identities_for_display();
        Self {
            identities,
            identity_admin,
            users: read_users(),
        }
    }
}

impl UsersModel {
    /// The model over what `read` found on this device.
    fn from_read(cx: PaneCx, read: UsersRead) -> Self {
        let UsersRead {
            identities,
            identity_admin,
            users,
        } = read;
        let (users, users_error) = users_and_error(users);
        Self {
            cx,
            users,
            users_error,
            identities,
            backends: Backends {
                users: Arc::new(AccountsServiceAdmin),
                identities: identity_admin.map(|admin| Arc::new(admin) as Arc<dyn IdentityAdmin>),
            },
            of_device: true,
            form: AccountForm::default(),
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
            backends: Backends {
                users: Arc::new(SampleAdmin),
                identities: Some(Arc::new(SampleAdmin)),
            },
            of_device: false,
            form: AccountForm::default(),
            error: None,
            changed: false,
        }
    }

    fn reload(&mut self) {
        self.changed = true;
        if !self.of_device {
            return;
        }
        (self.users, self.users_error) = users_and_error(read_users());
        if let Ok(dir) = runtime_identity::identity_dir_from_env() {
            self.identities = runtime_identity::list_identities(&dir).unwrap_or_default();
        }
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let mut cards: Vec<El> = Vec::new();
        if let Some(error) = &self.error {
            cards.push(card(
                &t("settings-users-error-title"),
                rows(vec![caption(error)]),
            ));
        }
        let people = if let Some(error) = &self.users_error {
            rows(vec![error_caption(&t_args(
                "settings-users-read-error",
                &[("error", error.as_str().into())],
            ))])
        } else if self.users.is_empty() {
            rows(vec![caption(&t("settings-users-empty-accounts"))])
        } else {
            rows(self.users.iter().map(|u| self.user_row(u)).collect())
        };
        cards.push(card(&t("settings-users-login-accounts"), people));
        cards.push(self.add_form());

        let identity_rows = if self.identities.is_empty() {
            rows(vec![caption(&t("settings-users-empty-identities"))])
        } else {
            rows(
                self.identities
                    .iter()
                    .map(|i| self.identity_row(i))
                    .collect(),
            )
        };
        cards.push(card(&t("settings-users-identities-title"), identity_rows));
        let mut protection_rows: Vec<El> =
            vec![caption(&t("settings-users-protection-secret-service"))];
        if self
            .identities
            .iter()
            .any(|identity| identity.protection == KeyProtection::BrokerFile)
        {
            protection_rows.push(caption(&t("settings-users-protection-broker-file")));
        }
        if self
            .identities
            .iter()
            .any(|identity| identity.protection == KeyProtection::HardwareBacked)
        {
            protection_rows.push(caption(&t("settings-users-protection-hardware")));
        }
        cards.push(card(
            &t("settings-users-protection-title"),
            rows(protection_rows),
        ));
        cards.push(card(
            &t("settings-users-boundary-title"),
            rows(vec![caption(&t("settings-users-boundary-body"))]),
        ));
        crate::widgets::cards(cards)
    }

    fn add_form(&self) -> El {
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
            Button::new("settings.users.save", &t("settings-users-create")),
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
        let buttons = Row::new()
            .push(Box::new(create))
            .push(Box::new(Spacer::new()));
        card(
            &t("settings-users-add-form-title"),
            rows(vec![
                setting_row(&t("settings-users-full-name"), Box::new(full)),
                setting_row(&t("settings-users-username"), Box::new(user)),
                setting_row(&t("settings-users-password"), Box::new(password)),
                caption(&t("settings-users-password-note")),
                Box::new(buttons),
            ]),
        )
    }

    /// What can be done to `user`'s account: nothing for the signed-in one.
    fn user_actions(&self, user: &DeviceUser) -> Row {
        let mut actions = Row::new().push(Box::new(Spacer::new()));
        if !user.current {
            let role_action = if user.admin {
                t("settings-users-make-standard")
            } else {
                t("settings-users-make-administrator")
            };
            let auto_action = if user.automatic_login {
                t("settings-users-disable-autologin")
            } else {
                t("settings-users-enable-autologin")
            };
            let name = &user.username;
            actions = actions.push(Box::new(self.cx.bind_invocation(
                Button::new(&format!("settings.users.role.{name}"), &role_action),
                UsersEffect::SetAdministrator {
                    user: name.clone(),
                    admin: !user.admin,
                },
            )));
            // An unread automatic-login state offers no switch to flip.
            if user.automatic_login_error.is_none() {
                actions = actions.push(Box::new(self.cx.bind_invocation(
                    Button::new(&format!("settings.users.autologin.{name}"), &auto_action),
                    UsersEffect::SetAutomaticLogin {
                        user: name.clone(),
                        on: !user.automatic_login,
                    },
                )));
            }
            actions = actions
                .push(Box::new(self.cx.bind_invocation(
                    Button::new(
                        &format!("settings.users.remove_keep.{name}"),
                        &t("settings-users-remove-keep-files"),
                    ),
                    UsersEffect::RemoveUser {
                        user: name.clone(),
                        keep_files: true,
                    },
                )))
                .push(Box::new(self.cx.bind_invocation(
                    Button::new(
                        &format!("settings.users.remove.{name}"),
                        &t("settings-users-remove-account-and-files"),
                    ),
                    UsersEffect::RemoveUser {
                        user: name.clone(),
                        keep_files: false,
                    },
                )));
        }
        actions
    }

    fn user_row(&self, user: &DeviceUser) -> El {
        let actions = self.user_actions(user);
        let role = if user.admin {
            t("settings-users-role-administrator")
        } else {
            t("settings-users-role-standard")
        };
        let you = if user.current {
            t("settings-users-signed-in")
        } else {
            String::new()
        };
        let mut row = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(
                Row::new()
                    .push(Box::new(
                        Text::new(&user.full_name).with_role(ui::TextRole::Body),
                    ))
                    .push(Box::new(Spacer::new()))
                    .push(semantic_status_pill(
                        &role,
                        if user.admin {
                            StatusTone::Success
                        } else {
                            StatusTone::Neutral
                        },
                    )),
            ))
            .push(Box::new(
                Text::new(&t_args(
                    "settings-users-uid-line",
                    &[
                        ("username", user.username.as_str().into()),
                        ("uid", user.uid.into()),
                        ("you", you.as_str().into()),
                    ],
                ))
                .with_role(ui::TextRole::Caption)
                .dimmed(),
            ));
        if let Some(error) = &user.automatic_login_error {
            row = row.push(error_caption(&t_args(
                "settings-users-autologin-error",
                &[("error", error.as_str().into())],
            )));
        }
        Box::new(row.push(Box::new(actions)))
    }

    fn identity_row(&self, id: &IdentitySummary) -> El {
        let name = if id.display_name.is_empty() {
            &id.id
        } else {
            &id.display_name
        };
        let key = hex::encode(&id.public_key[..4]);
        let status = if id.active {
            t("settings-users-identity-active")
        } else {
            t("settings-users-identity-available")
        };
        let mut top = Row::new()
            .push(Box::new(Text::new(name).with_role(ui::TextRole::Body)))
            .push(Box::new(Spacer::new()))
            .push(semantic_status_pill(
                &status,
                if id.active {
                    StatusTone::Success
                } else {
                    StatusTone::Neutral
                },
            ));
        if !id.active {
            top = top
                .push(Box::new(self.cx.bind_invocation(
                    Button::new(
                        &format!("settings.users.identity.activate.{}", id.id),
                        &t("settings-users-identity-use"),
                    ),
                    UsersEffect::UseIdentity {
                        identity: id.id.clone(),
                    },
                )))
                .push(Box::new(self.cx.bind_invocation(
                    Button::new(
                        &format!("settings.users.identity.remove.{}", id.id),
                        &t("settings-users-identity-remove"),
                    ),
                    UsersEffect::RemoveIdentity {
                        identity: id.id.clone(),
                    },
                )));
        }
        if id.protection == KeyProtection::LegacyPlaintext {
            top = top.push(Box::new(self.cx.bind_invocation(
                Button::new(
                    &format!("settings.users.identity.protect.{}", id.id),
                    &t("settings-users-identity-protect"),
                ),
                UsersEffect::ProtectIdentity {
                    identity: id.id.clone(),
                },
            )));
        }
        let protection = id.protection.label();
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(Box::new(top))
                .push(Box::new(
                    Text::new(&t_args(
                        "settings-users-identity-detail",
                        &[
                            ("id", id.id.as_str().into()),
                            ("key", key.as_str().into()),
                            ("threshold", id.threshold.into()),
                            ("total", id.total_shares.into()),
                            ("protection", protection.into()),
                        ],
                    ))
                    .with_role(ui::TextRole::Caption)
                    .dimmed(),
                )),
        )
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
        password.fill(0);
        Ok(())
    }
    fn delete(&self, _: &DeviceUser, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn set_admin(&self, _: &str, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn set_autologin(&self, _: &str, _: bool) -> Result<(), String> {
        Ok(())
    }
}

impl IdentityAdmin for SampleAdmin {
    fn activate(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn remove(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn protect(&self, _: &str) -> Result<(), String> {
        Ok(())
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
    }
    #[derive(Default)]
    struct RecordingIds(Mutex<Vec<String>>);
    impl IdentityAdmin for RecordingIds {
        fn activate(&self, id: &str) -> Result<(), String> {
            self.0.lock().unwrap().push(format!("activate:{id}"));
            Ok(())
        }
        fn remove(&self, id: &str) -> Result<(), String> {
            self.0.lock().unwrap().push(format!("remove:{id}"));
            Ok(())
        }
        fn protect(&self, id: &str) -> Result<(), String> {
            self.0.lock().unwrap().push(format!("protect:{id}"));
            Ok(())
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

    fn backends(users: &Arc<RecordingAdmin>, ids: &Arc<RecordingIds>) -> Backends {
        Backends {
            users: Arc::clone(users) as Arc<dyn UserAdmin>,
            identities: Some(Arc::clone(ids) as Arc<dyn IdentityAdmin>),
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
        let ids = Arc::new(RecordingIds::default());
        let backends = backends(&users, &ids);
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
            UsersEffect::UseIdentity {
                identity: "work".into(),
            },
            UsersEffect::ProtectIdentity {
                identity: "personal".into(),
            },
        ] {
            carry_out(&backends, &people, &effect, none()).unwrap();
        }
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
