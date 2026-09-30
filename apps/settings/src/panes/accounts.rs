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

use std::collections::HashMap;
use ui::doc::read_bool;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId, transaction::Transactable};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Button, Column, Row, Spacer, Text, TextInput, TextInputKind, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::accounts::{AccountConfig, AccountDirectory, AccountKind, AccountRoot, AccountView};
use crate::widgets::{
    El, caption, card, card_with_action, rows, setting_row, status_pill, value_row,
};

mod cloud_signin;
pub use cloud_signin::{BrowserOpener, PortalOpener};
#[cfg(test)]
mod signin_tests;

/// Height (logical px) of the per-account maintenance buttons. The shared
/// `Button` default — the same height as the card's trailing "Add account"
/// button and every dropdown in Settings — so one card never mixes two
/// control heights.
const ACTION_BUTTON_H: f32 = 40.0;

/// Key the "Add" button on the Email card writes to open the form.
pub const ADD_MAIL_KEY: &str = "settings.accounts.add.mail";
pub const ADD_CALDAV_KEY: &str = "settings.accounts.add.caldav";
pub const ADD_WEBDAV_KEY: &str = "settings.accounts.add.webdav";
pub const ADD_NEXTCLOUD_KEY: &str = "settings.accounts.add.nextcloud";
pub const ADD_GOOGLE_DRIVE_KEY: &str = "settings.accounts.add.google-drive";
pub const ADD_ONEDRIVE_KEY: &str = "settings.accounts.add.onedrive";
pub const ADD_DROPBOX_KEY: &str = "settings.accounts.add.dropbox";
pub const ADD_ICLOUD_DRIVE_KEY: &str = "settings.accounts.add.icloud-drive";
/// Keys the inline form's Save / Cancel buttons write.
pub const FORM_SAVE_KEY: &str = "settings.accounts.form.save";
pub const FORM_CANCEL_KEY: &str = "settings.accounts.form.cancel";
pub const REAUTH_SAVE_KEY: &str = "settings.accounts.reauth.save";
pub const REAUTH_CANCEL_KEY: &str = "settings.accounts.reauth.cancel";
pub const REMOVE_CONFIRM_KEY: &str = "settings.accounts.remove.confirm";
pub const REMOVE_CANCEL_KEY: &str = "settings.accounts.remove.cancel";

/// Per-account Remove key.
fn remove_key(id: &str) -> String {
    format!("settings.accounts.remove.{id}")
}

pub(crate) fn reauth_key(id: &str) -> String {
    format!("settings.accounts.reauth.{id}")
}

fn edit_key(id: &str) -> String {
    format!("settings.accounts.edit.{id}")
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the resource action binding mirrors the framework's distinct address, label, operation, and destructive fields"
)]
fn account_action_button(
    control_id: &str,
    label: &str,
    resource_id: String,
    resource_label: String,
    action_id: &str,
    kind: tilo_actions::OperationKind,
    operation: crate::actions::AccountOperation,
    destructive: bool,
) -> Button {
    let button = Button::new(control_id, label, None);
    match crate::actions::account_operation_binding(
        resource_id,
        resource_label,
        action_id,
        label,
        kind,
        &operation,
        destructive,
    ) {
        Some(binding) => button.on_action(binding),
        None => button,
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
                Some(Err("connection check timed out".into()))
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
        }
    }
}

