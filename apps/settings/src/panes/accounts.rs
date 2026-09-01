//! External Accounts pane — one inventory over the canonical account stores
//! that installed clients consume.
//!
//! ## What's wired
//!
//! - **Remove** loads the owning store, drops the account, and persists it.
//! - **Add email account** opens an inline form; **Save** builds an
//!   [`AccountConfig`] (auto-configuring IMAP/SMTP from the address) and
//!   appends it to [`MailStore`], persisting the same JSON Mail loads.
//!
//! ## Live vs. sample
//!
//! Production loads only real stores and renders an honest empty state.
//! Screenshot and widget tests may inject [`crate::accounts::sample_directory`]
//! explicitly; those rows are labelled examples and omit mutation controls.
//!
//! Adding CalDAV/WebDAV/Nextcloud accounts and editing existing ones need
//! their own forms and authorization flows. They are intentionally not
//! shipped as buttons that do not act.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Button, Column, Row, Spacer, Text, TextInput, TextInputKind, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::accounts::{
    AccountConfig, AccountDirectory, AccountKind, AccountView, DavStore, NextcloudStore,
};
use crate::widgets::{
    El, caption, card, card_with_action, hgap, icon_button, rows, setting_row, status_pill,
    value_row,
};

/// Key the "Add" button on the Email card writes to open the form.
pub const ADD_MAIL_KEY: &str = "settings.accounts.add.mail";
/// Keys the inline form's Save / Cancel buttons write.
pub const FORM_SAVE_KEY: &str = "settings.accounts.form.save";
pub const FORM_CANCEL_KEY: &str = "settings.accounts.form.cancel";
/// Text fields the inline form binds.
pub const FORM_NAME_KEY: &str = "accounts.form.name";
pub const FORM_EMAIL_KEY: &str = "accounts.form.email";
pub const FORM_USER_KEY: &str = "accounts.form.username";
pub const FORM_PASSWORD_KEY: &str = "accounts.form.password";
pub const REAUTH_PASSWORD_KEY: &str = "accounts.reauth.password";
pub const REAUTH_SAVE_KEY: &str = "settings.accounts.reauth.save";
pub const REAUTH_CANCEL_KEY: &str = "settings.accounts.reauth.cancel";
pub const REMOVE_CONFIRM_KEY: &str = "settings.accounts.remove.confirm";
pub const REMOVE_CANCEL_KEY: &str = "settings.accounts.remove.cancel";

/// Per-account Remove key.
fn remove_key(id: &str) -> String {
    format!("settings.accounts.remove.{id}")
}

fn reauth_key(id: &str) -> String {
    format!("settings.accounts.reauth.{id}")
}

