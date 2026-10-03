//! Local login accounts and Tilo signing identities.
//!
//! A Unix login account owns a home/session; it may contain several Tilo
//! identities (personal, work, pseudonymous). Local-account mutations go
//! through `AccountsService` and polkit. Identity activation writes only a
//! non-secret selector; the launcher mounts one exact record into new apps.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::sync::{Arc, Mutex};
use ui::doc::{read_bool, read_str_or_text};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId, transaction::Transactable};
use runtime_identity::{IdentitySummary, KeyProtection};
use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{Button, ButtonRole, Row, Spacer, TextInput, TextInputKind, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;
use zeroize::Zeroizing;

use crate::i18n::{t, t_args};
use crate::widgets::{
    El, StatusTone, caption, card, card_with_action, field_row, item_row, rows,
    semantic_status_pill,
};

const ADD_KEY: &str = "settings.users.add";
const SAVE_KEY: &str = "settings.users.form.save";
const CANCEL_KEY: &str = "settings.users.form.cancel";
const FULL_NAME_KEY: &str = "users.form.full_name";
const USERNAME_KEY: &str = "users.form.username";
const FORM_KEYS: [&str; 2] = [FULL_NAME_KEY, USERNAME_KEY];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceUser {
    pub username: String,
    pub full_name: String,
    pub uid: u64,
    pub admin: bool,
    pub current: bool,
    pub automatic_login: bool,
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
            })
        })
        .collect()
}

pub(crate) fn users_for_display() -> Vec<DeviceUser> {
    let current = std::env::var("USER").ok();
    let admins = fs::read_to_string("/etc/group")
        .map(|s| admins_from_group(&s))
        .unwrap_or_default();
    let mut users = fs::read_to_string("/etc/passwd")
        .map(|s| read_passwd_users(&s, current.as_deref(), &admins))
        .unwrap_or_default();
    for user in &mut users {
        user.automatic_login = os_actions::users::automatic_login(&user.username).unwrap_or(false);
    }
    users
}

fn identities_for_display() -> (Vec<IdentitySummary>, Option<DiskIdentityAdmin>) {
    let Ok(dir) = runtime_identity::identity_dir_from_env() else {
        return (Vec::new(), None);
    };
    let ids = runtime_identity::list_identities(&dir).unwrap_or_default();
    (ids, Some(DiskIdentityAdmin { dir }))
}

/// How an identity's key is kept, in words a person recognises.
fn protection_label(protection: KeyProtection) -> String {
    t(match protection {
        KeyProtection::SecretService => "settings-users-protection-service",
        KeyProtection::BrokerFile => "settings-users-protection-file",
        KeyProtection::HardwareBacked => "settings-users-protection-hardware-backed",
        KeyProtection::LegacyPlaintext => "settings-users-protection-legacy",
        KeyProtection::Unavailable => "settings-users-protection-unavailable",
    })
}