/// External accounts pane.
#[allow(
    clippy::struct_excessive_bools,
    reason = "the flags are independent form and asynchronous-operation latches, not a single state machine"
)]
pub struct AccountsPane {
    account_root: AccountRoot,
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
    commands: ui::LocalState<Option<AccountViewCommand>>,
    form_submit: bool,
    form_cancel: bool,
    reauth_submit: bool,
    reauth_cancel: bool,
    reauth_id: Option<String>,
    account_tester: std::sync::Arc<dyn crate::account_probe::AccountTester>,
    form_probe: Option<ProbeJob>,
    form_candidate: Option<AccountDraft>,
    reauth_probe: Option<ProbeJob>,
    reauth_candidate: Option<(accounts::AccountId, accounts::Secret)>,
    pending_remove: Option<(AccountKind, String)>,
    /// A running browser or Apple ID sign-in.
    sign_in: Option<cloud_signin::SignInFlow>,
    custodian: account_custody::Custodian,
    opener: std::sync::Arc<dyn BrowserOpener>,
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
            commands: ui::LocalState::new(None),
            form_submit: false,
            form_cancel: false,
            reauth_submit: false,
            reauth_cancel: false,
            reauth_id: None,
            account_tester: std::sync::Arc::new(crate::account_probe::LiveAccountTester),
            form_probe: None,
            form_candidate: None,
            reauth_probe: None,
            reauth_candidate: None,
            pending_remove: None,
            sign_in: None,
            custodian: account_root.custodian(),
            opener: std::sync::Arc::new(PortalOpener),
            removed: Vec::new(),
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
            account_root,
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
            commands: ui::LocalState::new(None),
            form_submit: false,
            form_cancel: false,
            reauth_submit: false,
            reauth_cancel: false,
            reauth_id: None,
            account_tester: std::sync::Arc::new(crate::account_probe::LiveAccountTester),
            form_probe: None,
            form_candidate: None,
            reauth_probe: None,
            reauth_candidate: None,
            pending_remove: None,
            sign_in: None,
            custodian: account_root.custodian(),
            opener: std::sync::Arc::new(PortalOpener),
            removed: Vec::new(),
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
            account_root,
        }
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
    pub fn submit_form_for_test(&self) {
        self.commands.set(Some(AccountViewCommand::Submit));
    }

    /// Drive the same nonblocking sync polling the live host uses until the
    /// pending account probe and its synchronous disk transaction finish.
    /// Bounded by the probe's production deadline; unlike a fixed number of
    /// scheduler yields this remains reliable when the test suite is busy.
    #[doc(hidden)]
    pub fn finish_form_submission_for_test(
        &mut self,
        state: &mut AutoCommit,
        root: &ObjId,
    ) -> bool {
        let deadline = ui::time::now() + ProbeJob::DEADLINE;
        while self.form_open() && ui::time::now() < deadline {
            self.sync(state, root);
            if self.form_open() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        !self.form_open()
    }

    #[must_use]
    pub fn with_command_state(
        mut self,
        commands: &ui::LocalState<Option<AccountViewCommand>>,
    ) -> Self {
        self.commands = commands.clone();
        self
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
        self.structure_dirty = true;
    }

    fn close_sign_in(&mut self) {
        if let Some(flow) = self.sign_in.as_mut() {
            flow.cancel();
        }
        self.sign_in = None;
        self.structure_dirty = true;
    }

    fn poll_sign_in(&mut self) {
        let Some(flow) = self.sign_in.as_mut() else {
            return;
        };
        let (completed, changed) = flow.poll();
        if changed {
            self.structure_dirty = true;
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
        self.structure_dirty = true;
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
        self.structure_dirty = true;
    }

    fn handle_local_command(&mut self) {
        let command = self.commands.with(Clone::clone);
        self.commands.set(None);
        match command {
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
                self.structure_dirty = true;
            }
            Some(AccountViewCommand::Edit(id)) => self.open_edit(&id),
            Some(AccountViewCommand::Reauthenticate(id)) => self.open_reauthentication(id),
            Some(AccountViewCommand::Submit) if self.sign_in.is_some() => {
                if let Some(flow) = self.sign_in.as_mut() {
                    flow.submit();
                }
                self.structure_dirty = true;
            }
            Some(AccountViewCommand::Cancel) if self.sign_in.is_some() => self.close_sign_in(),
            Some(AccountViewCommand::Submit) => self.form_submit = true,
            Some(AccountViewCommand::Cancel) => self.form_cancel = true,
            Some(AccountViewCommand::SubmitReauthentication) => self.reauth_submit = true,
            Some(AccountViewCommand::CancelReauthentication) => self.reauth_cancel = true,
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
        self.structure_dirty = true;
    }

    fn clear_form_fields(&self) {
        self.form_name.set(String::new());
        self.form_email.set(String::new());
        self.form_user.set(String::new());
        self.form_server.set(String::new());
        self.form_password.set(String::new());
    }

    fn build_composition(&self, theme: &Theme) -> Box<dyn Widget> {
        let mut panel: Vec<El> = Vec::new();

        let mut intro: Vec<El> = Vec::new();
        intro.push(Box::new(
            Text::new(
                "Connect email, calendars and files once, then use them in compatible apps. \
                 Account details and credentials stay on this device; they are never shared \
                 with a workspace or collaborators.",
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
        let mut introduction = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(8.0);
        for line in intro {
            introduction = introduction.push(line);
        }
        panel.push(Box::new(introduction));

        if let Some(flow) = &self.sign_in {
            panel.push(flow.card());
        } else if let Some(kind) = self.form_kind {
            panel.push(self.account_form(kind));
        }
        if let Some(id) = &self.reauth_id {
            panel.push(self.reauth_form(id));
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

        panel.push(self.mail_card(theme));
        panel.push(self.kind_card(AccountKind::CalDav, theme));
        panel.push(self.kind_card(AccountKind::WebDav, theme));
        panel.push(self.kind_card(AccountKind::Nextcloud, theme));
        for kind in [
            AccountKind::GoogleDrive,
            AccountKind::OneDrive,
            AccountKind::Dropbox,
            AccountKind::ICloudDrive,
        ] {
            panel.push(self.kind_card(kind, theme));
        }

        crate::widgets::cards(panel)
    }

    fn mail_card(&self, theme: &Theme) -> El {
        let title = AccountKind::Mail.label();
        let add_btn: El = Box::new(account_action_button(
            "settings.accounts.add.mail",
            "Add account",
            crate::actions::pane_resource_id(crate::categories::Category::ExternalAccounts),
            "External Accounts".into(),
            "add-mail",
            tilo_actions::OperationKind::View,
            crate::actions::AccountOperation::Add {
                kind: "mail".into(),
            },
            false,
        ));
        let views = self.dir.views_of(AccountKind::Mail);
        let body = self.kind_body(AccountKind::Mail, &views, theme);
        card_with_action(title, add_btn, body)
    }

    fn kind_card(&self, kind: AccountKind, theme: &Theme) -> El {
        let views = self.dir.views_of(kind);
        card_with_action(
            kind.label(),
            Box::new(account_action_button(
                &format!("settings.accounts.add.{}", kind.slug()),
                "Add account",
                crate::actions::pane_resource_id(crate::categories::Category::ExternalAccounts),
                "External Accounts".into(),
                &format!("add-{}", kind.slug()),
                tilo_actions::OperationKind::View,
                crate::actions::AccountOperation::Add {
                    kind: kind.slug().into(),
                },
                false,
            )),
            self.kind_body(kind, &views, theme),
        )
    }

    fn kind_body(&self, kind: AccountKind, views: &[AccountView], theme: &Theme) -> El {
        let mut body: Vec<El> = if views.is_empty() {
            vec![caption(&format!("No {} accounts yet.", kind.label()))]
        } else {
            views.iter().map(|v| self.account_row(v, theme)).collect()
        };
        if kind.is_cloud_storage()
            && let Some(reason) = cloud_signin::unavailable_reason(kind, &self.custodian)
        {
            body.push(caption(&reason));
        } else if kind.best_effort() {
            body.push(caption(
                "Best effort: iCloud Drive has no public API, so Tilo uses the web API iCloud.com uses. It may stop working until Tilo is updated.",
            ));
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
    fn account_row(&self, view: &AccountView, theme: &Theme) -> El {
        let id = accounts::AccountId::from(view.id.clone());
        let runtime = self.sync_statuses.get(&id);
        let (status, status_color) = if self.is_sample {
            ("Example", theme.text_dim)
        } else if !view.enabled {
            ("Disabled", theme.text_dim)
        } else {
            match runtime.map(|status| status.health) {
                Some(accounts::AccountSyncHealth::Syncing) => {
                    ("Syncing", theme.tokens.colors.accent[5])
                }
                Some(accounts::AccountSyncHealth::Healthy) => ("Up to date", theme.status_success),
                Some(accounts::AccountSyncHealth::Offline) => {
                    ("Offline", theme.tokens.colors.semantic.warning)
                }
                Some(accounts::AccountSyncHealth::NeedsAuthentication) => {
                    ("Sign in again", theme.tokens.colors.semantic.warning)
                }
                Some(accounts::AccountSyncHealth::Error) => {
                    ("Needs attention", theme.tokens.colors.semantic.danger)
                }
                Some(accounts::AccountSyncHealth::Never) | None => {
                    ("Not checked yet", theme.text_dim)
                }
            }
        };

        // Keep identity and health together; maintenance actions get their own
        // row so narrow layouts remain legible and touch targets never crush
        // the connection summary.
        let mut top = Row::new()
            .push(Box::new(Text::new(&view.label).with_size(13.0)))
            .push(Box::new(Spacer::new()));
        if view.kind.best_effort() {
            top = top.push(status_pill("Best effort", theme.text_dim));
        }
        let top = top.push(status_pill(status, status_color));

        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(top))
            .push(Box::new(
                Text::new(&view.summary)
                    .with_size(11.0)
                    .with_color(theme.text_dim),
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
                    .with_color(theme.text_dim),
            ));
            let account_resource = format!("settings.account:{}", view.id);
            let needs_sign_in = runtime.is_some_and(|status| {
                status.health == accounts::AccountSyncHealth::NeedsAuthentication
            });
            let reauth_label = if view.kind == AccountKind::ICloudDrive && needs_sign_in {
                "Re-authenticate"
            } else {
                "Sign in again"
            };
            let mut primary_actions = Row::new();
            if !view.kind.is_cloud_storage() {
                primary_actions = primary_actions.push(Box::new(
                    account_action_button(
                        &format!("settings.accounts.edit.{}", view.id),
                        "Edit",
                        account_resource.clone(),
                        view.label.clone(),
                        "edit",
                        tilo_actions::OperationKind::View,
                        crate::actions::AccountOperation::Edit {
                            id: view.id.clone(),
                        },
                        false,
                    )
                    .fit_content(ACTION_BUTTON_H),
                ));
            }
            let primary_actions = primary_actions
                .push(Box::new(
                    account_action_button(
                        &format!("settings.accounts.reauth.{}", view.id),
                        reauth_label,
                        account_resource.clone(),
                        view.label.clone(),
                        "reauthenticate",
                        tilo_actions::OperationKind::View,
                        crate::actions::AccountOperation::Reauthenticate {
                            id: view.id.clone(),
                        },
                        false,
                    )
                    .fit_content(ACTION_BUTTON_H),
                ))
                .push(Box::new(Spacer::new()));
            let secondary_actions = Row::new()
                .push(Box::new(
                    account_action_button(
                        &format!("settings.accounts.test.{}", view.id),
                        if view.kind == AccountKind::Mail {
                            "Check server"
                        } else {
                            "Verify sign-in"
                        },
                        account_resource.clone(),
                        view.label.clone(),
                        "test",
                        tilo_actions::OperationKind::Effect,
                        crate::actions::AccountOperation::Test {
                            id: view.id.clone(),
                        },
                        false,
                    )
                    .fit_content(ACTION_BUTTON_H),
                ))
                .push(Box::new(Spacer::new()))
                .push(Box::new(
                    account_action_button(
                        &format!("settings.accounts.remove.{}", view.id),
                        "Remove",
                        account_resource,
                        view.label.clone(),
                        "remove",
                        tilo_actions::OperationKind::Effect,
                        crate::actions::AccountOperation::Remove {
                            id: view.id.clone(),
                        },
                        true,
                    )
                    .danger()
                    .fit_content(ACTION_BUTTON_H),
                ));
            column = column
                .push(Box::new(primary_actions))
                .push(Box::new(secondary_actions));
        }
        Box::new(column)
    }

    /// The inline add-email form: fields bound to CRDT keys plus Save/Cancel.
    #[allow(
        clippy::too_many_lines,
        reason = "the form declares every credential field and accessibility label in one reviewable surface"
    )]
    fn account_form(&self, kind: AccountKind) -> El {
        let name = TextInput::from_state(&self.form_name)
            .with_kind(TextInputKind::Name)
            .with_id("accounts.form.name")
            .with_accessible_name("Display name")
            .with_placeholder("Display name")
            .with_font_size(13.0);
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
        .with_accessible_name(if kind == AccountKind::Mail {
            "Email address"
        } else {
            "Username"
        })
        .with_placeholder(if kind == AccountKind::Mail {
            "you@example.com"
        } else {
            "Username or email"
        })
        .with_font_size(13.0);
        let user = TextInput::from_state(&self.form_user)
            .with_kind(TextInputKind::Username)
            .with_id("accounts.form.username")
            .with_accessible_name("Login name")
            .with_placeholder("Username (defaults to your address)")
            .with_font_size(13.0);
        let password = TextInput::from_state(&self.form_password)
            .with_kind(TextInputKind::Password)
            .with_id("accounts.form.password")
            .with_accessible_name("Password or app password")
            .with_secret(true)
            .with_placeholder("Password")
            .with_font_size(13.0);
        let server = TextInput::from_state(&self.form_server)
            .with_kind(TextInputKind::Url)
            .with_id("accounts.form.server")
            .with_accessible_name("Server URL")
            .with_placeholder(match kind {
                AccountKind::CalDav => "https://calendar.example.com/.well-known/caldav",
                AccountKind::WebDav => "https://files.example.com/remote.php/dav/files/you/",
                AccountKind::Nextcloud => "https://cloud.example.com",
                AccountKind::Mail => "Detected from your email address",
                AccountKind::GoogleDrive
                | AccountKind::OneDrive
                | AccountKind::Dropbox
                | AccountKind::ICloudDrive => "",
            })
            .with_font_size(13.0);
        let buttons = Row::new()
            .push(Box::new(account_action_button(
                "settings.accounts.form.save",
                "Save account",
                crate::actions::pane_resource_id(crate::categories::Category::ExternalAccounts),
                "External Accounts".into(),
                "save-form",
                tilo_actions::OperationKind::Effect,
                crate::actions::AccountOperation::SubmitForm,
                false,
            )))
            .push(Box::new(account_action_button(
                "settings.accounts.form.cancel",
                "Cancel",
                crate::actions::pane_resource_id(crate::categories::Category::ExternalAccounts),
                "External Accounts".into(),
                "cancel-form",
                tilo_actions::OperationKind::View,
                crate::actions::AccountOperation::CancelForm,
                false,
            )))
            .push(Box::new(Spacer::new()));
        card(
            &format!(
                "{} {} account",
                if self.form_account_id.is_some() {
                    "Edit"
                } else {
                    "Add"
                },
                kind.label()
            ),
            rows({
                let mut fields = vec![
                    setting_row("Display name", Box::new(name)),
                    setting_row(
                        if kind == AccountKind::Mail {
                            "Email address"
                        } else {
                            "Username"
                        },
                        Box::new(identity),
                    ),
                ];
                if kind == AccountKind::Mail {
                    fields.push(setting_row("Login name", Box::new(user)));
                } else {
                    fields.push(setting_row("Server", Box::new(server)));
                }
                if kind != AccountKind::Nextcloud {
                    fields.push(setting_row("Password or app password", Box::new(password)));
                }
                if let Some(error) = &self.form_error {
                    fields.push(caption(&format!("Could not save: {error}")));
                }
                fields.push(caption("Required fields, the secure endpoint, and the credential are checked with a bounded protocol sign-in before anything is saved. Credentials and drafts stay device-local; they never enter collaboration state or account metadata."));
                fields.push(Box::new(buttons));
                fields
            }),
        )
    }

    fn reauth_form(&self, id: &str) -> El {
        let password = TextInput::from_state(&self.reauth_password)
            .with_kind(TextInputKind::Password)
            .with_id("accounts.reauth.password")
            .with_accessible_name("New password or app password")
            .with_secret(true)
            .with_placeholder("New password or app password")
            .with_font_size(13.0);
        let mut fields = vec![
            value_row("Account", id),
            setting_row("Credential", Box::new(password)),
            caption(
                "This replaces an existing password or app password in the OS account vault. Accounts that sign in through the browser or an Apple ID offer a new sign-in instead.",
            ),
        ];
        if let Some(error) = &self.reauth_error {
            fields.push(caption(&format!("Could not update credential: {error}")));
        }
        fields.push(Box::new(
            Row::new()
                .push(Box::new(account_action_button(
                    "settings.accounts.reauth.save",
                    "Update credential",
                    crate::actions::pane_resource_id(crate::categories::Category::ExternalAccounts),
                    "External Accounts".into(),
                    "save-reauthentication",
                    tilo_actions::OperationKind::Effect,
                    crate::actions::AccountOperation::SubmitReauthentication,
                    false,
                )))
                .push(Box::new(account_action_button(
                    "settings.accounts.reauth.cancel",
                    "Cancel",
                    crate::actions::pane_resource_id(crate::categories::Category::ExternalAccounts),
                    "External Accounts".into(),
                    "cancel-reauthentication",
                    tilo_actions::OperationKind::View,
                    crate::actions::AccountOperation::CancelReauthentication,
                    false,
                )))
                .push(Box::new(Spacer::new())),
        ));
        card("Replace saved password", rows(fields))
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
        let mut secret = account_root.vault().get(&id).unwrap_or_default();
        if password.trim().is_empty() {
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

    /// Open the add-email form (and seed its bound `Text` objects).
    fn handle_add(&mut self, state: &mut AutoCommit, root: &ObjId) {
        for kind in AccountKind::ALL {
            let key = add_key(kind);
            if read_bool(state, root, key).unwrap_or(false) {
                let _ = state.put(root, key, automerge::ScalarValue::Boolean(false));
                if kind.signs_in() {
                    if self.sign_in.is_none() {
                        self.open_sign_in(kind, None);
                    }
                } else if self.form_kind.is_none() && self.sign_in.is_none() {
                    self.form_kind = Some(kind);
                    self.form_error = None;
                    self.form_account_id = None;
                    self.form_password.set(String::new());
                    self.clear_form_fields();
                    self.structure_dirty = true;
                }
            }
        }
    }

    fn handle_edit_triggers(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.is_sample {
            return;
        }
        for kind in AccountKind::ALL {
            for view in self.dir.views_of(kind) {
                let key = edit_key(&view.id);
                if !read_bool(state, root, &key).unwrap_or(false) {
                    continue;
                }
                let _ = state.put(root, key, false);
                self.open_edit(&view.id);
                return;
            }
        }
    }

    fn handle_form_cancel(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.form_cancel || read_bool(state, root, FORM_CANCEL_KEY).unwrap_or(false) {
            self.form_cancel = false;
            let _ = state.put(
                root,
                FORM_CANCEL_KEY,
                automerge::ScalarValue::Boolean(false),
            );
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
                self.structure_dirty = true;
            }
        }
    }

    /// Save starts a bounded protocol probe against a device-local draft. No
    /// metadata or credential is persisted until [`Self::poll_form_probe`]
    /// receives success.
    fn handle_form_save(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.form_submit || read_bool(state, root, FORM_SAVE_KEY).unwrap_or(false) {
            self.form_submit = false;
            let _ = state.put(root, FORM_SAVE_KEY, automerge::ScalarValue::Boolean(false));
            if let Some(flow) = self.sign_in.as_mut() {
                flow.submit();
                self.structure_dirty = true;
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
                    self.form_error = Some("Checking the account before saving…".into());
                    self.structure_dirty = true;
                }
                Err(error) => {
                    self.form_error = Some(error);
                    self.structure_dirty = true;
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
                self.structure_dirty = true;
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
        self.structure_dirty = true;
    }

    /// Per-account reauthentication triggers for accounts with a stored
    /// password (sample rows never offer reauth).
    fn handle_reauth_triggers(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.is_sample {
            return;
        }
        let views: Vec<_> = AccountKind::ALL
            .into_iter()
            .flat_map(|kind| self.dir.views_of(kind))
            .collect();
        for view in views {
            let has_password = self
                .vault
                .get(&accounts::AccountId::from(view.id.clone()))
                .is_some_and(|secret| !secret.password.is_empty());
            if !has_password && !view.kind.signs_in() {
                continue;
            }
            let key = reauth_key(&view.id);
            if read_bool(state, root, &key).unwrap_or(false) {
                let _ = state.put(root, key, automerge::ScalarValue::Boolean(false));
                self.open_reauthentication(view.id);
            }
        }
    }

    fn handle_reauth_cancel(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.reauth_cancel || read_bool(state, root, REAUTH_CANCEL_KEY).unwrap_or(false) {
            self.reauth_cancel = false;
            let _ = state.put(
                root,
                REAUTH_CANCEL_KEY,
                automerge::ScalarValue::Boolean(false),
            );
            self.reauth_id = None;
            self.reauth_error = None;
            self.reauth_probe = None;
            self.reauth_candidate = None;
            self.reauth_password.set(String::new());
            self.structure_dirty = true;
        }
    }

    fn handle_reauth_save(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.reauth_submit || read_bool(state, root, REAUTH_SAVE_KEY).unwrap_or(false) {
            self.reauth_submit = false;
            let _ = state.put(
                root,
                REAUTH_SAVE_KEY,
                automerge::ScalarValue::Boolean(false),
            );
            if self.reauth_probe.is_some() {
                return;
            }
            if let Some(id) = self.reauth_id.clone() {
                let password = self.reauth_password.with(Clone::clone);
                if password.trim().is_empty() {
                    self.reauth_error = Some("enter a password or app password".into());
                    self.structure_dirty = true;
                    return;
                }
                let account_id = accounts::AccountId::from(id);
                let Some(account) = self.account_root.store().get(&account_id).cloned() else {
                    self.reauth_error = Some("account no longer exists".into());
                    self.structure_dirty = true;
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
                self.reauth_error = Some("Checking the replacement credential…".into());
                self.structure_dirty = true;
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
                self.structure_dirty = true;
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
        self.structure_dirty = true;
    }

    fn handle_remove_cancel(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if read_bool(state, root, REMOVE_CANCEL_KEY).unwrap_or(false) {
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
        if read_bool(state, root, REMOVE_CONFIRM_KEY).unwrap_or(false) {
            let _ = state.put(
                root,
                REMOVE_CONFIRM_KEY,
                automerge::ScalarValue::Boolean(false),
            );
            if let Some((kind, id)) = self.pending_remove.take() {
                match Self::remove_account(&self.account_root, &self.custodian, kind, &id) {
                    Ok(()) => {
                        self.removed.push(id);
                        self.reload();
                        self.structure_dirty = true;
                    }
                    Err(error) => {
                        self.form_error = Some(error);
                        self.structure_dirty = true;
                    }
                }
            }
        }
    }

    /// Per-account Remove buttons. Snapshot (kind, id, key) so we don't hold
    /// a borrow across the mutating store calls.
    fn handle_remove_triggers(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.is_sample {
            return;
        }
        let targets: Vec<(AccountKind, String, String)> = AccountKind::ALL
            .into_iter()
            .flat_map(|kind| {
                self.dir
                    .views_of(kind)
                    .into_iter()
                    .map(move |v| (kind, v.id.clone(), remove_key(&v.id)))
            })
            .collect();
        for (kind, id, key) in targets {
            if !read_bool(state, root, &key).unwrap_or(false) || self.removed.contains(&id) {
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
    let now = ui::time::wall()
        .duration_since(ui::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64);
    let age = now.saturating_sub(unix).max(0);
    match age {
        0..=59 => "just now".into(),
        60..=3_599 => format!("{} min ago", age / 60),
        3_600..=86_399 => format!("{} h ago", age / 3_600),
        _ => format!("{} d ago", age / 86_400),
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

pub fn build() -> Box<dyn Widget> {
    Box::new(AccountsPane::new())
}

/// Build the pane from a caller-supplied directory (tests / deterministic
/// `--shots`). Treated as real accounts so Remove buttons render.
pub fn build_with(dir: &AccountDirectory) -> Box<dyn Widget> {
    Box::new(AccountsPane::from_dir(dir.clone(), false))
}

impl Widget for AccountsPane {
    fn sync(&mut self, state: &mut AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document(state, root);
        self.handle_local_command();
        self.handle_add(state, root);
        self.handle_edit_triggers(state, root);
        self.handle_form_cancel(state, root);
        self.handle_form_save(state, root);
        self.poll_form_probe();
        self.poll_sign_in();
        self.handle_reauth_triggers(state, root);
        self.handle_reauth_cancel(state, root);
        self.handle_reauth_save(state, root);
        self.poll_reauth_probe();
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
        let mut composed = self.build_composition(theme);
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
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

    fn needs_idle_sync(&self) -> bool {
        self.form_probe.is_some()
            || self.reauth_probe.is_some()
            || self
                .sign_in
                .as_ref()
                .is_some_and(cloud_signin::SignInFlow::busy)
            || self
                .composed
                .as_ref()
                .is_some_and(|child| child.needs_idle_sync())
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

fn add_key(kind: AccountKind) -> &'static str {
    match kind {
        AccountKind::Mail => ADD_MAIL_KEY,
        AccountKind::CalDav => ADD_CALDAV_KEY,
        AccountKind::WebDav => ADD_WEBDAV_KEY,
        AccountKind::Nextcloud => ADD_NEXTCLOUD_KEY,
        AccountKind::GoogleDrive => ADD_GOOGLE_DRIVE_KEY,
        AccountKind::OneDrive => ADD_ONEDRIVE_KEY,
        AccountKind::Dropbox => ADD_DROPBOX_KEY,
        AccountKind::ICloudDrive => ADD_ICLOUD_DRIVE_KEY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::sample_directory;
    use automerge::ReadDoc;
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};
    use ui::Theme;

    struct RejectCandidate;

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
    fn settle_probes(pane: &mut AccountsPane, doc: &mut AutoCommit) {
        let deadline = ui::time::now() + ProbeJob::DEADLINE;
        loop {
            pane.sync(doc, &automerge::ROOT);
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

    fn lay_out(pane: &mut dyn Widget) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }

    /// Sample directories are read-only, real ones get labelled destructive remove buttons, and empty stores stay empty.
    #[test]
    fn accounts_pane_renders_only_real_accounts_as_removable() {
        // Case `sample_directory_renders_read_only_without_remove_buttons`.
        {
            // The sample is illustration: one live Add button per provider kind,
            // and no Remove buttons.
            let mut pane = AccountsPane::from_dir(sample_directory(), true);
            lay_out(&mut pane);
            assert_eq!(
                count_named(&pane, "Button"),
                AccountKind::ALL.len(),
                "every provider kind can be added over the sample"
            );
        }

        // Case `real_directory_renders_remove_buttons`.
        {
            // Without stored passwords, real rows offer removal but do not invent
            // a reauthentication action.
            let dir = sample_directory(); // 2 mail + 1 cal + 1 nextcloud = 4 accounts
            let mut pane = AccountsPane::from_dir(dir, false);
            lay_out(&mut pane);
            // One Add per kind + four maintenance actions on each of four accounts.
            assert_eq!(count_named(&pane, "Button"), AccountKind::ALL.len() + 16);
        }

        // Case `account_removal_is_labeled_and_visibly_destructive`.
        {
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

    /// The inline add form: local drafts, labelled fields, empty-email no-op and rejected adds.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn account_form_drafts_are_local_accessible_and_validated() {
        // Case `add_button_opens_the_inline_form`.
        {
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

        // Case `form_drafts_are_local_and_every_field_has_explicit_accessibility`.
        {
            let commands = ui::LocalState::new(None);
            let mut pane =
                AccountsPane::from_dir(sample_directory(), true).with_command_state(&commands);
            let mut doc = AutoCommit::new();
            commands.set(Some(AccountViewCommand::Add(AccountKind::WebDav)));
            pane.sync(&mut doc, &automerge::ROOT);
            pane.set_form_fields_for_test("Home files", "", "ada", "https://files.example.test");
            pane.set_form_password_for_test("never-collaborate-this");
            pane.sync(&mut doc, &automerge::ROOT);
            for key in [
                "accounts.form.name",
                "accounts.form.email",
                "accounts.form.username",
                "accounts.form.server",
                "accounts.form.password",
            ] {
                assert!(
                    doc.get(&automerge::ROOT, key)
                        .expect("read draft key")
                        .is_none(),
                    "device-local form field {key} must never enter Automerge"
                );
            }

            lay_out(&mut pane);
            let mut nodes = Vec::new();
            access_nodes(&pane, &mut nodes);
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
            let commands = ui::LocalState::new(None);
            let mut pane = AccountsPane::in_root(root.clone())
                .with_command_state(&commands)
                .with_account_tester(std::sync::Arc::new(RejectCandidate));
            let mut doc = AutoCommit::new();
            commands.set(Some(AccountViewCommand::Add(AccountKind::Mail)));
            pane.sync(&mut doc, &automerge::ROOT);
            pane.set_form_fields_for_test(
                "Rejected",
                "rejected@example.test",
                "rejected@example.test",
                "",
            );
            pane.set_form_password_for_test("candidate-password");
            commands.set(Some(AccountViewCommand::Submit));
            pane.sync(&mut doc, &automerge::ROOT);
            settle_probes(&mut pane, &mut doc);
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

            let commands = ui::LocalState::new(None);
            let mut pane = AccountsPane::in_root(root.clone())
                .with_command_state(&commands)
                .with_account_tester(std::sync::Arc::new(RejectCandidate));
            let mut doc = AutoCommit::new();
            commands.set(Some(AccountViewCommand::Reauthenticate(id.to_string())));
            pane.sync(&mut doc, &automerge::ROOT);
            pane.set_reauth_password_for_test("candidate-password");
            commands.set(Some(AccountViewCommand::SubmitReauthentication));
            pane.sync(&mut doc, &automerge::ROOT);
            settle_probes(&mut pane, &mut doc);
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
}
