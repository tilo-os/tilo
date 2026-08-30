//! Local login accounts and qOS signing identities.
//!
//! A Unix login account owns a home/session; it may contain several qOS
//! identities (personal, work, pseudonymous). Local-account mutations go
//! through AccountsService and polkit. Identity activation writes only a
//! non-secret selector; the launcher mounts one exact record into new apps.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::sync::{Arc, Mutex};

use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use runtime_identity::{IdentitySummary, KeyProtection};
use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{Button, Column, Row, Spacer, Text, TextInput, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};
use zeroize::Zeroizing;

use crate::widgets::{El, caption, card, card_with_action, rows, setting_row, status_pill};

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
    fn create(&self, username: &str, full_name: &str, password: &mut [u8]) -> Result<(), String>;
    fn delete(&self, user: &DeviceUser, remove_files: bool) -> Result<(), String>;
    fn set_admin(&self, username: &str, admin: bool) -> Result<(), String>;
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
            (f.len() >= 4 && matches!(f[0], "wheel" | "sudo")).then_some(f[3])
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
            let uid = f[2].parse::<u64>().ok()?;
            if !(1000..60000).contains(&uid) || f[6].ends_with("nologin") || f[6].ends_with("false")
            {
                return None;
            }
            let full = f[4].split(',').next().unwrap_or("").trim();
            Some(DeviceUser {
                username: f[0].to_string(),
                full_name: if full.is_empty() { f[0] } else { full }.to_string(),
                uid,
                admin: admins.contains(f[0]),
                current: current_user == Some(f[0]),
                automatic_login: false,
            })
        })
        .collect()
}