pub struct UsersPane {
    users: Vec<DeviceUser>,
    identities: Vec<IdentitySummary>,
    user_admin: Arc<dyn UserAdmin>,
    identity_admin: Option<Arc<dyn IdentityAdmin>>,
    /// Process-local only. Password text never enters Automerge, presence,
    /// undo history, settings persistence, or diagnostics.
    password_buffer: Arc<Mutex<String>>,
    form_open: bool,
    confirm_remove_user: Option<String>,
    confirm_remove_identity: Option<String>,
    error: Option<String>,
    operation_status: ui::LocalState<crate::actions::LocalOperationStatus>,
    command_state: ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    local_doc: AutoCommit,
    isolate_document: bool,
    structure_dirty: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for UsersPane {
    fn default() -> Self {
        Self::new()
    }
}

impl UsersPane {
    pub fn new() -> Self {
        let (identities, identity_admin) = identities_for_display();
        Self {
            users: users_for_display(),
            identities,
            user_admin: Arc::new(AccountsServiceAdmin),
            identity_admin: identity_admin.map(|a| Arc::new(a) as Arc<dyn IdentityAdmin>),
            password_buffer: Arc::new(Mutex::new(String::new())),
            form_open: false,
            confirm_remove_user: None,
            confirm_remove_identity: None,
            error: None,
            operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            command_state: ui::LocalState::new(None),
            local_doc: AutoCommit::new(),
            isolate_document: false,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    #[cfg(any(test, not(feature = "thin")))]
    fn with_data(
        users: Vec<DeviceUser>,
        identities: Vec<IdentitySummary>,
        user_admin: Arc<dyn UserAdmin>,
        identity_admin: Arc<dyn IdentityAdmin>,
    ) -> Self {
        Self {
            users,
            identities,
            user_admin,
            identity_admin: Some(identity_admin),
            password_buffer: Arc::new(Mutex::new(String::new())),
            form_open: false,
            confirm_remove_user: None,
            confirm_remove_identity: None,
            error: None,
            operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            command_state: ui::LocalState::new(None),
            local_doc: AutoCommit::new(),
            isolate_document: false,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
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
        if self.form_open {
            cards.push(self.add_form());
        }
        let add = Box::new(Button::new(
            "settings.users.add",
            &t("settings-users-add"),
            Some(ADD_KEY),
        ));
        let people = if self.users.is_empty() {
            rows(vec![caption(&t("settings-users-empty-accounts"))])
        } else {
            rows(self.users.iter().map(|u| self.user_row(u)).collect())
        };
        cards.push(card_with_action(
            &t("settings-users-login-accounts"),
            add,
            people,
        ));

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
        if self
            .identities
            .iter()
            .any(|identity| identity.protection == KeyProtection::BrokerFile)
        {
            cards.push(caption(&t("settings-users-protection-broker-file")));
        }
        crate::widgets::cards(cards)
    }

    #[must_use]
    pub fn with_operation_status(
        mut self,
        status: &ui::LocalState<crate::actions::LocalOperationStatus>,
    ) -> Self {
        self.operation_status = status.clone();
        self
    }
    #[must_use]
    pub fn with_command_state(
        mut self,
        commands: &ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    ) -> Self {
        self.command_state = commands.clone();
        self.isolate_document = true;
        self
    }

    fn add_form(&self) -> El {
        let full = TextInput::new(Vec::new())
            .with_kind(TextInputKind::Name)
            .with_id(FULL_NAME_KEY)
            .with_on_change_path(FULL_NAME_KEY)
            .with_placeholder(&t("settings-users-placeholder-full-name"));
        let user = TextInput::new(Vec::new())
            .with_kind(TextInputKind::Username)
            .with_id(USERNAME_KEY)
            .with_on_change_path(USERNAME_KEY)
            .with_placeholder(&t("settings-users-placeholder-username"));
        let password = TextInput::local()
            .with_kind(TextInputKind::Password)
            .with_id("users.form.password")
            .with_external_buffer(self.password_buffer.clone())
            .with_secret(true)
            .with_placeholder(&t("settings-users-placeholder-password"));
        let buttons = Row::new()
            .push(Box::new(Button::new(
                "settings.users.save",
                &t("settings-users-create"),
                Some(SAVE_KEY),
            )))
            .push(Box::new(Button::new(
                "settings.users.cancel",
                &t("settings-users-cancel"),
                Some(CANCEL_KEY),
            )))
            .push(Box::new(Spacer::new()));
        card(
            &t("settings-users-add-form-title"),
            rows(vec![
                field_row(&t("settings-users-full-name"), Box::new(full)),
                field_row(&t("settings-users-username"), Box::new(user)),
                field_row(&t("settings-users-password"), Box::new(password)),
                caption(&t("settings-users-password-note")),
                Box::new(buttons),
            ]),
        )
    }

    fn user_row(&self, user: &DeviceUser) -> El {
        let role_key = format!("settings.users.role.{}", user.username);
        let auto_key = format!("settings.users.autologin.{}", user.username);
        let remove_key = format!("settings.users.remove.{}", user.username);
        let confirm_key = format!("settings.users.confirm_remove.{}", user.username);
        let keep_key = format!("settings.users.confirm_remove_keep.{}", user.username);
        let mut actions: Vec<Button> = Vec::new();
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
            actions.push(Button::new(&role_key, &role_action, Some(&role_key)));
            actions.push(Button::new(&auto_key, &auto_action, Some(&auto_key)));
            if self.confirm_remove_user.as_deref() == Some(user.username.as_str()) {
                actions.push(Button::new(
                    &keep_key,
                    &t("settings-users-remove-keep-files"),
                    Some(&keep_key),
                ));
                actions.push(
                    Button::new(
                        &confirm_key,
                        &t("settings-users-remove-account-and-files"),
                        Some(&confirm_key),
                    )
                    .role(ButtonRole::Destructive),
                );
            } else {
                actions.push(
                    Button::new(
                        &remove_key,
                        &t("settings-users-remove-ellipsis"),
                        Some(&remove_key),
                    )
                    .role(ButtonRole::Destructive),
                );
            }
        }
        let role = if user.admin {
            t("settings-users-role-administrator")
        } else {
            t("settings-users-role-standard")
        };
        let detail = if user.current {
            t_args(
                "settings-users-signed-in-as",
                &[("username", user.username.as_str().into())],
            )
        } else {
            user.username.clone()
        };
        item_row(
            &user.full_name,
            vec![semantic_status_pill(
                &role,
                if user.admin {
                    StatusTone::Success
                } else {
                    StatusTone::Neutral
                },
            )],
            &[detail],
            actions,
        )
    }

    fn identity_row(&self, id: &IdentitySummary) -> El {
        let activate = format!("settings.users.identity.activate.{}", id.id);
        let remove = format!("settings.users.identity.remove.{}", id.id);
        let confirm = format!("settings.users.identity.confirm_remove.{}", id.id);
        let protect = format!("settings.users.identity.protect.{}", id.id);
        let name = if id.display_name.is_empty() {
            &id.id
        } else {
            &id.display_name
        };
        let status = if id.active {
            t("settings-users-identity-active")
        } else {
            t("settings-users-identity-available")
        };
        let mut actions: Vec<Button> = Vec::new();
        if !id.active {
            actions.push(Button::new(
                &activate,
                &t("settings-users-identity-use"),
                Some(&activate),
            ));
            actions.push(
                if self.confirm_remove_identity.as_deref() == Some(id.id.as_str()) {
                    Button::new(
                        &confirm,
                        &t("settings-users-identity-remove"),
                        Some(&confirm),
                    )
                    .role(ButtonRole::Destructive)
                } else {
                    Button::new(&remove, &t("settings-users-remove-ellipsis"), Some(&remove))
                        .role(ButtonRole::Destructive)
                },
            );
        }
        if id.protection == KeyProtection::LegacyPlaintext {
            actions.push(Button::new(
                &protect,
                &t("settings-users-identity-protect"),
                Some(&protect),
            ));
        }
        item_row(
            name,
            vec![semantic_status_pill(
                &status,
                if id.active {
                    StatusTone::Success
                } else {
                    StatusTone::Neutral
                },
            )],
            &[protection_label(id.protection)],
            actions,
        )
    }

    fn reload(&mut self) {
        self.users = users_for_display();
        if let Ok(dir) = runtime_identity::identity_dir_from_env() {
            self.identities = runtime_identity::list_identities(&dir).unwrap_or_default();
        }
    }

    fn apply(&mut self, result: Result<(), String>) {
        if let crate::actions::LocalOperationStatus::Running { token } = self.operation_status.get()
            && token.starts_with(crate::actions::USER_OPERATION_TYPE)
        {
            self.operation_status.set(match &result {
                Ok(()) => crate::actions::LocalOperationStatus::Succeeded {
                    token,
                    message: t("settings-users-updated"),
                },
                Err(error) => crate::actions::LocalOperationStatus::Failed {
                    token,
                    message: error.clone(),
                },
            });
        }
        self.error = result.err();
        self.reload();
        self.structure_dirty = true;
    }

    fn sync_form(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if take_bool(state, root, ADD_KEY) {
            self.form_open = true;
            seed_form(state, root);
            self.structure_dirty = true;
        }
        if take_bool(state, root, CANCEL_KEY) {
            self.form_open = false;
            clear_form(state, root);
            if let Ok(mut password) = self.password_buffer.lock() {
                password.clear();
            }
            self.structure_dirty = true;
        }
        if take_bool(state, root, SAVE_KEY) {
            let full = read_str_or_text(state, root, FULL_NAME_KEY).unwrap_or_default();
            let username = read_str_or_text(state, root, USERNAME_KEY).unwrap_or_default();
            let password = Zeroizing::new(
                self.password_buffer
                    .lock()
                    .map(|mut value| std::mem::take(&mut *value))
                    .unwrap_or_default(),
            );
            clear_form(state, root);
            let mut bytes = password.as_bytes().to_vec();
            let result = self
                .user_admin
                .create(username.trim(), full.trim(), &mut bytes);
            bytes.fill(0);
            if result.is_ok() {
                self.form_open = false;
            }
            self.apply(result);
        }
    }

    fn sync_users(&mut self, state: &mut AutoCommit, root: &ObjId) {
        for user in self.users.clone() {
            let role = format!("settings.users.role.{}", user.username);
            let auto = format!("settings.users.autologin.{}", user.username);
            let remove = format!("settings.users.remove.{}", user.username);
            let confirm = format!("settings.users.confirm_remove.{}", user.username);
            let keep = format!("settings.users.confirm_remove_keep.{}", user.username);
            if take_bool(state, root, &role) {
                let result = self.user_admin.set_admin(&user.username, !user.admin);
                self.apply(result);
            }
            if take_bool(state, root, &auto) {
                let result = self
                    .user_admin
                    .set_autologin(&user.username, !user.automatic_login);
                self.apply(result);
            }
            if take_bool(state, root, &remove) {
                self.confirm_remove_user = Some(user.username.clone());
                self.structure_dirty = true;
            }
            if take_bool(state, root, &confirm) {
                self.confirm_remove_user = None;
                let result = self.user_admin.delete(&user, true);
                self.apply(result);
            }
            if take_bool(state, root, &keep) {
                self.confirm_remove_user = None;
                let result = self.user_admin.delete(&user, false);
                self.apply(result);
            }
        }
    }

    fn sync_identities(&mut self, state: &mut AutoCommit, root: &ObjId) {
        for id in self.identities.clone() {
            let activate = format!("settings.users.identity.activate.{}", id.id);
            let remove = format!("settings.users.identity.remove.{}", id.id);
            let confirm = format!("settings.users.identity.confirm_remove.{}", id.id);
            let protect = format!("settings.users.identity.protect.{}", id.id);
            if take_bool(state, root, &activate) {
                let result = self
                    .identity_admin
                    .as_ref()
                    .ok_or_else(|| t("settings-users-identity-store-unavailable"))
                    .and_then(|admin| admin.activate(&id.id));
                self.apply(result);
            }
            if take_bool(state, root, &remove) {
                self.confirm_remove_identity = Some(id.id.clone());
                self.structure_dirty = true;
            }
            if take_bool(state, root, &confirm) {
                self.confirm_remove_identity = None;
                let result = self
                    .identity_admin
                    .as_ref()
                    .ok_or_else(|| t("settings-users-identity-store-unavailable"))
                    .and_then(|admin| admin.remove(&id.id));
                self.apply(result);
            }
            if take_bool(state, root, &protect) {
                let result = self
                    .identity_admin
                    .as_ref()
                    .ok_or_else(|| t("settings-users-identity-store-unavailable"))
                    .and_then(|admin| admin.protect(&id.id));
                self.apply(result);
            }
        }
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(UsersPane::new())
}

fn take_bool(state: &mut AutoCommit, root: &ObjId, key: &str) -> bool {
    let value = read_bool(state, root, key).unwrap_or(false);
    if value {
        let _ = state.put(root, key, false);
    }
    value
}

fn seed_form(state: &mut AutoCommit, root: &ObjId) {
    for key in FORM_KEYS {
        let _ = crate::ensure_text_object(state, root, key);
    }
}

fn clear_form(state: &mut AutoCommit, root: &ObjId) {
    for key in FORM_KEYS {
        crate::clear_text_object(state, root, key);
    }
}

impl Widget for UsersPane {
    fn sync(&mut self, shared: &mut AppModel, root: &ModelPath) {
        let (shared, _) = ui::doc::document(shared, root);
        let isolated = self.isolate_document;
        let mut state = if isolated {
            std::mem::take(&mut self.local_doc)
        } else {
            std::mem::take(shared)
        };
        let root = automerge::ROOT;
        if let Some(crate::actions::LocalPaneCommand::User(operation)) = self.command_state.get() {
            self.command_state.set(None);
            let expected_token = crate::actions::local_operation_token_for(
                crate::actions::USER_OPERATION_TYPE,
                &operation,
            );
            let token_matches = matches!(
                self.operation_status.get(),
                crate::actions::LocalOperationStatus::Running { token }
                    if token == expected_token
            );
            let username = match &operation {
                crate::actions::UserOperation::SetAdmin { username, .. }
                | crate::actions::UserOperation::SetAutologin { username, .. }
                | crate::actions::UserOperation::Remove { username, .. } => username,
            };
            let user = self
                .users
                .iter()
                .find(|user| user.username == *username && !user.current)
                .cloned();
            if !token_matches {
                self.error = Some(t("settings-users-stale-command"));
                self.structure_dirty = true;
            } else if let Some(user) = user {
                let result = match operation {
                    crate::actions::UserOperation::SetAdmin { admin, .. } => {
                        if user.admin == admin {
                            Ok(())
                        } else {
                            self.user_admin.set_admin(&user.username, admin)
                        }
                    }
                    crate::actions::UserOperation::SetAutologin { enabled, .. } => {
                        if user.automatic_login == enabled {
                            Ok(())
                        } else {
                            self.user_admin.set_autologin(&user.username, enabled)
                        }
                    }
                    crate::actions::UserOperation::Remove { remove_files, .. } => {
                        self.user_admin.delete(&user, remove_files)
                    }
                };
                self.apply(result);
            } else {
                self.apply(Err(t("settings-users-account-gone")));
            }
        }
        self.sync_form(&mut state, &root);
        self.sync_users(&mut state, &root);
        self.sync_identities(&mut state, &root);
        if let Some(c) = &mut self.composed {
            c.sync(&mut state, &root);
        }
        if isolated {
            self.local_doc = state;
        } else {
            *shared = state;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        // `child` was just created in this same tree, so this call is not
        // reachable to fail; if it (impossibly) did, wrap it in itself
        // rather than panic this panic=abort process.
        let node = taffy
            .new_with_children(
                taffy::Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::auto(),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x + l.size.width),
            origin.y + f64::from(l.location.y + l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&child) = children.first()
        {
            c.apply_layout(taffy, child, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&child) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, child));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }
    fn name(&self) -> &'static str {
        "UsersPane"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self
            .composed
            .as_mut()
            .is_some_and(|c| c.take_structure_dirty());
        own || child
    }
}
/// Fixture data for captures: two login accounts and a signing identity.
#[cfg(not(feature = "thin"))]
mod fixture {
    use super::{
        Arc, DeviceUser, IdentityAdmin, IdentitySummary, KeyProtection, UserAdmin, UsersPane,
    };

    struct Bench;

    impl UserAdmin for Bench {
        fn create(
            &self,
            _username: &str,
            _full_name: &str,
            _password: &mut [u8],
        ) -> Result<(), String> {
            Ok(())
        }
        fn delete(&self, _user: &DeviceUser, _remove_files: bool) -> Result<(), String> {
            Ok(())
        }
        fn set_admin(&self, _username: &str, _admin: bool) -> Result<(), String> {
            Ok(())
        }
        fn set_autologin(&self, _username: &str, _enabled: bool) -> Result<(), String> {
            Ok(())
        }
    }

    impl IdentityAdmin for Bench {
        fn activate(&self, _id: &str) -> Result<(), String> {
            Ok(())
        }
        fn remove(&self, _id: &str) -> Result<(), String> {
            Ok(())
        }
        fn protect(&self, _id: &str) -> Result<(), String> {
            Ok(())
        }
    }

    impl UsersPane {
        /// The pane over fixed accounts and identities: what a capture shows,
        /// the same on every machine.
        pub(crate) fn for_shot() -> Self {
            let users = vec![
                DeviceUser {
                    username: "ada".into(),
                    full_name: "Ada Lovelace".into(),
                    uid: 1000,
                    admin: true,
                    current: true,
                    automatic_login: true,
                },
                DeviceUser {
                    username: "grace".into(),
                    full_name: "Grace Hopper".into(),
                    uid: 1001,
                    admin: false,
                    current: false,
                    automatic_login: false,
                },
            ];
            let identities = vec![
                IdentitySummary {
                    id: "personal".into(),
                    display_name: "Ada Lovelace".into(),
                    public_key: [0x3a; 32],
                    threshold: 2,
                    total_shares: 3,
                    active: true,
                    protection: KeyProtection::SecretService,
                },
                IdentitySummary {
                    id: "work".into(),
                    display_name: "Ada at Analytical Engines".into(),
                    public_key: [0x7c; 32],
                    threshold: 2,
                    total_shares: 3,
                    active: false,
                    protection: KeyProtection::SecretService,
                },
            ];
            Self::with_data(users, identities, Arc::new(Bench), Arc::new(Bench))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ReadDoc;
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
        }
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

    /// Login-user parsing, two-stage removal, identity switching/protection and passwords kept out of the document.
    #[test]
    fn user_accounts_parse_and_mutate_through_narrow_backends() {
        // Case `parses_real_login_users`.
        {
            let admins = admins_from_group("wheel:x:10:ada\n");
            let users = read_passwd_users(
                "root:x:0:0:root:/root:/bin/bash\nada:x:1000:1000:Ada Lovelace:/home/ada:/bin/bash\n",
                Some("ada"),
                &admins,
            );
            assert_eq!(users.len(), 1);
            assert!(users[0].admin && users[0].current);
            assert_eq!(users[0].uid, 1000);
        }

        // Case `removal_is_two_stage_and_never_targets_current_user`.
        {
            let users = Arc::new(RecordingAdmin::default());
            let ids = Arc::new(RecordingIds::default());
            let mut pane = UsersPane::with_data(
                vec![user("ada", true), user("bob", false)],
                vec![identity("personal", true)],
                users.clone(),
                ids,
            );
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            doc.put(&root, "settings.users.remove.bob", true).unwrap();
            pane.sync(&mut doc, &root);
            assert!(users.0.lock().unwrap().is_empty());
            doc.put(&root, "settings.users.confirm_remove.bob", true)
                .unwrap();
            pane.sync(&mut doc, &root);
            assert_eq!(users.0.lock().unwrap().as_slice(), ["delete:bob:true"]);
        }

        // Case `identity_switch_uses_narrow_backend`.
        {
            let users = Arc::new(RecordingAdmin::default());
            let ids = Arc::new(RecordingIds::default());
            let mut pane = UsersPane::with_data(
                vec![user("ada", true)],
                vec![identity("personal", true), identity("work", false)],
                users,
                ids.clone(),
            );
            let mut doc = AutoCommit::new();
            doc.put(
                &automerge::ROOT,
                "settings.users.identity.activate.work",
                true,
            )
            .unwrap();
            pane.sync(&mut doc, &automerge::ROOT);
            assert_eq!(ids.0.lock().unwrap().as_slice(), ["activate:work"]);
        }

        // Case `legacy_identity_protection_uses_the_narrow_backend`.
        {
            let users = Arc::new(RecordingAdmin::default());
            let ids = Arc::new(RecordingIds::default());
            let mut legacy = identity("personal", true);
            legacy.protection = KeyProtection::LegacyPlaintext;
            let mut pane =
                UsersPane::with_data(vec![user("ada", true)], vec![legacy], users, ids.clone());
            let mut doc = AutoCommit::new();
            doc.put(
                &automerge::ROOT,
                "settings.users.identity.protect.personal",
                true,
            )
            .unwrap();
            pane.sync(&mut doc, &automerge::ROOT);
            assert_eq!(ids.0.lock().unwrap().as_slice(), ["protect:personal"]);
        }

        // Case `account_password_never_enters_the_settings_document`.
        {
            let users = Arc::new(RecordingAdmin::default());
            let ids = Arc::new(RecordingIds::default());
            let mut pane = UsersPane::with_data(
                vec![user("ada", true)],
                vec![identity("personal", true)],
                users,
                ids,
            );
            *pane.password_buffer.lock().unwrap() = "correct horse battery staple".into();
            let mut doc = AutoCommit::new();
            doc.put(&automerge::ROOT, SAVE_KEY, true).unwrap();
            pane.sync(&mut doc, &automerge::ROOT);
            assert!(
                doc.get(&automerge::ROOT, "users.form.password")
                    .unwrap()
                    .is_none()
            );
            assert!(pane.password_buffer.lock().unwrap().is_empty());
        }
    }
}
