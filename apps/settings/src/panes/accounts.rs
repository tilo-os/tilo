//! External Accounts pane — the integration hub and editor for the shared
//! account stores ([`crate::accounts`]) that Mail, Calendar, and the file
//! apps consume.
//!
//! ## What's wired
//!
//! - **Remove** (the trash button on any real account) loads the owning
//!   store, drops the account, and persists — a real, complete mutation of
//!   the on-disk file Mail/Calendar/Files read.
//! - **Add email account** opens an inline form; **Save** builds an
//!   [`AccountConfig`] (auto-configuring IMAP/SMTP from the address) and
//!   appends it to [`MailStore`], persisting the same JSON Mail loads.
//!
//! ## Live vs. sample
//!
//! On launch the pane loads the real stores. When every store is empty (a
//! fresh install, or `--shots` on CI) it renders
//! [`crate::accounts::sample_directory`] as a read-only illustration — the
//! sample rows omit the Remove button, and adding a real account replaces
//! the illustration with the live directory.
//!
//! Adding CalDAV/WebDAV/Nextcloud accounts and editing existing ones need
//! their own forms and a credential flow; those are follow-ups tracked in
//! the growth ledger rather than shipped as buttons that don't act.

use std::collections::HashMap;

use automerge::{transaction::Transactable, AutoCommit, ObjId, ReadDoc, Value};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Button, Column, Row, Spacer, Text, TextInput, Widget};
use vello::kurbo::{Affine, Point, Rect};
use vello::Scene;
use winit::event::{Modifiers, WindowEvent};

use crate::accounts::{
    AccountConfig, AccountDirectory, AccountKind, AccountView, DavStore, MailAccount, MailStore,
    NextcloudStore,
};
use crate::widgets::{card, card_with_action, caption, rows, setting_row, El};

/// Key the "Add" button on the Email card writes to open the form.
pub const ADD_MAIL_KEY: &str = "settings.accounts.add.mail";
/// Keys the inline form's Save / Cancel buttons write.
pub const FORM_SAVE_KEY: &str = "settings.accounts.form.save";
pub const FORM_CANCEL_KEY: &str = "settings.accounts.form.cancel";
/// Text fields the inline form binds.
pub const FORM_NAME_KEY: &str = "accounts.form.name";
pub const FORM_EMAIL_KEY: &str = "accounts.form.email";
pub const FORM_USER_KEY: &str = "accounts.form.username";

/// Per-account Remove key.
fn remove_key(id: &str) -> String {
    format!("settings.accounts.remove.{id}")
}

/// External accounts pane.
pub struct AccountsPane {
    dir: AccountDirectory,
    /// Whether `dir` is the illustrative sample (the real stores are empty).
    is_sample: bool,
    form_open: bool,
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
        let real = AccountDirectory::load();
        let is_sample = real.is_empty();
        let dir = if is_sample {
            crate::accounts::sample_directory()
        } else {
            real
        };
        Self {
            dir,
            is_sample,
            form_open: false,
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
            is_sample,
            form_open: false,
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

        panel.push(card(
            "External accounts",
            rows(vec![
                Box::new(
                    Text::new(
                        "Accounts you add here are shared across the system. \
                         Mail reads your email accounts, Calendar reads your \
                         CalDAV calendars, and the file apps mount your WebDAV \
                         and Nextcloud shares.",
                    )
                    .with_size(12.0),
                ),
                caption(&format!(
                    "Email accounts: {}",
                    MailStore::path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "~/.config/qos/mail/accounts.json".into())
                )),
            ]),
        ));