fn users_for_display() -> Vec<DeviceUser> {
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
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    #[cfg(test)]
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
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let mut cards: Vec<El> = Vec::new();
        if let Some(error) = &self.error {
            cards.push(card(
                "Could not apply that change",
                rows(vec![caption(error)]),
            ));
        }
        if self.form_open {
            cards.push(self.add_form());
        }
        let add = Box::new(Button::new("settings.users.add", "Add user", Some(ADD_KEY)));
        let people = if self.users.is_empty() {
            rows(vec![caption(
                "No regular login accounts found on this device.",
            )])
        } else {
            rows(self.users.iter().map(|u| self.user_row(u)).collect())
        };
        cards.push(card_with_action("Login accounts", add, people));

        let identity_rows = if self.identities.is_empty() {
            rows(vec![caption(
                "No signing identity is available. Identity setup runs in your personal session; it never runs as root.",
            )])
        } else {
            rows(
                self.identities
                    .iter()
                    .map(|i| self.identity_row(i))
                    .collect(),
            )
        };
        cards.push(card("Your qOS identities", identity_rows));
        let mut protection_rows: Vec<El> = vec![caption(
            "Secret Service is preferred. If the desktop keyring is locked, signing pauses and asks the session to unlock it; qOS never silently copies that key into an app.",
        )];
        if self
            .identities
            .iter()
            .any(|identity| identity.protection == KeyProtection::BrokerFile)
        {
            protection_rows.push(caption(
                "This session is using the compatible private-file fallback for at least one identity. It is isolated from apps by the signing broker, but is not encrypted at rest. Install and unlock a freedesktop Secret Service to protect future identities in the desktop keyring.",
            ));
        }
        if self
            .identities
            .iter()
            .any(|identity| identity.protection == KeyProtection::HardwareBacked)
        {
            protection_rows.push(caption(
                "Hardware-backed means the installed provider actually sealed this identity. The presence of a TPM alone is never treated as proof.",
            ));
        }
        cards.push(card("Signing protection", rows(protection_rows)));
        cards.push(card(
            "Identity boundary",
            rows(vec![caption(
                "A login account may have several identities. The active identity is used only by newly launched apps; running apps keep their existing author. Workspaces may restrict which identity can enter, but cannot read another identity's key. Apps receive a short-lived signing connection for the selected identity—never the private key or identity directory.",
            )]),
        ));
        crate::widgets::cards(cards)
    }

    fn add_form(&self) -> El {
        let full = TextInput::new(Vec::new())
            .with_id(FULL_NAME_KEY)
            .with_on_change_path(FULL_NAME_KEY)
            .with_placeholder("Full name")
            .with_font_size(13.0);
        let user = TextInput::new(Vec::new())
            .with_id(USERNAME_KEY)
            .with_on_change_path(USERNAME_KEY)
            .with_placeholder("login-name")
            .with_font_size(13.0);
        let password = TextInput::local()
            .with_id("users.form.password")
            .with_external_buffer(self.password_buffer.clone())
            .with_secret(true)
            .with_placeholder("At least 10 characters")
            .with_font_size(13.0);
        let buttons = Row::new()
            .push(Box::new(Button::new(
                "settings.users.save",
                "Create",
                Some(SAVE_KEY),
            )))
            .push(Box::new(Button::new(
                "settings.users.cancel",
                "Cancel",
                Some(CANCEL_KEY),
            )))
            .push(Box::new(Spacer::new()));
        card(
            "Add login account",
            rows(vec![
                setting_row("Full name", Box::new(full)),
                setting_row("User name", Box::new(user)),
                setting_row("Password", Box::new(password)),
                caption(
                    "The password is scrubbed from Settings state before it crosses the authenticated system boundary. New accounts start as Standard.",
                ),
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
        let mut actions = Row::new().push(Box::new(Spacer::new()));
        if !user.current {
            actions = actions
                .push(Box::new(Button::new(
                    &role_key,
                    if user.admin {
                        "Make standard"
                    } else {
                        "Make administrator"
                    },
                    Some(&role_key),
                )))
                .push(Box::new(Button::new(
                    &auto_key,
                    if user.automatic_login {
                        "Disable automatic login"
                    } else {
                        "Enable automatic login"
                    },
                    Some(&auto_key),
                )));
            actions = if self.confirm_remove_user.as_deref() == Some(user.username.as_str()) {
                actions
                    .push(Box::new(Button::new(
                        &keep_key,
                        "Remove, keep files",
                        Some(&keep_key),
                    )))
                    .push(Box::new(Button::new(
                        &confirm_key,
                        "Remove account and files",
                        Some(&confirm_key),
                    )))
            } else {
                actions.push(Box::new(Button::new(
                    &remove_key,
                    "Remove…",
                    Some(&remove_key),
                )))
            };
        }
        let role = if user.admin {
            "Administrator"
        } else {
            "Standard"
        };
        let you = if user.current { " · signed in" } else { "" };
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(Box::new(
                    Row::new()
                        .push(Box::new(Text::new(&user.full_name).with_size(13.0)))
                        .push(Box::new(Spacer::new()))
                        .push(status_pill(
                            role,
                            if user.admin {
                                Theme::dark().status_success
                            } else {
                                Theme::dark().text_dim
                            },
                        )),
                ))
                .push(Box::new(
                    Text::new(&format!("{} · UID {}{}", user.username, user.uid, you))
                        .with_size(11.0)
                        .with_color(Theme::dark().text_dim),
                ))
                .push(Box::new(actions)),
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
        let key = id.public_key[..4]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let mut top = Row::new()
            .push(Box::new(Text::new(name).with_size(13.0)))
            .push(Box::new(Spacer::new()))
            .push(status_pill(
                if id.active { "Active" } else { "Available" },
                if id.active {
                    Theme::dark().status_success
                } else {
                    Theme::dark().text_dim
                },
            ));
        if !id.active {
            top = top.push(Box::new(Button::new(&activate, "Use", Some(&activate))));
            top = if self.confirm_remove_identity.as_deref() == Some(id.id.as_str()) {
                top.push(Box::new(Button::new(
                    &confirm,
                    "Remove identity",
                    Some(&confirm),
                )))
            } else {
                top.push(Box::new(Button::new(&remove, "Remove…", Some(&remove))))
            };
        }
        if id.protection == KeyProtection::LegacyPlaintext {
            top = top.push(Box::new(Button::new(
                &protect,
                "Protect now",
                Some(&protect),
            )));
        }
        let protection = id.protection.label();
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(Box::new(top))
                .push(Box::new(
                    Text::new(&format!(
                        "{} · key {}… · {}-of-{} · {}",
                        id.id, key, id.threshold, id.total_shares, protection
                    ))
                    .with_size(11.0)
                    .with_color(Theme::dark().text_dim),
                )),
        )
    }

    fn reload(&mut self) {
        self.users = users_for_display();
        if let Ok(dir) = runtime_identity::identity_dir_from_env() {
            self.identities = runtime_identity::list_identities(&dir).unwrap_or_default();
        }
    }

    fn apply(&mut self, result: Result<(), String>) {
        self.error = result.err();
        self.reload();
        self.structure_dirty = true;
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(UsersPane::new())
}

fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> bool {
    matches!(state.get(root, key), Ok(Some((Value::Scalar(v), _))) if matches!(v.as_ref(), automerge::ScalarValue::Boolean(true)))
}

fn take_bool(state: &mut AutoCommit, root: &ObjId, key: &str) -> bool {
    let value = read_bool(state, root, key);
    if value {
        let _ = state.put(root, key, false);
    }
    value
}

fn read_text(state: &AutoCommit, root: &ObjId, key: &str) -> String {
    match state.get(root, key) {
        Ok(Some((Value::Object(automerge::ObjType::Text), obj))) => {
            state.text(&obj).unwrap_or_default()
        }
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            automerge::ScalarValue::Str(s) => s.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    }
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
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
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
            let full = read_text(state, root, FULL_NAME_KEY);
            let username = read_text(state, root, USERNAME_KEY);
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

        for id in self.identities.clone() {
            let activate = format!("settings.users.identity.activate.{}", id.id);
            let remove = format!("settings.users.identity.remove.{}", id.id);
            let confirm = format!("settings.users.identity.confirm_remove.{}", id.id);
            let protect = format!("settings.users.identity.protect.{}", id.id);
            if take_bool(state, root, &activate) {
                let result = self
                    .identity_admin
                    .as_ref()
                    .ok_or_else(|| "identity store is unavailable".to_string())
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
                    .ok_or_else(|| "identity store is unavailable".to_string())
                    .and_then(|admin| admin.remove(&id.id));
                self.apply(result);
            }
            if take_bool(state, root, &protect) {
                let result = self
                    .identity_admin
                    .as_ref()
                    .ok_or_else(|| "identity store is unavailable".to_string())
                    .and_then(|admin| admin.protect(&id.id));
                self.apply(result);
            }
        }
        if let Some(c) = &mut self.composed {
            c.sync(state, root);
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
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&child) = children.first()
        {
            c.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                child,
                taffy,
            );
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

    #[test]
    fn parses_real_login_users() {
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

    #[test]
    fn removal_is_two_stage_and_never_targets_current_user() {
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

    #[test]
    fn identity_switch_uses_narrow_backend() {
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

    #[test]
    fn legacy_identity_protection_uses_the_narrow_backend() {
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

    #[test]
    fn account_password_never_enters_the_settings_document() {
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