/// External accounts pane.
pub struct AccountsPane {
    dir: AccountDirectory,
    sync_statuses: accounts::AccountSyncStatusStore,
    vault: accounts::SecretVault,
    /// Whether `dir` is the illustrative sample (the real stores are empty).
    is_sample: bool,
    form_open: bool,
    reauth_id: Option<String>,
    pending_remove: Option<(AccountKind, String)>,
    /// Ids we've already removed this session, so a stale key edge doesn't
    /// try to remove twice.
    removed: Vec<String>,
    structure_dirty: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for AccountsPane {
    fn default() -> Self {
        Self::new()
    }
}

impl AccountsPane {
    pub fn new() -> Self {
        // The live pane shows the *real* stores — empty until the user adds an
        // account. It never substitutes illustrative sample accounts: showing
        // fabricated `ada@example.com` rows would present made-up data on an
        // empty state. The deterministic `--shots`/tests use
        // `from_dir(sample_directory(), true)` for a rich still instead.
        Self {
            dir: AccountDirectory::load(),
            sync_statuses: accounts::AccountSyncStatusStore::load(),
            vault: accounts::SecretVault::open(),
            is_sample: false,
            form_open: false,
            reauth_id: None,
            pending_remove: None,
            removed: Vec::new(),
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Test / `--shots` constructor over an explicit directory. `is_sample`
    /// controls whether Remove buttons render (real accounts) or not
    /// (illustration).
    pub fn from_dir(dir: AccountDirectory, is_sample: bool) -> Self {
        Self {
            dir,
            sync_statuses: accounts::AccountSyncStatusStore::default(),
            vault: accounts::SecretVault::open(),
            is_sample,
            form_open: false,
            reauth_id: None,
            pending_remove: None,
            removed: Vec::new(),
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    pub fn is_sample(&self) -> bool {
        self.is_sample
    }

    pub fn form_open(&self) -> bool {
        self.form_open
    }

    pub fn directory(&self) -> &AccountDirectory {
        &self.dir
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let mut panel: Vec<El> = Vec::new();

        let mut intro: Vec<El> = Vec::new();
        intro.push(Box::new(
            Text::new(
                "Accounts are saved on this device and used by compatible installed apps. \
                 They are not shared with collaborators or a workspace. Email accounts \
                 can be added here today; calendar and file accounts appear when their \
                 owning app provisions them.",
            )
            .with_size(12.0),
        ));
        if self.vault.custody() == accounts::SecretCustody::LocalFile {
            intro.push(caption(
                "The desktop Secret Service is unavailable, so this session uses an owner-only local vault key. Install or unlock a Secret Service for stronger key custody.",
            ));
        }
        if self.is_sample {
            // Be honest that the visible rows are an illustration, not real
            // connected accounts — the store is empty until you add one.
            intro.push(caption(
                "You have no accounts yet. The entries below are examples of \
                 what you can add — use \u{201c}Add\u{201d} to create a real one.",
            ));
        } else if self.dir.is_empty() {
            // The live empty state: honest, no fabricated rows.
            intro.push(caption(
                "You have no accounts yet. Use \u{201c}Add\u{201d} to connect \
                 your first account.",
            ));
        }
        panel.push(card("Accounts on this device", rows(intro)));

        if self.form_open {
            panel.push(Self::mail_form());
        }
        if let Some(id) = &self.reauth_id {
            panel.push(Self::reauth_form(id));
        }
        if let Some((kind, id)) = &self.pending_remove
            && let Some(view) = self
                .dir
                .views_of(*kind)
                .into_iter()
                .find(|view| &view.id == id)
        {
            panel.push(card(
                "Remove account?",
                rows(vec![
                    caption(&format!(
                        "{} will be removed from this device. Locally cached copies may be removed by its apps; data on the provider is not deleted. The encrypted credential and local sync-health record are removed.",
                        view.label
                    )),
                    Box::new(
                        Row::new()
                            .push(Box::new(Button::new(
                                "settings.accounts.remove.confirm",
                                "Remove from this device",
                                Some(REMOVE_CONFIRM_KEY),
                            )))
                            .push(hgap(8.0))
                            .push(Box::new(Button::new(
                                "settings.accounts.remove.cancel",
                                "Cancel",
                                Some(REMOVE_CANCEL_KEY),
                            )))
                            .push(Box::new(Spacer::new())),
                    ),
                ]),
            ));
        }

        // Email card carries the live "Add" button.
        panel.push(self.mail_card());
        // The remaining kinds are read-only lists (add flows are follow-ups).
        panel.push(self.kind_card(AccountKind::CalDav));
        panel.push(self.kind_card(AccountKind::WebDav));
        panel.push(self.kind_card(AccountKind::Nextcloud));

        crate::widgets::cards(panel)
    }

    fn mail_card(&self) -> El {
        let title = format!(
            "{}  {}",
            AccountKind::Mail.glyph(),
            AccountKind::Mail.label()
        );
        let add_btn: El = Box::new(Button::new(
            "settings.accounts.add.mail",
            "Add email account",
            Some(ADD_MAIL_KEY),
        ));
        let views = self.dir.views_of(AccountKind::Mail);
        let body = self.kind_body(AccountKind::Mail, &views);
        card_with_action(&title, add_btn, body)
    }

    fn kind_card(&self, kind: AccountKind) -> El {
        let title = format!("{}  {}", kind.glyph(), kind.label());
        let views = self.dir.views_of(kind);
        card(&title, self.kind_body(kind, &views))
    }

    fn kind_body(&self, kind: AccountKind, views: &[AccountView]) -> El {
        if views.is_empty() {
            rows(vec![caption(&format!(
                "No {} accounts yet.",
                kind.label().to_lowercase()
            ))])
        } else {
            rows(views.iter().map(|v| self.account_row(v)).collect())
        }
    }

    /// One account: label + connection summary, status, and a Remove button
    /// (only for real accounts — sample rows are illustrative).
    ///
    /// Sample rows report an honest "Example" status rather than fabricating
    /// a live "Connected" — nothing is actually connected in the illustration.
    fn account_row(&self, view: &AccountView) -> El {
        let id = accounts::AccountId::from(view.id.clone());
        let runtime = self.sync_statuses.get(&id);
        let (status, status_color) = if self.is_sample {
            ("Example", ui::Theme::dark().text_dim)
        } else if !view.enabled {
            ("Disabled", ui::Theme::dark().text_dim)
        } else {
            match runtime.map(|status| status.health) {
                Some(accounts::AccountSyncHealth::Syncing) => {
                    ("Syncing", ui::Theme::dark().tokens.colors.accent[5])
                }
                Some(accounts::AccountSyncHealth::Healthy) => {
                    ("Up to date", ui::Theme::dark().status_success)
                }
                Some(accounts::AccountSyncHealth::Offline) => {
                    ("Offline", ui::Theme::dark().tokens.colors.semantic.warning)
                }
                Some(accounts::AccountSyncHealth::NeedsAuthentication) => (
                    "Sign in again",
                    ui::Theme::dark().tokens.colors.semantic.warning,
                ),
                Some(accounts::AccountSyncHealth::Error) => (
                    "Needs attention",
                    ui::Theme::dark().tokens.colors.semantic.danger,
                ),
                Some(accounts::AccountSyncHealth::Never) | None => {
                    ("Not checked yet", ui::Theme::dark().text_dim)
                }
            }
        };

        // Top line: the account label with the status chip and (for real
        // accounts) an explicit destructive action inline at the right. A single
        // leading `Text` keeps the trailing controls on the same line — a
        // multi-line block on the left would seize the row and wrap them
        // below. The connection summary goes on its own line beneath.
        let mut top = Row::new()
            .push(Box::new(Text::new(&view.label).with_size(13.0)))
            .push(Box::new(Spacer::new()))
            .push(status_pill(status, status_color));
        if !self.is_sample {
            let rk = remove_key(&view.id);
            let remove_id = format!("settings.accounts.remove.{}", view.id);
            if self
                .vault
                .get(&accounts::AccountId::from(view.id.clone()))
                .is_some_and(|secret| !secret.password.is_empty())
            {
                top = top.push(hgap(8.0)).push(icon_button(
                    &format!("settings.accounts.reauth.{}", view.id),
                    "\u{f0425}",
                    &reauth_key(&view.id),
                ));
            }
            // Removing an account is consequential even though it has a
            // confirmation step. Keep its intent visible and use the shared
            // danger treatment rather than hiding it behind an unlabeled
            // trash icon next to ordinary account maintenance.
            top = top.push(hgap(10.0)).push(Box::new(
                Button::new(&remove_id, "Remove", Some(&rk))
                    .danger()
                    .fit_content(34.0),
            ));
        }

        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(top))
            .push(Box::new(
                Text::new(&view.summary)
                    .with_size(11.0)
                    .with_color(ui::Theme::dark().text_dim),
            ));
        if !self.is_sample {
            let sync_line = runtime.map_or_else(
                || "No application has reported a sync for this account.".to_string(),
                |status| {
                    let when = status
                        .last_success_unix
                        .map_or_else(|| "never completed".into(), relative_sync_time);
                    match status.message.as_deref() {
                        Some(message) => format!("Last successful sync: {when} · {message}"),
                        None => format!("Last successful sync: {when}"),
                    }
                },
            );
            column = column.push(Box::new(
                Text::new(&sync_line)
                    .with_size(11.0)
                    .with_color(ui::Theme::dark().text_dim),
            ));
        }
        Box::new(column)
    }

    /// The inline add-email form: fields bound to CRDT keys plus Save/Cancel.
    fn mail_form() -> El {
        let name = TextInput::new(Vec::new())
            .with_kind(TextInputKind::Name)
            .with_id("accounts.form.name")
            .with_on_change_path(FORM_NAME_KEY)
            .with_placeholder("Display name")
            .with_font_size(13.0);
        let email = TextInput::new(Vec::new())
            .with_kind(TextInputKind::Email)
            .with_id("accounts.form.email")
            .with_on_change_path(FORM_EMAIL_KEY)
            .with_placeholder("you@example.com")
            .with_font_size(13.0);
        let user = TextInput::new(Vec::new())
            .with_kind(TextInputKind::Username)
            .with_id("accounts.form.username")
            .with_on_change_path(FORM_USER_KEY)
            .with_placeholder("Username (defaults to your address)")
            .with_font_size(13.0);
        let password = TextInput::new(Vec::new())
            .with_kind(TextInputKind::Password)
            .with_id("accounts.form.password")
            .with_on_change_path(FORM_PASSWORD_KEY)
            .with_secret(true)
            .with_placeholder("Password")
            .with_font_size(13.0);
        let buttons = Row::new()
            .push(Box::new(Button::new(
                "settings.accounts.form.save",
                "Save",
                Some(FORM_SAVE_KEY),
            )))
            .push(Box::new(Button::new(
                "settings.accounts.form.cancel",
                "Cancel",
                Some(FORM_CANCEL_KEY),
            )))
            .push(Box::new(Spacer::new()));
        card(
            "Add email account",
            rows(vec![
                setting_row("Display name", Box::new(name)),
                setting_row("Email address", Box::new(email)),
                setting_row("Username", Box::new(user)),
                setting_row("Password", Box::new(password)),
                caption(
                    "IMAP/SMTP servers are detected from your email address. \
                     Passwords go to the OS secret store, never the shared \
                     accounts file.",
                ),
                Box::new(buttons),
            ]),
        )
    }

    fn reauth_form(id: &str) -> El {
        let password = TextInput::new(Vec::new())
            .with_kind(TextInputKind::Password)
            .with_id("accounts.reauth.password")
            .with_on_change_path(REAUTH_PASSWORD_KEY)
            .with_secret(true)
            .with_placeholder("New password or app password")
            .with_font_size(13.0);
        card(
            "Replace saved password",
            rows(vec![
                value_row("Account", id),
                setting_row("Credential", Box::new(password)),
                caption(
                    "This replaces an existing password or app password in the OS account vault. OAuth authorization remains with the owning application.",
                ),
                Box::new(
                    Row::new()
                        .push(Box::new(Button::new(
                            "settings.accounts.reauth.save",
                            "Update credential",
                            Some(REAUTH_SAVE_KEY),
                        )))
                        .push(hgap(8.0))
                        .push(Box::new(Button::new(
                            "settings.accounts.reauth.cancel",
                            "Cancel",
                            Some(REAUTH_CANCEL_KEY),
                        )))
                        .push(Box::new(Spacer::new())),
                ),
            ]),
        )
    }

    /// Reload the real directory after a mutation. The live pane never
    /// substitutes the sample, so `is_sample` stays as constructed (always
    /// `false` for the live pane).
    fn reload(&mut self) {
        self.dir = AccountDirectory::load();
        self.sync_statuses = accounts::AccountSyncStatusStore::load();
        self.vault = accounts::SecretVault::open();
    }

    /// Persist a new mail account from the form fields. Returns whether an
    /// account was actually added (an empty address is a no-op).
    fn save_mail_account(name: &str, email: &str, username: &str, password: &str) -> bool {
        if email.trim().is_empty() {
            return false;
        }
        let mut config = AccountConfig {
            email: email.trim().to_string(),
            display_name: name.trim().to_string(),
            username: username.trim().to_string(),
            ..Default::default()
        };
        // Fills IMAP/SMTP hosts + username/display-name defaults from the
        // address, exactly as Mail's own add flow does.
        config.autofill_from_email();
        // Persist the connection settings into the shared canonical store (the
        // same store Mail reads). The `#[serde(skip)]` password never reaches
        // this JSON — it goes to the encrypted vault, keyed by the account id.
        let mut shared = accounts::AccountStore::load();
        let id = accounts::AccountId::from(config.email.clone());
        let mut stored = accounts::Account::new(id.clone(), "email");
        stored.display_name = if config.display_name.is_empty() {
            config.email.clone()
        } else {
            config.display_name.clone()
        };
        stored.set_config(&config);
        shared.upsert(stored);
        if let Err(e) = shared.save() {
            warn!(target: "qos::settings", op = "mail_store_save", error = %e, "failed to persist new mail account");
            return false;
        }
        // Seal the password in the vault so Mail can log in — the credential
        // now persists, encrypted, instead of being dropped on the floor.
        if !password.trim().is_empty() {
            let mut vault = accounts::SecretVault::open();
            vault.put(
                &id,
                &accounts::Secret::password(password.trim().to_string()),
            );
            vault.save();
        }
        true
    }

    /// Remove an account of `kind` by id from its owning store, persisting.
    fn remove_account(kind: AccountKind, id: &str) {
        let res = match kind {
            // Email + CalDAV live in the shared canonical store; remove from it
            // (and drop any sealed credential) so the removal is real.
            AccountKind::Mail | AccountKind::CalDav => {
                let account_id = accounts::AccountId::from(id.to_string());
                let mut shared = accounts::AccountStore::load();
                shared.remove(&account_id);
                shared.save()
            }
            AccountKind::WebDav => {
                let mut s = DavStore::load_from(DavStore::files_path());
                s.accounts.retain(|a| a.id != id);
                s.save_to(DavStore::files_path()).map(|_| ())
            }
            AccountKind::Nextcloud => {
                let mut s = NextcloudStore::load();
                s.accounts.retain(|a| a.id != id);
                s.save().map(|_| ())
            }
        };
        if let Err(e) = res {
            warn!(target: "qos::settings", op = "account_remove", error = %e, "failed to persist account removal");
        } else {
            let account_id = accounts::AccountId::from(id.to_string());
            let mut vault = accounts::SecretVault::open();
            if vault.remove(&account_id) {
                vault.save();
            }
            if let Err(error) = accounts::AccountSyncReporter::new(account_id).remove() {
                warn!(target: "qos::settings", op = "account_status_remove", %error, "failed to remove account sync status");
            }
        }
    }

    /// Open the add-email form (and seed its bound `Text` objects).
    fn handle_add_mail(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if read_bool(state, root, ADD_MAIL_KEY) {
            let _ = state.put(root, ADD_MAIL_KEY, automerge::ScalarValue::Boolean(false));
            if !self.form_open {
                self.form_open = true;
                seed_form_fields(state, root);
                self.structure_dirty = true;
            }
        }
    }

    fn handle_form_cancel(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if read_bool(state, root, FORM_CANCEL_KEY) {
            let _ = state.put(
                root,
                FORM_CANCEL_KEY,
                automerge::ScalarValue::Boolean(false),
            );
            if self.form_open {
                self.form_open = false;
                clear_form(state, root);
                self.structure_dirty = true;
            }
        }
    }

    /// Save the form → create + persist a real mail account.
    fn handle_form_save(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if read_bool(state, root, FORM_SAVE_KEY) {
            let _ = state.put(root, FORM_SAVE_KEY, automerge::ScalarValue::Boolean(false));
            let name = read_str(state, root, FORM_NAME_KEY).unwrap_or_default();
            let email = read_str(state, root, FORM_EMAIL_KEY).unwrap_or_default();
            let user = read_str(state, root, FORM_USER_KEY).unwrap_or_default();
            let password = read_str(state, root, FORM_PASSWORD_KEY).unwrap_or_default();
            if Self::save_mail_account(&name, &email, &user, &password) {
                self.form_open = false;
                clear_form(state, root);
                self.reload();
                self.structure_dirty = true;
            }
        }
    }

    /// Per-account reauthentication triggers for accounts with a stored
    /// password (sample rows never offer reauth).
    fn handle_reauth_triggers(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.is_sample {
            return;
        }
        let views: Vec<_> = [
            AccountKind::Mail,
            AccountKind::CalDav,
            AccountKind::WebDav,
            AccountKind::Nextcloud,
        ]
        .into_iter()
        .flat_map(|kind| self.dir.views_of(kind))
        .collect();
        for view in views {
            let has_password = self
                .vault
                .get(&accounts::AccountId::from(view.id.clone()))
                .is_some_and(|secret| !secret.password.is_empty());
            if !has_password {
                continue;
            }
            let key = reauth_key(&view.id);
            if read_bool(state, root, &key) {
                let _ = state.put(root, key, automerge::ScalarValue::Boolean(false));
                self.reauth_id = Some(view.id);
                let _ = crate::ensure_text_object(state, root, REAUTH_PASSWORD_KEY);
                self.structure_dirty = true;
            }
        }
    }

    fn handle_reauth_cancel(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if read_bool(state, root, REAUTH_CANCEL_KEY) {
            let _ = state.put(
                root,
                REAUTH_CANCEL_KEY,
                automerge::ScalarValue::Boolean(false),
            );
            self.reauth_id = None;
            crate::clear_text_object(state, root, REAUTH_PASSWORD_KEY);
            self.structure_dirty = true;
        }
    }

    fn handle_reauth_save(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if read_bool(state, root, REAUTH_SAVE_KEY) {
            let _ = state.put(
                root,
                REAUTH_SAVE_KEY,
                automerge::ScalarValue::Boolean(false),
            );
            if let Some(id) = self.reauth_id.take() {
                let password = read_str(state, root, REAUTH_PASSWORD_KEY).unwrap_or_default();
                if !password.trim().is_empty() {
                    let mut vault = accounts::SecretVault::open();
                    let account_id = accounts::AccountId::from(id);
                    let mut secret = vault.get(&account_id).unwrap_or_default();
                    secret.password = password.trim().to_string();
                    vault.put(&account_id, &secret);
                    vault.save();
                }
                crate::clear_text_object(state, root, REAUTH_PASSWORD_KEY);
                self.structure_dirty = true;
            }
        }
    }

    fn handle_remove_cancel(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if read_bool(state, root, REMOVE_CANCEL_KEY) {
            let _ = state.put(
                root,
                REMOVE_CANCEL_KEY,
                automerge::ScalarValue::Boolean(false),
            );
            self.pending_remove = None;
            self.structure_dirty = true;
        }
    }

    fn handle_remove_confirm(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if read_bool(state, root, REMOVE_CONFIRM_KEY) {
            let _ = state.put(
                root,
                REMOVE_CONFIRM_KEY,
                automerge::ScalarValue::Boolean(false),
            );
            if let Some((kind, id)) = self.pending_remove.take() {
                Self::remove_account(kind, &id);
                self.removed.push(id);
                self.reload();
                self.structure_dirty = true;
            }
        }
    }

    /// Per-account Remove buttons. Snapshot (kind, id, key) so we don't hold
    /// a borrow across the mutating store calls.
    fn handle_remove_triggers(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.is_sample {
            return;
        }
        let targets: Vec<(AccountKind, String, String)> = [
            AccountKind::Mail,
            AccountKind::CalDav,
            AccountKind::WebDav,
            AccountKind::Nextcloud,
        ]
        .into_iter()
        .flat_map(|kind| {
            self.dir
                .views_of(kind)
                .into_iter()
                .map(move |v| (kind, v.id.clone(), remove_key(&v.id)))
        })
        .collect();
        for (kind, id, key) in targets {
            if !read_bool(state, root, &key) || self.removed.contains(&id) {
                continue;
            }
            let _ = state.put(root, key.as_str(), automerge::ScalarValue::Boolean(false));
            self.pending_remove = Some((kind, id));
            self.structure_dirty = true;
            break;
        }
    }
}

fn relative_sync_time(unix: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64);
    let age = now.saturating_sub(unix).max(0);
    match age {
        0..=59 => "just now".into(),
        60..=3_599 => format!("{} min ago", age / 60),
        3_600..=86_399 => format!("{} h ago", age / 3_600),
        _ => format!("{} d ago", age / 86_400),
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(AccountsPane::new())
}

/// Build the pane from a caller-supplied directory (tests / deterministic
/// `--shots`). Treated as real accounts so Remove buttons render.
pub fn build_with(dir: &AccountDirectory) -> Box<dyn Widget> {
    Box::new(AccountsPane::from_dir(dir.clone(), false))
}

impl Widget for AccountsPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        self.handle_add_mail(state, root);
        self.handle_form_cancel(state, root);
        self.handle_form_save(state, root);
        self.handle_reauth_triggers(state, root);
        self.handle_reauth_cancel(state, root);
        self.handle_reauth_save(state, root);
        self.handle_remove_cancel(state, root);
        self.handle_remove_confirm(state, root);
        self.handle_remove_triggers(state, root);

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
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        // A wrapping-node failure would mean the taffy tree rejected a node it
        // just handed back to us; fall back to the child itself so layout
        // still proceeds, just without the outer flex column.
        let node = taffy.new_with_children(style, &[child]).unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // No computed layout for `node` means `build_layout` wasn't run first;
        // keep the previous rect rather than panicking.
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
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
            && let Some(&cn) = children.first()
        {
            c.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                cn,
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
        "AccountsPane"
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

fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> bool {
    matches!(
        state.get(root, key),
        Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))
    )
}

fn read_str(state: &AutoCommit, root: &ObjId, key: &str) -> Option<String> {
    match state.get(root, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        Ok(Some((Value::Object(automerge::ObjType::Text), obj))) => state.text(&obj).ok(),
        _ => None,
    }
}

/// The form's four editable text keys, in field order.
const FORM_FIELD_KEYS: [&str; 4] = [
    FORM_NAME_KEY,
    FORM_EMAIL_KEY,
    FORM_USER_KEY,
    FORM_PASSWORD_KEY,
];

/// Seed the form's bound `Text` objects so the fields can actually be typed
/// into (a `TextInput` persists edits only through an existing Text object).
fn seed_form_fields(state: &mut AutoCommit, root: &ObjId) {
    for key in FORM_FIELD_KEYS {
        let _ = crate::ensure_text_object(state, root, key);
    }
}

/// Blank the form's bound text keys so a reopened form starts empty — clearing
/// the `Text` objects' contents (not replacing them with scalars, which would
/// break the next round of typing).
fn clear_form(state: &mut AutoCommit, root: &ObjId) {
    for key in FORM_FIELD_KEYS {
        crate::clear_text_object(state, root, key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::sample_directory;
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};
    use ui::Theme;

    fn count_named(w: &dyn Widget, name: &str) -> usize {
        let mut n = usize::from(w.name() == name);
        for c in w.children() {
            n += count_named(c, name);
        }
        n
    }

    fn button_labels(w: &dyn Widget, labels: &mut Vec<String>) {
        if w.name() == "Button"
            && let Some(label) = w.test_id()
        {
            labels.push(label.to_string());
        }
        for c in w.children() {
            button_labels(c, labels);
        }
    }

    fn lay_out(pane: &mut dyn Widget) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }

    #[test]
    fn sample_directory_renders_read_only_without_remove_buttons() {
        // The sample is illustration: exactly one live Add button (Email),
        // and no Remove buttons.
        let mut pane = AccountsPane::from_dir(sample_directory(), true);
        lay_out(&mut pane);
        assert_eq!(
            count_named(&pane, "Button"),
            1,
            "only the Email Add button is live over the sample"
        );
    }

    #[test]
    fn real_directory_renders_remove_buttons() {
        // Without stored passwords, real rows offer removal but do not invent
        // a reauthentication action.
        let dir = sample_directory(); // 2 mail + 1 cal + 1 nextcloud = 4 accounts
        let mut pane = AccountsPane::from_dir(dir, false);
        lay_out(&mut pane);
        // 1 Add + 4 Remove.
        assert_eq!(count_named(&pane, "Button"), 5);
    }

    #[test]
    fn account_removal_is_labeled_and_visibly_destructive() {
        // Real accounts get an explicit Remove action rather than a lone
        // trash glyph. The confirmation flow still owns the mutation; this
        // assertion guards the decision point users see before it.
        let mut pane = AccountsPane::from_dir(sample_directory(), false);
        lay_out(&mut pane);
        let mut labels = Vec::new();
        button_labels(&pane, &mut labels);
        assert_eq!(
            labels
                .iter()
                .filter(|label| label.as_str() == "Remove")
                .count(),
            4,
            "one explicit Remove action per real account"
        );
        assert!(
            labels.contains(&"Add email account".to_string()),
            "the account-type action names what it adds"
        );
    }

    #[test]
    fn add_button_opens_the_inline_form() {
        let mut pane = AccountsPane::from_dir(sample_directory(), true);
        assert!(!pane.form_open());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let _ = doc.put(&root, ADD_MAIL_KEY, true);
        pane.sync(&mut doc, &root);
        assert!(pane.form_open(), "Add opens the form");
        assert!(
            pane.take_structure_dirty(),
            "opening the form requests a rebuild"
        );
        // Cancel closes it.
        let _ = doc.put(&root, FORM_CANCEL_KEY, true);
        pane.sync(&mut doc, &root);
        assert!(!pane.form_open(), "Cancel closes the form");
    }

    /// The live pane (`new()`, over an empty store) must NOT fabricate the
    /// illustrative sample — it renders an honest empty state, mirroring the
    /// Printers pane's no-fabrication rule. Asserts the real production path,
    /// with `XDG_CONFIG_HOME` redirected to an empty temp dir.
    #[test]
    fn live_pane_over_empty_store_does_not_fabricate_sample() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("accounts_empty_live");
        let pane = AccountsPane::new();
        assert!(!pane.is_sample(), "the live pane never flags the sample");
        assert!(
            pane.directory().is_empty(),
            "an empty store renders empty — no fabricated ada@example.com rows"
        );
        // And no account of any kind leaked in from the sample.
        for kind in [
            AccountKind::Mail,
            AccountKind::CalDav,
            AccountKind::WebDav,
            AccountKind::Nextcloud,
        ] {
            assert!(
                pane.directory().views_of(kind).is_empty(),
                "{kind:?} must be empty"
            );
        }
    }

    #[test]
    fn save_with_empty_email_is_a_noop() {
        // Guard: saving a blank address must not create an account (and must
        // not touch disk). We assert the pure guard directly.
        assert!(!AccountsPane::save_mail_account("Nobody", "  ", "", ""));
    }
}