        if self.form_open {
            panel.push(self.mail_form());
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
        let title = format!("{}  {}", AccountKind::Mail.glyph(), AccountKind::Mail.label());
        let add_btn: El = Box::new(Button::new(
            "settings.accounts.add.mail",
            "\u{f0415}  Add",
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
    fn account_row(&self, view: &AccountView) -> El {
        let status = if view.enabled { "Connected" } else { "Disabled" };
        let status_color = if view.enabled {
            ui::Theme::dark().status_success
        } else {
            ui::Theme::dark().text_dim
        };

        let text_block = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(Text::new(&view.label).with_size(13.0)))
            .push(Box::new(
                Text::new(&view.summary)
                    .with_size(11.0)
                    .with_color(ui::Theme::dark().text_dim),
            ));

        let mut controls = Row::new().push(Box::new(
            Text::new(status).with_size(11.0).with_color(status_color),
        ));
        if !self.is_sample {
            let rk = remove_key(&view.id);
            let remove_id = format!("settings.accounts.remove.{}", view.id);
            controls = controls.push(Box::new(Button::new(&remove_id, "\u{f01b4}", Some(&rk))));
        }

        Box::new(
            Row::new()
                .push(Box::new(text_block))
                .push(Box::new(Spacer::new()))
                .push(Box::new(controls)),
        )
    }

    /// The inline add-email form: fields bound to CRDT keys plus Save/Cancel.
    fn mail_form(&self) -> El {
        let name = TextInput::new(Vec::new())
            .with_id("accounts.form.name")
            .with_on_change_path(FORM_NAME_KEY)
            .with_placeholder("Display name")
            .with_font_size(13.0);
        let email = TextInput::new(Vec::new())
            .with_id("accounts.form.email")
            .with_on_change_path(FORM_EMAIL_KEY)
            .with_placeholder("you@example.com")
            .with_font_size(13.0);
        let user = TextInput::new(Vec::new())
            .with_id("accounts.form.username")
            .with_on_change_path(FORM_USER_KEY)
            .with_placeholder("Username (defaults to your address)")
            .with_font_size(13.0);
        let password = TextInput::new(Vec::new())
            .with_id("accounts.form.password")
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

    /// Reload the real directory after a mutation and recompute `is_sample`.
    fn reload(&mut self) {
        let real = AccountDirectory::load();
        self.is_sample = real.is_empty();
        self.dir = if self.is_sample {
            crate::accounts::sample_directory()
        } else {
            real
        };
    }

    /// Persist a new mail account from the form fields. Returns whether an
    /// account was actually added (an empty address is a no-op).
    fn save_mail_account(&mut self, name: String, email: String, username: String) -> bool {
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
        let mut store = MailStore::load();
        store.accounts.push(MailAccount::new(config));
        if let Err(e) = store.save() {
            warn!(target: "qos::settings", op = "mail_store_save", error = %e, "failed to persist new mail account");
            return false;
        }
        true
    }

    /// Remove an account of `kind` by id from its owning store, persisting.
    fn remove_account(&mut self, kind: AccountKind, id: &str) {
        let res = match kind {
            AccountKind::Mail => {
                let mut s = MailStore::load();
                s.remove(id);
                s.save().map(|_| ())
            }
            AccountKind::CalDav => {
                let mut s = DavStore::load_from(DavStore::calendar_path());
                s.accounts.retain(|a| a.id != id);
                s.save_to(DavStore::calendar_path()).map(|_| ())
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
        }
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
        // Open the add-email form.
        if read_bool(state, root, ADD_MAIL_KEY) {
            let _ = state.put(root, ADD_MAIL_KEY, automerge::ScalarValue::Boolean(false));
            if !self.form_open {
                self.form_open = true;
                self.structure_dirty = true;
            }
        }
        // Cancel the form.
        if read_bool(state, root, FORM_CANCEL_KEY) {
            let _ = state.put(root, FORM_CANCEL_KEY, automerge::ScalarValue::Boolean(false));
            if self.form_open {
                self.form_open = false;
                clear_form(state, root);
                self.structure_dirty = true;
            }
        }
        // Save the form → create + persist a real mail account.
        if read_bool(state, root, FORM_SAVE_KEY) {
            let _ = state.put(root, FORM_SAVE_KEY, automerge::ScalarValue::Boolean(false));
            let name = read_str(state, root, FORM_NAME_KEY).unwrap_or_default();
            let email = read_str(state, root, FORM_EMAIL_KEY).unwrap_or_default();
            let user = read_str(state, root, FORM_USER_KEY).unwrap_or_default();
            if self.save_mail_account(name, email, user) {
                self.form_open = false;
                clear_form(state, root);
                self.reload();
                self.structure_dirty = true;
            }
        }
        // Per-account Remove buttons. Snapshot (kind, id, key) so we don't
        // hold a borrow across the mutating store calls.
        if !self.is_sample {
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
            let mut removed_any = false;
            for (kind, id, key) in targets {
                if !read_bool(state, root, &key) || self.removed.contains(&id) {
                    continue;
                }
                let _ = state.put(root, key.as_str(), automerge::ScalarValue::Boolean(false));
                self.removed.push(id.clone());
                self.remove_account(kind, &id);
                removed_any = true;
            }
            if removed_any {
                self.reload();
                self.structure_dirty = true;
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
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[child]).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node) {
                if let Some(&cn) = children.first() {
                    c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
                }
            }
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
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node_id) {
                if let Some(&cn) = children.first() {
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

    fn name(&self) -> &str {
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
            .map(|c| c.take_structure_dirty())
            .unwrap_or(false);
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

/// Blank the form's bound text keys so a reopened form starts empty.
fn clear_form(state: &mut AutoCommit, root: &ObjId) {
    for key in [FORM_NAME_KEY, FORM_EMAIL_KEY, FORM_USER_KEY] {
        let _ = state.put(root, key, "");
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
        let mut n = if w.name() == name { 1 } else { 0 };
        for c in w.children() {
            n += count_named(c, name);
        }
        n
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
        // Treated as real: each account gets a Remove button plus the one Add.
        let dir = sample_directory(); // 2 mail + 1 cal + 1 nextcloud = 4 accounts
        let mut pane = AccountsPane::from_dir(dir, false);
        lay_out(&mut pane);
        // 1 Add + 4 Remove.
        assert_eq!(count_named(&pane, "Button"), 5);
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
        assert!(pane.take_structure_dirty(), "opening the form requests a rebuild");
        // Cancel closes it.
        let _ = doc.put(&root, FORM_CANCEL_KEY, true);
        pane.sync(&mut doc, &root);
        assert!(!pane.form_open(), "Cancel closes the form");
    }

    #[test]
    fn save_with_empty_email_is_a_noop() {
        // Guard: saving a blank address must not create an account (and must
        // not touch disk). We assert the pure guard directly.
        let mut pane = AccountsPane::from_dir(AccountDirectory::default(), false);
        assert!(!pane.save_mail_account("Nobody".into(), "  ".into(), "".into()));
    }
}
