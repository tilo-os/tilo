//! The vault as the window shows it: the item cache, the place chosen in
//! the sidebar, the open item, the editor draft, short-lived disclosures and
//! the import, export and backup flows.
//!
//! Every part of the window (the sidebar, the item list, the content pages
//! and the dialogs) reads this state and asks for changes through
//! [`Request`]s, which the root applies. The palette, agents and scripts
//! reach the same state through [`PasswordsEffect`]s.

use std::cell::RefCell;
use std::rc::Rc;

use accounts::{CredentialGrant, GeneratedPasswordPolicy, SecretId, SecretItem, SecretItemKind};
use ui::lock::MutexRecover as _;
use ui::widget::NavPane;
use vello::kurbo::Rect;
use zeroize::Zeroizing;

use crate::actions::PasswordsEffect;
use crate::transfer::ImportLogin;

/// The core every part of the window reads and changes.
pub(crate) type Shared = Rc<RefCell<Core>>;

/// Requests from controls, applied by the root.
pub(crate) type Requests = Rc<RefCell<Vec<Request>>>;

/// How long a revealed secret stays on screen.
const REVEAL_FOR: std::time::Duration = std::time::Duration::from_secs(10);

/// How long a copied secret stays on the clipboard.
const CLIPBOARD_FOR: std::time::Duration = std::time::Duration::from_secs(30);

/// Which items the list shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum VaultFilter {
    #[default]
    All,
    Favorites,
    Kind(SecretItemKind),
}

impl VaultFilter {
    pub(crate) const ALL: [Self; 7] = [
        Self::All,
        Self::Favorites,
        Self::Kind(SecretItemKind::Login),
        Self::Kind(SecretItemKind::ApiKey),
        Self::Kind(SecretItemKind::SecureNote),
        Self::Kind(SecretItemKind::Wifi),
        Self::Kind(SecretItemKind::Other),
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Favorites => "favorites",
            Self::Kind(SecretItemKind::Login) => "login",
            Self::Kind(SecretItemKind::ApiKey) => "api-key",
            Self::Kind(SecretItemKind::SecureNote) => "note",
            Self::Kind(SecretItemKind::Wifi) => "wifi",
            Self::Kind(SecretItemKind::Other) => "other",
        }
    }

    pub(crate) fn parse(value: &str) -> Self {
        match value {
            "favorites" => Self::Favorites,
            "login" => Self::Kind(SecretItemKind::Login),
            "api-key" => Self::Kind(SecretItemKind::ApiKey),
            "note" => Self::Kind(SecretItemKind::SecureNote),
            "wifi" => Self::Kind(SecretItemKind::Wifi),
            "other" => Self::Kind(SecretItemKind::Other),
            _ => Self::All,
        }
    }

    /// The list's title and the sidebar row's label.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::All => "All items",
            Self::Favorites => "Favorites",
            Self::Kind(SecretItemKind::Login) => "Logins",
            Self::Kind(SecretItemKind::ApiKey) => "API keys",
            Self::Kind(SecretItemKind::SecureNote) => "Secure notes",
            Self::Kind(SecretItemKind::Wifi) => "Wi-Fi",
            Self::Kind(SecretItemKind::Other) => "Other secrets",
        }
    }

    pub(crate) fn matches(self, item: &SecretItem) -> bool {
        match self {
            Self::All => true,
            Self::Favorites => is_favorite(item),
            Self::Kind(kind) => item.kind == kind,
        }
    }
}

/// What the content column shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Screen {
    #[default]
    Vault,
    /// The list of items and the editor of a new or open item.
    Editor,
    /// Reused, weak and old passwords.
    Review,
    /// Apps that may use vault items without asking.
    AppAccess,
    /// Backups, browser import and export.
    Transfer,
}

impl Screen {
    /// Whether the item list is beside the content.
    pub(crate) fn shows_list(self) -> bool {
        matches!(self, Self::Vault | Self::Editor)
    }
}

/// An editor field, for validation messages and the draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DraftField {
    Name,
    Username,
    Origin,
    Password,
    Notes,
    Tags,
    CustomFields,
}

pub(crate) enum Dialog {
    ImportPreview {
        rows: Vec<ImportLogin>,
        /// Per row: already in the vault, so the import skips it.
        skipped: Vec<bool>,
    },
    ExportPlaintext {
        acknowledged: bool,
        /// The person chose a file before acknowledging.
        reminded: bool,
    },
    Backup {
        error: Option<&'static str>,
    },
    Restore {
        bytes: Zeroizing<Vec<u8>>,
        error: Option<String>,
    },
    DeleteConfirm {
        id: SecretId,
        title: String,
    },
    AccessConfirm {
        proposal: String,
        lifetime: String,
    },
}

impl Dialog {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::ImportPreview { .. } => "import",
            Self::ExportPlaintext { .. } => "export",
            Self::Backup { .. } => "backup",
            Self::Restore { .. } => "restore",
            Self::DeleteConfirm { .. } => "delete",
            Self::AccessConfirm { .. } => "access",
        }
    }
}

/// What a control asks the app to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    /// A sidebar destination, by its row id.
    Navigate(String),
    Open(SecretId),
    ToggleFavorite(SecretId),
    NewItem,
    Edit,
    CancelEdit,
    Save,
    Generate,
    SetKind(SecretItemKind),
    CopyIdentity,
    CopySecret,
    RevealSecret,
    CopyOrigins,
    Delete,
    ConfirmDelete,
    CloseDialog,
    Import,
    ConfirmImport,
    Export,
    Acknowledge(bool),
    ConfirmExport,
    Backup,
    ConfirmBackup,
    Restore,
    ConfirmRestore,
    Revoke(String),
    ReviewAccess {
        proposal: String,
        lifetime: String,
    },
    ConfirmAccess,
    DenyAccess(String),
}

/// A finding of the password review for one item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Finding {
    pub(crate) id: SecretId,
    pub(crate) reused: bool,
    pub(crate) weak: bool,
    pub(crate) old: bool,
}

impl Finding {
    /// "Reused and weak": what is wrong, in one phrase.
    pub(crate) fn reasons(&self) -> String {
        let reasons: Vec<&str> = [
            (self.reused, "reused"),
            (self.weak, "weak"),
            (self.old, "not changed in a year"),
        ]
        .into_iter()
        .filter_map(|(on, reason)| on.then_some(reason))
        .collect();
        let joined = match reasons.as_slice() {
            [] => String::new(),
            [one] => (*one).to_owned(),
            [first, second] => format!("{first} and {second}"),
            [first, second, third, ..] => format!("{first}, {second} and {third}"),
        };
        ui::format::capitalize_first(&joined)
    }
}

pub(crate) fn is_favorite(item: &SecretItem) -> bool {
    item.tags.contains("favorite")
}

/// Seconds since the epoch on the app's clock (pinned in captures).
pub(crate) fn now_unix() -> u64 {
    u64::try_from(ui::clock::local_now().timestamp()).unwrap_or(0)
}

/// What a kind is called in the detail view and the editor.
pub(crate) fn kind_label(kind: SecretItemKind) -> &'static str {
    match kind {
        SecretItemKind::Login => "Login",
        SecretItemKind::ApiKey => "API key",
        SecretItemKind::SecureNote => "Secure note",
        SecretItemKind::Wifi => "Wi-Fi network",
        SecretItemKind::Other => "Secret",
    }
}

/// What the secret of a kind is called.
pub(crate) fn secret_label(kind: SecretItemKind) -> &'static str {
    match kind {
        SecretItemKind::ApiKey => "API key",
        SecretItemKind::Wifi | SecretItemKind::Login => "Password",
        SecretItemKind::Other => "Secret",
        SecretItemKind::SecureNote => "Note",
    }
}

/// What the identity of a kind (the username row) is called.
pub(crate) fn identity_label(kind: SecretItemKind) -> &'static str {
    match kind {
        SecretItemKind::ApiKey => "Account",
        SecretItemKind::Wifi => "Network",
        SecretItemKind::Login | SecretItemKind::Other | SecretItemKind::SecureNote => "Username",
    }
}

/// What the origins of a kind are called.
pub(crate) fn origin_label(kind: SecretItemKind, count: usize) -> &'static str {
    match (kind, count) {
        (SecretItemKind::ApiKey, _) => "Service",
        (_, 0 | 1) => "Website",
        _ => "Websites",
    }
}

/// The identity a row shows: the network for Wi-Fi, else the username.
pub(crate) fn identity(item: &SecretItem) -> &str {
    match item.kind {
        SecretItemKind::Wifi => item
            .fields
            .get("network")
            .map_or(item.username.as_str(), String::as_str),
        _ => item.username.as_str(),
    }
}

/// The one secondary fact of a list row.
pub(crate) fn summary(item: &SecretItem) -> String {
    let identity = identity(item);
    if !identity.is_empty() {
        return identity.to_owned();
    }
    if let Some(origin) = item.origins.iter().next() {
        return host_of(origin).to_owned();
    }
    kind_label(item.kind).to_owned()
}

/// `bank.example` for `https://bank.example`.
pub(crate) fn host_of(origin: &str) -> &str {
    origin
        .split_once("://")
        .map_or(origin, |(_, rest)| rest)
        .trim_end_matches('/')
}

/// "Browser" for `so.tilo.browser`, "Code editor" for `so.tilo.code-editor`.
pub(crate) fn app_name(app_id: &str) -> String {
    let last = app_id
        .rsplit('.')
        .next()
        .unwrap_or(app_id)
        .replace('-', " ");
    ui::format::capitalize_first(&last)
}

pub(crate) fn origins_match(item: &SecretItem, candidate: &str) -> bool {
    if candidate.trim().is_empty() {
        return item.origins.is_empty();
    }
    let mut probe = SecretItem::login("", "", "");
    probe.add_origin(candidate).is_ok()
        && probe
            .origins
            .iter()
            .next()
            .is_some_and(|origin| item.origins.contains(origin))
}

pub(crate) struct Core {
    /// The vault; `None` for the fixture captures show.
    pub(crate) model: Option<crate::PasswordsModel>,
    /// Metadata only: passwords, notes and field values are cleared before
    /// an item enters this cache (it feeds rendering, resources and
    /// accessibility).
    pub(crate) items: Vec<SecretItem>,
    pub(crate) selected: Option<SecretId>,
    pub(crate) filter: VaultFilter,
    pub(crate) screen: Screen,
    pub(crate) editing: Option<SecretId>,
    pub(crate) draft_title: String,
    pub(crate) draft_kind: SecretItemKind,
    pub(crate) draft_username: String,
    pub(crate) draft_origin: String,
    pub(crate) draft_password: Zeroizing<String>,
    pub(crate) draft_notes: Zeroizing<String>,
    pub(crate) draft_tags: String,
    pub(crate) draft_custom_fields: Zeroizing<String>,
    pub(crate) draft_error: Option<(DraftField, String)>,
    /// Explicit, short-lived disclosure. The value zeroizes when it expires,
    /// when focus leaves the window, or when another item is revealed.
    pub(crate) revealed: Option<(SecretId, Zeroizing<String>, ui::time::Instant)>,
    /// The last copy, whose outcome can arrive after its confirmation.
    pub(crate) copy: ui::CopyWatch,
    /// A notice for the window to hand its shell, which shows and expires it.
    pub(crate) toast: Option<String>,
    /// The title of an item just saved from the editor, for the window to
    /// say in passing: closing the editor alone looks the same as Cancel.
    pub(crate) saved: Option<String>,
    pub(crate) health: crate::VaultHealth,
    pub(crate) findings: Vec<Finding>,
    pub(crate) app_grants: Vec<CredentialGrant>,
    pub(crate) app_proposals: Vec<accounts::CredentialProposal>,
    model_revision: Option<u64>,
    pub(crate) custody: Option<&'static str>,
    pub(crate) recovery_error: Option<String>,
    pub(crate) import_portal: Option<crate::transfer::Choosing>,
    pub(crate) import_parse: Option<std::sync::mpsc::Receiver<Result<Vec<ImportLogin>, String>>>,
    pub(crate) export_portal: Option<crate::transfer::Saving>,
    pub(crate) backup_portal: Option<crate::transfer::Saving>,
    pub(crate) restore_portal: Option<crate::transfer::Choosing>,
    pub(crate) transfer_poll_deadline: std::cell::Cell<Option<ui::time::Instant>>,
    /// The vault is still writing an edit: a browser page to its storage,
    /// or the frame loop's writer thread to the disk.
    pub(crate) writing: bool,
    pub(crate) backup_password: Zeroizing<String>,
    pub(crate) backup_confirmation: Zeroizing<String>,
    pub(crate) dialog: Option<Dialog>,
    /// A narrow window should bring this pane to the front.
    pub(crate) show_pane: Option<NavPane>,
    /// Scroll the open item into view when the list next shows.
    pub(crate) reveal_open: bool,
    pub(crate) window: Rect,
    /// Bumped whenever the content page must be built again.
    pub(crate) page_revision: u64,
    /// Bumped whenever the item list or its title changes.
    pub(crate) list_revision: u64,
    /// The vault is being opened on a thread of its own: a window on the
    /// compositor's frame loop never waits on the disk for it.
    pub(crate) opening: bool,
    /// Wakes the window once the vault is open.
    wake: std::sync::Arc<std::sync::Mutex<ui::WakeHandle>>,
}

impl Core {
    fn empty(model: Option<crate::PasswordsModel>) -> Self {
        Self {
            model,
            items: Vec::new(),
            selected: None,
            filter: VaultFilter::All,
            screen: Screen::Vault,
            editing: None,
            draft_title: String::new(),
            draft_kind: SecretItemKind::Login,
            draft_username: String::new(),
            draft_origin: String::new(),
            draft_password: Zeroizing::new(String::new()),
            draft_notes: Zeroizing::new(String::new()),
            draft_tags: String::new(),
            draft_custom_fields: Zeroizing::new(String::new()),
            draft_error: None,
            revealed: None,
            copy: ui::CopyWatch::default(),
            toast: None,
            saved: None,
            health: crate::VaultHealth::default(),
            findings: Vec::new(),
            app_grants: Vec::new(),
            app_proposals: Vec::new(),
            model_revision: None,
            writing: false,
            custody: None,
            recovery_error: None,
            import_portal: None,
            import_parse: None,
            export_portal: None,
            backup_portal: None,
            restore_portal: None,
            transfer_poll_deadline: std::cell::Cell::new(None),
            backup_password: Zeroizing::new(String::new()),
            backup_confirmation: Zeroizing::new(String::new()),
            dialog: None,
            show_pane: None,
            reveal_open: true,
            window: Rect::ZERO,
            page_revision: 0,
            list_revision: 0,
            opening: false,
            wake: std::sync::Arc::default(),
        }
    }

    /// The vault in `model`: the person's in a live window.
    pub(crate) fn with_model(model: crate::PasswordsModel) -> Self {
        let mut core = Self::empty(Some(model));
        if core.open_without_waiting() {
            core.refresh();
            core.select_first_visible();
        }
        core
    }

    /// Whether the vault is open; if not, it is opened on a thread of its
    /// own and the window shows it on the sync after that finishes.
    fn open_without_waiting(&mut self) -> bool {
        let Some(model) = &self.model else {
            return true;
        };
        let wake = std::sync::Arc::clone(&self.wake);
        let open = model.open_without_waiting(move || wake.lock_recover().wake());
        self.opening = !open;
        open
    }

    /// Wake the window through `wake` once the vault is open.
    pub(crate) fn wake_through(&self, wake: ui::WakeHandle) {
        *self.wake.lock_recover() = wake;
    }

    /// Fixture items with no vault behind them (captures and tests).
    pub(crate) fn demo() -> Self {
        let mut core = Self::empty(None);
        core.load_demo();
        core
    }

    /// Replace whatever is shown with the fixture vault.
    pub(crate) fn load_demo(&mut self) {
        self.model = None;
        let full = crate::demo::full_items();
        self.health = crate::analyze_health(&full);
        self.findings = crate::findings(&full);
        self.items = crate::demo::items();
        self.app_grants = crate::demo::grants();
        self.custody = Some("System keyring");
        self.recovery_error = None;
        self.filter = VaultFilter::All;
        self.screen = Screen::Vault;
        self.dialog = None;
        self.editing = None;
        self.selected = Some(SecretId("demo-bank".into()));
        self.touch();
    }

    /// Add enough fixture logins that the list scrolls (captures).
    pub(crate) fn extend_demo_for_scrolling(&mut self) {
        self.items.extend(crate::demo::more_items());
        self.items.sort_by_key(|item| item.title.to_lowercase());
        self.reveal_open = true;
        self.touch();
    }

    /// The page and the list must be built again.
    pub(crate) fn touch(&mut self) {
        self.page_revision = self.page_revision.wrapping_add(1);
        self.list_revision = self.list_revision.wrapping_add(1);
    }

    pub(crate) fn touch_page(&mut self) {
        self.page_revision = self.page_revision.wrapping_add(1);
    }

    pub(crate) fn toast(&mut self, message: impl Into<String>) {
        self.toast = Some(message.into());
    }

    /// Whether this window's vault is unlocked: it opened and its key was
    /// found. `None` for a window that opens no vault (a past version, the
    /// fixture captures show), which says nothing about the person's.
    pub(crate) fn secrets_unlocked(&self) -> Option<bool> {
        let model = self.model.as_ref()?;
        if !model.opens_vault() {
            return None;
        }
        Some(self.recovery_error.is_none())
    }

    pub(crate) fn refresh(&mut self) {
        let Some(model) = &self.model else {
            return;
        };
        self.model_revision = model.revision();
        self.recovery_error = model.recovery_error().map(|_| {
            "Your vault could not be unlocked. Restore access to its key, then reopen Passwords."
                .to_owned()
        });
        self.health = model.health();
        let full = model.items();
        self.findings = crate::findings(&full);
        self.app_grants = model.app_grants();
        self.app_proposals = model.app_proposals();
        self.custody = Some(model.custody().label());
        self.items = full
            .into_iter()
            .map(|mut item| {
                item.password.clear();
                // A note or an arbitrary field value can be secret too.
                item.notes.clear();
                for (label, value) in &mut item.fields {
                    if label != "network" {
                        value.clear();
                    }
                }
                item
            })
            .collect();
        self.items.sort_by_key(|item| item.title.to_lowercase());
        if self
            .selected
            .as_ref()
            .is_some_and(|id| !self.items.iter().any(|item| &item.id == id))
        {
            self.selected = None;
        }
        self.touch();
    }

    pub(crate) fn visible(&self) -> Vec<&SecretItem> {
        self.items
            .iter()
            .filter(|item| self.filter.matches(item))
            .collect()
    }

    pub(crate) fn count(&self, filter: VaultFilter) -> usize {
        self.items
            .iter()
            .filter(|item| filter.matches(item))
            .count()
    }

    pub(crate) fn item(&self, id: &SecretId) -> Option<&SecretItem> {
        self.items.iter().find(|item| &item.id == id)
    }

    pub(crate) fn selected_item(&self) -> Option<&SecretItem> {
        self.selected.as_ref().and_then(|id| self.item(id))
    }

    fn select_first_visible(&mut self) {
        if self
            .selected
            .as_ref()
            .and_then(|id| self.item(id))
            .is_some_and(|item| self.filter.matches(item))
        {
            return;
        }
        self.selected = self.visible().first().map(|item| item.id.clone());
    }

    pub(crate) fn select_filter(&mut self, filter: VaultFilter) {
        self.filter = filter;
        self.screen = Screen::Vault;
        self.select_first_visible();
        self.show_pane = Some(NavPane::Overview);
        self.touch();
    }

    /// Show the item `id`, clearing a filter that hides it. `false` when the
    /// vault has no such item.
    pub(crate) fn open(&mut self, id: &SecretId) -> bool {
        let Some(listed) = self.item(id).map(|item| self.filter.matches(item)) else {
            return false;
        };
        if !listed {
            self.filter = VaultFilter::All;
        }
        if self.screen == Screen::Editor {
            self.close_editor();
        }
        self.screen = Screen::Vault;
        if self.selected.as_ref() != Some(id) {
            self.selected = Some(id.clone());
            self.revealed = None;
        }
        self.reveal_open = true;
        self.show_pane = Some(NavPane::Content);
        self.touch();
        true
    }

    /// Move the selection by one row (arrow keys).
    pub(crate) fn step(&mut self, forward: bool) {
        let visible: Vec<SecretId> = self.visible().iter().map(|item| item.id.clone()).collect();
        let Some(last) = visible.len().checked_sub(1) else {
            return;
        };
        let current = self
            .selected
            .as_ref()
            .and_then(|id| visible.iter().position(|candidate| candidate == id));
        let next = match current {
            None => 0,
            Some(at) if forward => at.saturating_add(1).min(last),
            Some(at) => at.saturating_sub(1),
        };
        if let Some(id) = visible.get(next).cloned() {
            self.open(&id);
            self.show_pane = None;
        }
    }

    pub(crate) fn toggle_favorite(&mut self, id: &SecretId) {
        let Some(was) = self.item(id).map(is_favorite) else {
            return;
        };
        if let Err(error) = self.set_favorite(id, !was) {
            self.toast(error);
        }
    }

    /// Make the item `id` a favorite or not, saying so; nothing changes when
    /// it already is what was asked.
    pub(crate) fn set_favorite(&mut self, id: &SecretId, favorite: bool) -> Result<(), String> {
        let Some(was) = self.item(id).map(is_favorite) else {
            return Err("That item is no longer in your vault".to_owned());
        };
        if was == favorite {
            return Ok(());
        }
        if let Some(model) = &mut self.model {
            let Some(mut full) = model.items().into_iter().find(|item| &item.id == id) else {
                return Err("That item is no longer in your vault".to_owned());
            };
            if favorite {
                full.tags.insert("favorite".into());
            } else {
                full.tags.remove("favorite");
            }
            model.update(&mut full)?;
            self.flush_vault();
            self.refresh();
        } else if let Some(item) = self.items.iter_mut().find(|item| &item.id == id) {
            if favorite {
                item.tags.insert("favorite".into());
            } else {
                item.tags.remove("favorite");
            }
            self.touch();
        }
        self.toast(if favorite {
            "Added to favorites"
        } else {
            "Removed from favorites"
        });
        Ok(())
    }

    pub(crate) fn revealed_value(&self, id: &SecretId) -> Option<&str> {
        self.revealed
            .as_ref()
            .filter(|(revealed, _, until)| revealed == id && *until > ui::motion::frame_now())
            .map(|(_, value, _)| value.as_str())
    }

    pub(crate) fn has_private_note(&self, id: &SecretId) -> bool {
        self.model.as_ref().is_some_and(|model| {
            model
                .items()
                .into_iter()
                .find(|candidate| &candidate.id == id)
                .is_some_and(|candidate| !candidate.notes.trim().is_empty())
        })
    }

    pub(crate) fn copy_secret(&mut self) {
        let Some(item) = self.selected_item() else {
            return;
        };
        let (id, kind) = (item.id.clone(), item.kind);
        let Some(model) = &self.model else {
            return;
        };
        match model.reveal_value(&id) {
            Ok(secret) => {
                if !self
                    .copy
                    .watch(ui::try_clipboard_copy_secret(&secret, CLIPBOARD_FOR))
                {
                    self.toast("Could not copy to the clipboard. Try again.");
                    return;
                }
                let label = secret_label(kind).to_lowercase();
                // Without threads (a browser page) the clipboard is not cleared.
                self.toast(if ui::rt::available(ui::rt::Feature::Threads) {
                    format!("Copied {label}. The clipboard clears in 30 seconds.")
                } else {
                    format!("Copied {label}")
                });
            }
            Err(error) => self.toast(error),
        }
    }

    pub(crate) fn reveal_secret(&mut self) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        if self.revealed_value(&id).is_some() {
            self.revealed = None;
            self.touch_page();
            return;
        }
        let Some(model) = &self.model else {
            return;
        };
        match model.reveal_value(&id) {
            Ok(secret) => {
                self.revealed = Some((
                    id,
                    secret,
                    ui::time::now()
                        .checked_add(REVEAL_FOR)
                        .unwrap_or_else(ui::time::now),
                ));
                self.touch_page();
            }
            Err(error) => self.toast(error),
        }
    }

    pub(crate) fn hide_revealed(&mut self) {
        if self.revealed.take().is_some() {
            self.touch_page();
        }
    }

    /// Hide a disclosure whose time ran out.
    pub(crate) fn expire(&mut self) {
        if self
            .revealed
            .as_ref()
            .is_some_and(|(_, _, until)| *until <= ui::time::now())
        {
            self.hide_revealed();
        }
    }

    pub(crate) fn copy_identity(&mut self) {
        let Some(item) = self.selected_item() else {
            return;
        };
        let value = identity(item).to_owned();
        let kind = item.kind;
        if value.is_empty() {
            return;
        }
        if !self.copy.watch(ui::try_clipboard_copy(&value)) {
            self.toast("Could not copy to the clipboard. Try again.");
            return;
        }
        self.toast(match kind {
            SecretItemKind::Wifi => "Copied network name",
            SecretItemKind::ApiKey => "Copied account",
            _ => "Copied username",
        });
    }

    pub(crate) fn copy_origins(&mut self) {
        let Some(item) = self.selected_item() else {
            return;
        };
        if item.origins.is_empty() {
            return;
        }
        let count = item.origins.len();
        let origins = item.origins.iter().cloned().collect::<Vec<_>>().join("\n");
        if !self.copy.watch(ui::try_clipboard_copy(&origins)) {
            self.toast("Could not copy to the clipboard. Try again.");
            return;
        }
        self.toast(if count == 1 {
            "Copied address".to_owned()
        } else {
            format!("Copied {count} addresses")
        });
    }

    pub(crate) fn begin_add(&mut self) {
        self.editing = None;
        self.draft_title.clear();
        self.draft_kind = match self.filter {
            VaultFilter::Kind(kind) => kind,
            VaultFilter::All | VaultFilter::Favorites => SecretItemKind::Login,
        };
        self.draft_username.clear();
        self.draft_origin.clear();
        self.draft_password.clear();
        self.draft_notes.clear();
        self.draft_tags.clear();
        self.draft_custom_fields.clear();
        self.draft_error = None;
        self.screen = Screen::Editor;
        self.show_pane = Some(NavPane::Content);
        self.touch();
    }

    pub(crate) fn begin_edit(&mut self) {
        let Some(cached) = self.selected_item() else {
            return;
        };
        let item = self
            .model
            .as_ref()
            .and_then(|model| model.items().into_iter().find(|item| item.id == cached.id))
            .unwrap_or_else(|| cached.clone());
        self.editing = Some(item.id.clone());
        self.draft_kind = item.kind;
        self.draft_title.clone_from(&item.title);
        identity(&item).clone_into(&mut self.draft_username);
        self.draft_origin = item.origins.iter().cloned().collect::<Vec<_>>().join(", ");
        self.draft_password.clear();
        self.draft_notes = Zeroizing::new(item.notes);
        self.draft_tags = item
            .tags
            .iter()
            .filter(|tag| tag.as_str() != "favorite")
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let mut custom_fields = item
            .fields
            .iter()
            .filter(|(label, _)| label.as_str() != "network")
            .collect::<Vec<_>>();
        custom_fields.sort_by_key(|(left, _)| *left);
        self.draft_custom_fields = Zeroizing::new(
            custom_fields
                .into_iter()
                .map(|(label, value)| format!("{label} = {}", escape_custom_value(value)))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        self.draft_error = None;
        self.screen = Screen::Editor;
        self.show_pane = Some(NavPane::Content);
        self.touch();
    }

    fn close_editor(&mut self) {
        self.screen = Screen::Vault;
        self.draft_error = None;
        self.clear_sensitive_draft();
    }

    pub(crate) fn cancel_edit(&mut self) {
        self.close_editor();
        self.show_pane = Some(if self.editing.is_some() {
            NavPane::Content
        } else {
            NavPane::Overview
        });
        self.touch();
    }

    pub(crate) fn clear_sensitive_draft(&mut self) {
        self.draft_password.clear();
        self.draft_notes.clear();
        self.draft_tags.clear();
        self.draft_custom_fields.clear();
    }

    pub(crate) fn set_draft(&mut self, field: DraftField, value: &str) {
        // An edit answers the field's complaint; the next save checks again.
        if self
            .draft_error
            .as_ref()
            .is_some_and(|(failed, _)| *failed == field)
        {
            self.draft_error = None;
            self.touch_page();
        }
        match field {
            DraftField::Name => value.clone_into(&mut self.draft_title),
            DraftField::Username => value.clone_into(&mut self.draft_username),
            DraftField::Origin => value.clone_into(&mut self.draft_origin),
            DraftField::Password => self.draft_password = Zeroizing::new(value.to_owned()),
            DraftField::Notes => self.draft_notes = Zeroizing::new(value.to_owned()),
            DraftField::Tags => value.clone_into(&mut self.draft_tags),
            DraftField::CustomFields => {
                self.draft_custom_fields = Zeroizing::new(value.to_owned());
            }
        }
    }

    pub(crate) fn draft(&self, field: DraftField) -> &str {
        match field {
            DraftField::Name => &self.draft_title,
            DraftField::Username => &self.draft_username,
            DraftField::Origin => &self.draft_origin,
            DraftField::Password => &self.draft_password,
            DraftField::Notes => &self.draft_notes,
            DraftField::Tags => &self.draft_tags,
            DraftField::CustomFields => &self.draft_custom_fields,
        }
    }

    /// The fields the editor shows for the draft's kind, in order.
    pub(crate) fn draft_fields(&self) -> Vec<DraftField> {
        let mut fields = vec![DraftField::Name];
        match self.draft_kind {
            SecretItemKind::Login | SecretItemKind::ApiKey => fields.extend([
                DraftField::Username,
                DraftField::Origin,
                DraftField::Password,
            ]),
            SecretItemKind::Wifi | SecretItemKind::Other => {
                fields.extend([DraftField::Username, DraftField::Password]);
            }
            SecretItemKind::SecureNote => {}
        }
        fields.extend([
            DraftField::Notes,
            DraftField::Tags,
            DraftField::CustomFields,
        ]);
        fields
    }

    pub(crate) fn set_kind(&mut self, kind: SecretItemKind) {
        if self.draft_kind != kind {
            self.draft_kind = kind;
            self.draft_error = None;
            self.touch_page();
        }
    }

    fn reject(&mut self, field: DraftField, message: impl Into<String>) {
        self.draft_error = Some((field, message.into()));
        self.touch_page();
    }

    pub(crate) fn generate_password(&mut self) {
        match GeneratedPasswordPolicy::default().generate() {
            Ok(value) => {
                self.draft_password = Zeroizing::new(value.to_string());
                if self
                    .draft_error
                    .as_ref()
                    .is_some_and(|(field, _)| *field == DraftField::Password)
                {
                    self.draft_error = None;
                }
                self.touch_page();
                self.toast(format!(
                    "Generated a strong {}",
                    secret_label(self.draft_kind).to_lowercase()
                ));
            }
            Err(error) => self.toast(format!("Could not generate a password: {error:?}")),
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "saving a draft validates and persists every item kind in one sequential flow"
    )]
    pub(crate) fn save_draft(&mut self) {
        if self.draft_title.trim().is_empty() {
            self.reject(DraftField::Name, "Add a name so you can find this item");
            return;
        }
        if self.draft_kind == SecretItemKind::SecureNote && self.draft_notes.trim().is_empty() {
            self.reject(DraftField::Notes, "Add something to the note");
            return;
        }
        if self.editing.is_none()
            && !matches!(
                self.draft_kind,
                SecretItemKind::SecureNote | SecretItemKind::Login
            )
            && self.draft_password.is_empty()
        {
            let label = secret_label(self.draft_kind).to_lowercase();
            self.reject(DraftField::Password, format!("Add the {label}"));
            return;
        }
        let custom_fields = match parse_custom_fields(&self.draft_custom_fields) {
            Ok(fields) => fields,
            Err(error) => {
                self.reject(DraftField::CustomFields, error);
                return;
            }
        };
        let Some(model) = self.model.as_mut() else {
            // The fixture vault has no store: close the editor as a save would.
            self.close_editor();
            self.touch();
            return;
        };
        let existing = self
            .editing
            .as_ref()
            .and_then(|id| model.items().into_iter().find(|item| &item.id == id));
        if self.editing.is_some() && existing.is_none() {
            self.toast("This item is no longer in your vault");
            return;
        }
        let password = if self.draft_password.is_empty() {
            existing
                .as_ref()
                .map_or_else(String::new, |item| item.password.clone())
        } else {
            self.draft_password.to_string()
        };
        let password = if password.is_empty() && self.draft_kind == SecretItemKind::Login {
            GeneratedPasswordPolicy::default()
                .generate()
                .map(|value| value.to_string())
                .unwrap_or_default()
        } else {
            password
        };
        let mut item = existing.unwrap_or_else(|| SecretItem::login("", "", ""));
        item.kind = self.draft_kind;
        item.title = self.draft_title.trim().into();
        item.username = self.draft_username.trim().into();
        item.password = if self.draft_kind == SecretItemKind::SecureNote {
            String::new()
        } else {
            password
        };
        item.notes = self.draft_notes.to_string();
        let favorite = item.tags.contains("favorite");
        item.tags.clear();
        if favorite {
            item.tags.insert("favorite".into());
        }
        for tag in self
            .draft_tags
            .split([',', '\n'])
            .map(str::trim)
            .filter(|tag| !tag.is_empty() && *tag != "favorite")
        {
            item.tags.insert(tag.to_string());
        }
        item.fields.retain(|label, _| label == "network");
        item.fields.extend(custom_fields);
        item.origins.clear();
        if self.draft_kind == SecretItemKind::Wifi {
            item.fields
                .insert("network".into(), self.draft_username.trim().into());
        } else {
            // `network` is a Passwords-owned projection of the Wi-Fi name;
            // remove it when changing kinds, but retain every app-specific
            // custom field we do not own or understand.
            item.fields.remove("network");
        }
        let origins: Vec<String> = self
            .draft_origin
            .split([',', '\n', ';'])
            .map(str::trim)
            .filter(|origin| !origin.is_empty())
            .map(str::to_owned)
            .collect();
        if matches!(
            self.draft_kind,
            SecretItemKind::Login | SecretItemKind::ApiKey
        ) {
            for origin in &origins {
                // A bare host ("git.tilo.so") means its https site.
                let added = item.add_origin(origin).is_ok()
                    || (!origin.contains("://")
                        && item.add_origin(&format!("https://{origin}")).is_ok());
                if !added {
                    self.reject(
                        DraftField::Origin,
                        format!("{origin} is not a web address, such as https://example.com"),
                    );
                    return;
                }
            }
        }
        match model.save_item(&mut item) {
            Ok(id) => {
                self.flush_vault();
                self.selected = Some(id);
                self.saved = Some(item.title.clone());
                self.refresh();
                self.close_editor();
                self.show_pane = Some(NavPane::Content);
                self.reveal_open = true;
                self.touch();
            }
            Err(error) => self.toast(error),
        }
    }

    pub(crate) fn request_delete(&mut self) {
        let Some(id) = self.editing.clone().or_else(|| self.selected.clone()) else {
            return;
        };
        let title = self
            .item(&id)
            .map_or_else(|| "this item".to_owned(), |item| item.title.clone());
        self.dialog = Some(Dialog::DeleteConfirm { id, title });
        self.touch_page();
    }

    pub(crate) fn confirm_delete(&mut self) {
        let Some(Dialog::DeleteConfirm { id, .. }) = self.dialog.take() else {
            return;
        };
        if let Err(error) = self.delete(&id) {
            self.toast(error);
        }
    }

    pub(crate) fn delete(&mut self, id: &SecretId) -> Result<(), String> {
        let result = if let Some(model) = &mut self.model {
            match model.remove(id) {
                Ok(true) => {
                    self.flush_vault();
                    self.selected = None;
                    self.refresh();
                    self.close_editor();
                    self.select_first_visible();
                    self.show_pane = Some(NavPane::Overview);
                    Ok(())
                }
                Ok(false) => Err("That item is no longer in your vault".to_owned()),
                Err(error) => Err(format!("Could not delete the item: {error}")),
            }
        } else if self.item(id).is_some() {
            self.items.retain(|item| &item.id != id);
            self.selected = None;
            self.close_editor();
            self.select_first_visible();
            Ok(())
        } else {
            Err("That item is no longer in your vault".to_owned())
        };
        self.touch();
        result
    }

    pub(crate) fn close_dialog(&mut self) {
        if self.dialog.take().is_some() {
            self.clear_backup_form();
            self.touch_page();
        }
    }

    pub(crate) fn revoke(&mut self, id: &str) -> Result<(), String> {
        let grant = self
            .app_grants
            .iter()
            .find(|grant| grant.id() == id)
            .cloned()
            .ok_or_else(|| crate::CATALOG.t("passwords-permission-expired"))?;
        let name = app_name(&grant.app_id);
        if let Some(model) = &mut self.model {
            match model.revoke_app_grant(&grant) {
                Ok(true) => {
                    self.refresh();
                    self.toast(format!("{name} can no longer use this item"));
                }
                Ok(false) => return Err(crate::CATALOG.t("passwords-permission-expired")),
                Err(error) => {
                    self.toast(error.clone());
                    return Err(error);
                }
            }
        } else {
            self.app_grants.retain(|grant| grant.id() != id);
            self.touch();
        }
        Ok(())
    }
    pub(crate) fn model_changed(&self) -> bool {
        if self
            .model
            .as_ref()
            .and_then(crate::PasswordsModel::proposal_wake_after)
            .is_some_and(|after| after.is_zero())
        {
            return true;
        }
        self.model
            .as_ref()
            .and_then(crate::PasswordsModel::revision)
            .is_some_and(|revision| Some(revision) != self.model_revision)
    }
    pub(crate) fn refresh_model(&mut self) {
        if self.opening {
            if self.open_without_waiting() {
                self.refresh();
                self.select_first_visible();
            }
            return;
        }
        let proposals_changed = !self.app_proposals.is_empty()
            && self
                .model
                .as_ref()
                .is_some_and(|model| model.app_proposals() != self.app_proposals);
        if proposals_changed || self.model_changed() {
            self.revealed = None;
            self.refresh();
        }
    }

    pub(crate) fn request_import(&mut self) {
        if self.model.is_none() || self.import_portal.is_some() {
            return;
        }
        self.import_portal = Some(crate::transfer::Choosing::send(
            crate::transfer::import_request(),
            usize::try_from(crate::transfer::MAX_IMPORT_BYTES + 1).unwrap_or(usize::MAX),
        ));
    }

    pub(crate) fn request_export(&mut self) {
        if self.model.is_some() {
            self.dialog = Some(Dialog::ExportPlaintext {
                acknowledged: false,
                reminded: false,
            });
            self.touch_page();
        }
    }

    pub(crate) fn acknowledge(&mut self, acknowledged: bool) {
        if let Some(Dialog::ExportPlaintext {
            acknowledged: current,
            reminded,
        }) = &mut self.dialog
        {
            *current = acknowledged;
            if acknowledged && *reminded {
                *reminded = false;
                self.touch_page();
            }
        }
    }

    pub(crate) fn begin_plaintext_export(&mut self) {
        let Some(Dialog::ExportPlaintext {
            acknowledged,
            reminded,
        }) = &mut self.dialog
        else {
            return;
        };
        if !*acknowledged {
            *reminded = true;
            self.touch_page();
            return;
        }
        if self.export_portal.is_some() {
            return;
        }
        self.dialog = None;
        self.touch_page();
        let saving = crate::transfer::Saving::send(crate::transfer::export_request(), || {
            self.plaintext_export()
                .map(|(csv, count)| (Zeroizing::new(csv.as_bytes().to_vec()), count))
        });
        match saving {
            Ok(saving) => self.export_portal = Some(saving),
            Err(error) => self.toast(error),
        }
    }

    pub(crate) fn clear_backup_form(&mut self) {
        self.backup_password.clear();
        self.backup_confirmation.clear();
    }

    pub(crate) fn request_backup(&mut self) {
        if self.model.is_some() {
            self.clear_backup_form();
            self.dialog = Some(Dialog::Backup { error: None });
            self.touch_page();
        }
    }

    pub(crate) fn request_restore(&mut self) {
        if self.model.is_some() && self.restore_portal.is_none() {
            self.restore_portal = Some(crate::transfer::Choosing::send(
                crate::transfer::restore_request(),
                crate::transfer::MAX_BACKUP_BYTES + 1,
            ));
        }
    }

    /// What the backup form's confirmation field says while it is typed:
    /// a mismatch as soon as the confirmation stops matching the start of
    /// the password, nothing while it is empty or still on its way there.
    pub(crate) fn backup_confirmation_problem(&self) -> Option<&'static str> {
        (!self.backup_confirmation.is_empty()
            && !self
                .backup_password
                .starts_with(self.backup_confirmation.as_str()))
        .then_some("The passwords do not match")
    }

    pub(crate) fn backup_form_error(&self, confirmation: bool) -> Option<&'static str> {
        if self.backup_password.chars().count() < 12 {
            Some("Use at least 12 characters")
        } else if confirmation && *self.backup_password != *self.backup_confirmation {
            Some("The passwords do not match")
        } else {
            None
        }
    }

    pub(crate) fn begin_encrypted_backup(&mut self) {
        if !matches!(self.dialog, Some(Dialog::Backup { .. })) || self.backup_portal.is_some() {
            return;
        }
        if let Some(error) = self.backup_form_error(true) {
            self.dialog = Some(Dialog::Backup { error: Some(error) });
            self.touch_page();
            return;
        }
        self.dialog = None;
        self.touch_page();
        let passphrase = self.backup_password.clone();
        let saving = crate::transfer::Saving::send(crate::transfer::backup_request(), || {
            self.encrypted_backup(&passphrase)
        });
        match saving {
            Ok(saving) => self.backup_portal = Some(saving),
            Err(error) => {
                self.clear_backup_form();
                self.toast(error);
            }
        }
    }

    pub(crate) fn apply_encrypted_restore(&mut self) {
        let Some(Dialog::Restore { bytes, .. }) = self.dialog.take() else {
            return;
        };
        let passphrase = self.backup_password.clone();
        match crate::transfer::import_encrypted_backup(&bytes, &passphrase) {
            Ok(items) => {
                let count = items.len();
                let Some(model) = &mut self.model else {
                    return;
                };
                let mut restored = 0_usize;
                for mut item in items {
                    if model.update(&mut item).is_ok() {
                        restored = restored.saturating_add(1);
                    }
                }
                self.flush_vault();
                self.refresh();
                self.toast(if restored == count {
                    format!("Restored {restored} items")
                } else {
                    format!("Restored {restored} of {count} items")
                });
                self.clear_backup_form();
            }
            Err(error) => {
                self.dialog = Some(Dialog::Restore {
                    bytes,
                    error: Some(error),
                });
            }
        }
        self.touch_page();
    }

    /// Per row of an import: whether the vault already holds that login.
    pub(crate) fn import_skips(&self, rows: &[ImportLogin]) -> Vec<bool> {
        rows.iter()
            .map(|row| {
                self.items.iter().any(|item| {
                    item.title.eq_ignore_ascii_case(&row.title)
                        && item.username.eq_ignore_ascii_case(&row.username)
                        && origins_match(item, &row.origin)
                })
            })
            .collect()
    }

    pub(crate) fn apply_import(&mut self) {
        let Some(Dialog::ImportPreview { rows, .. }) = self.dialog.take() else {
            return;
        };
        self.touch_page();
        let existing = self
            .model
            .as_ref()
            .map_or_else(Vec::new, crate::PasswordsModel::items);
        let Some(model) = &mut self.model else {
            return;
        };
        let mut imported = 0usize;
        let mut skipped = 0usize;
        for row in rows {
            if existing.iter().any(|item| {
                item.title.eq_ignore_ascii_case(&row.title)
                    && item.username.eq_ignore_ascii_case(&row.username)
                    && origins_match(item, &row.origin)
            }) {
                skipped = skipped.saturating_add(1);
                continue;
            }
            let mut item = SecretItem::login(
                row.title.to_string(),
                row.username.to_string(),
                row.password.to_string(),
            );
            item.notes = row.notes.to_string();
            if !row.origin.is_empty() && item.add_origin(&row.origin).is_err() {
                skipped = skipped.saturating_add(1);
                continue;
            }
            if model.update(&mut item).is_ok() {
                imported = imported.saturating_add(1);
            } else {
                skipped = skipped.saturating_add(1);
            }
        }
        self.flush_vault();
        self.refresh();
        self.toast(match (imported, skipped) {
            (1, 0) => "Imported 1 login".to_owned(),
            (n, 0) => format!("Imported {n} logins"),
            (n, s) => format!("Imported {n} logins and skipped {s}"),
        });
    }

    /// The vault as a browser CSV, and how many items it holds.
    fn plaintext_export(&self) -> Result<(Zeroizing<String>, usize), String> {
        let Some(model) = &self.model else {
            return Err("There is no vault to export".into());
        };
        let items = model.items();
        Ok((crate::transfer::export_browser_csv(&items), items.len()))
    }

    /// The vault as an encrypted backup under `passphrase`, and how many
    /// items it holds.
    fn encrypted_backup(&self, passphrase: &str) -> Result<(Zeroizing<Vec<u8>>, usize), String> {
        let Some(model) = &self.model else {
            return Err("There is no vault to back up".into());
        };
        let items = model.items();
        let backup = crate::transfer::export_encrypted_backup(&items, passphrase)?;
        Ok((backup, items.len()))
    }

    fn write_plaintext_export(&self, path: &std::path::Path) -> Result<usize, String> {
        let (csv, count) = self.plaintext_export()?;
        tilo_files::native::atomic::write_with_mode(path, csv.as_bytes(), 0o600)
            .map_err(|_| "Could not finish writing the file".to_string())?;
        Ok(count)
    }

    fn write_encrypted_backup(
        &self,
        path: &std::path::Path,
        passphrase: &str,
    ) -> Result<usize, String> {
        let (backup, count) = self.encrypted_backup(passphrase)?;
        tilo_files::native::atomic::write_with_mode(path, &backup, 0o600)
            .map_err(|_| "Could not finish writing the backup".to_string())?;
        Ok(count)
    }

    /// Learn whether the vault's last edits are kept yet: a browser page
    /// writes its storage asynchronously, and on the frame loop a writer
    /// thread writes the disk, so a save is kept a moment after it shows. A
    /// failed write is said, not dropped.
    pub(crate) fn flush_vault(&mut self) {
        let Some(model) = self.model.as_mut() else {
            self.writing = false;
            return;
        };
        match model.flush() {
            Ok(kept) => self.writing = !kept,
            Err(error) => {
                self.writing = false;
                self.toast(format!("Couldn't keep your last change: {error}"));
            }
        }
    }

    pub(crate) fn transfers_pending(&self) -> bool {
        self.writing
            || self.import_portal.is_some()
            || self.import_parse.is_some()
            || self.export_portal.is_some()
            || self.backup_portal.is_some()
            || self.restore_portal.is_some()
    }

    #[expect(
        clippy::too_many_lines,
        reason = "drains each transfer channel in turn; keeping the states together keeps the vault update ordering visible"
    )]
    pub(crate) fn poll_transfers(&mut self) {
        use crate::transfer::{Chosen, Saving};
        use std::sync::mpsc::TryRecvError;
        use ui::portal::{PortalOutcome, SaveOutcome};

        if self.copy.failed() {
            self.toast("Could not copy to the clipboard. Try again.");
        }

        if self.writing {
            self.flush_vault();
        }

        if let Some(rx) = self.restore_portal.as_ref() {
            match rx.try_recv() {
                Ok(Chosen::Opened(file)) => {
                    self.restore_portal = None;
                    match crate::transfer::read_encrypted_backup(file) {
                        Ok(bytes) => {
                            self.clear_backup_form();
                            self.dialog = Some(Dialog::Restore { bytes, error: None });
                            self.touch_page();
                        }
                        Err(error) => self.toast(error),
                    }
                }
                Ok(Chosen::Cancelled) => self.restore_portal = None,
                Ok(Chosen::Failed(error)) => {
                    self.restore_portal = None;
                    self.toast(format!("Could not open the file chooser: {error}"));
                }
                Err(TryRecvError::Disconnected) => {
                    self.restore_portal = None;
                    self.toast("The file chooser closed unexpectedly");
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = self.import_portal.as_ref() {
            match rx.try_recv() {
                Ok(Chosen::Opened(file)) => {
                    self.import_portal = None;
                    let (tx, rx) = std::sync::mpsc::channel();
                    let spawned = ui::rt::spawn_thread("passwords-csv-parser", move || {
                        let _ = tx.send(crate::transfer::parse_browser_csv(file));
                    });
                    match spawned {
                        Ok(_) => self.import_parse = Some(rx),
                        Err(error) => self.toast(format!("Could not read the file: {error}")),
                    }
                }
                Ok(Chosen::Cancelled) => self.import_portal = None,
                Ok(Chosen::Failed(error)) => {
                    self.import_portal = None;
                    self.toast(format!("Could not open the file chooser: {error}"));
                }
                Err(TryRecvError::Disconnected) => {
                    self.import_portal = None;
                    self.toast("The file chooser closed unexpectedly");
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = self.import_parse.as_ref() {
            match rx.try_recv() {
                Ok(Ok(rows)) => {
                    self.import_parse = None;
                    let skipped = self.import_skips(&rows);
                    self.dialog = Some(Dialog::ImportPreview { rows, skipped });
                    self.touch_page();
                }
                Ok(Err(error)) => {
                    self.import_parse = None;
                    self.toast(error);
                }
                Err(TryRecvError::Disconnected) => {
                    self.import_parse = None;
                    self.toast("Reading the file stopped unexpectedly");
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(Saving::Contents(rx, _)) = self.export_portal.as_ref() {
            match rx.try_recv() {
                Ok(SaveOutcome::Saved | SaveOutcome::Cancelled) => self.export_portal = None,
                Ok(SaveOutcome::Failed(error)) => {
                    self.export_portal = None;
                    self.toast(format!("Could not save the file: {error}"));
                }
                Err(TryRecvError::Disconnected) => {
                    self.export_portal = None;
                    self.toast("The save dialog closed unexpectedly");
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(Saving::Contents(rx, count)) = self.backup_portal.as_ref() {
            let count = *count;
            match rx.try_recv() {
                Ok(SaveOutcome::Saved) => {
                    self.backup_portal = None;
                    self.clear_backup_form();
                    self.toast(format!("Backed up {count} items"));
                }
                Ok(SaveOutcome::Cancelled) => {
                    self.backup_portal = None;
                    self.clear_backup_form();
                }
                Ok(SaveOutcome::Failed(error)) => {
                    self.backup_portal = None;
                    self.clear_backup_form();
                    self.toast(format!("Could not save the backup: {error}"));
                }
                Err(TryRecvError::Disconnected) => {
                    self.backup_portal = None;
                    self.clear_backup_form();
                    self.toast("The save dialog closed unexpectedly");
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(Saving::Path(rx)) = self.export_portal.as_ref() {
            match rx.try_recv() {
                Ok(PortalOutcome::Opened(mut path)) => {
                    self.export_portal = None;
                    if path
                        .extension()
                        .and_then(|v| v.to_str())
                        .is_none_or(|ext| !ext.eq_ignore_ascii_case("csv"))
                    {
                        path.set_extension("csv");
                    }
                    match self.write_plaintext_export(&path) {
                        Ok(_) => {}
                        Err(error) => self.toast(error),
                    }
                }
                Ok(PortalOutcome::Cancelled) => self.export_portal = None,
                Ok(PortalOutcome::Failed(error)) => {
                    self.export_portal = None;
                    self.toast(format!("Could not open the save dialog: {error}"));
                }
                Err(TryRecvError::Disconnected) => {
                    self.export_portal = None;
                    self.toast("The save dialog closed unexpectedly");
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(Saving::Path(rx)) = self.backup_portal.as_ref() {
            match rx.try_recv() {
                Ok(PortalOutcome::Opened(mut path)) => {
                    self.backup_portal = None;
                    if path
                        .extension()
                        .and_then(|v| v.to_str())
                        .is_none_or(|ext| !ext.eq_ignore_ascii_case("tilopw"))
                    {
                        path.set_extension("tilopw");
                    }
                    let passphrase = self.backup_password.clone();
                    match self.write_encrypted_backup(&path, &passphrase) {
                        Ok(count) => self.toast(format!("Backed up {count} items")),
                        Err(error) => self.toast(error),
                    }
                    self.clear_backup_form();
                }
                Ok(PortalOutcome::Cancelled) => {
                    self.backup_portal = None;
                    self.clear_backup_form();
                }
                Ok(PortalOutcome::Failed(error)) => {
                    self.backup_portal = None;
                    self.clear_backup_form();
                    self.toast(format!("Could not open the save dialog: {error}"));
                }
                Err(TryRecvError::Disconnected) => {
                    self.backup_portal = None;
                    self.clear_backup_form();
                    self.toast("The save dialog closed unexpectedly");
                }
                Err(TryRecvError::Empty) => {}
            }
        }
    }

    /// When the window next needs to wake: a pending transfer to poll or a
    /// disclosure to hide.
    pub(crate) fn next_wake(&self) -> Option<ui::time::Instant> {
        let transfer = if self.transfers_pending() {
            // Keep the deadline fixed until sync consumes the tick. Returning
            // now + 16 ms on every query would postpone it indefinitely.
            let deadline = self.transfer_poll_deadline.get().unwrap_or_else(|| {
                ui::time::now()
                    .checked_add(std::time::Duration::from_millis(16))
                    .unwrap_or_else(ui::time::now)
            });
            self.transfer_poll_deadline.set(Some(deadline));
            Some(deadline)
        } else {
            self.transfer_poll_deadline.set(None);
            None
        };
        [
            transfer,
            self.model
                .as_ref()
                .and_then(crate::PasswordsModel::proposal_wake_after)
                .and_then(|after| ui::time::now().checked_add(after)),
            self.revealed.as_ref().map(|(_, _, until)| *until),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Apply a control's request.
    pub(crate) fn apply(&mut self, request: Request) {
        match request {
            Request::Navigate(id) => self.navigate(&id),
            Request::Open(id) => {
                self.open(&id);
            }
            Request::ToggleFavorite(id) => self.toggle_favorite(&id),
            Request::NewItem => self.begin_add(),
            Request::Edit => self.begin_edit(),
            Request::CancelEdit => self.cancel_edit(),
            Request::Save => self.save_draft(),
            Request::Generate => self.generate_password(),
            Request::SetKind(kind) => self.set_kind(kind),
            Request::CopyIdentity => self.copy_identity(),
            Request::CopySecret => self.copy_secret(),
            Request::RevealSecret => self.reveal_secret(),
            Request::CopyOrigins => self.copy_origins(),
            Request::Delete => self.request_delete(),
            Request::ConfirmDelete => self.confirm_delete(),
            Request::CloseDialog => self.close_dialog(),
            Request::Import => self.request_import(),
            Request::ConfirmImport => self.apply_import(),
            Request::Export => self.request_export(),
            Request::Acknowledge(on) => self.acknowledge(on),
            Request::ConfirmExport => self.begin_plaintext_export(),
            Request::Backup => self.request_backup(),
            Request::ConfirmBackup => self.begin_encrypted_backup(),
            Request::Restore => self.request_restore(),
            Request::ConfirmRestore => self.apply_encrypted_restore(),
            Request::Revoke(id) => {
                let _ = self.revoke(&id);
            }
            Request::ReviewAccess { proposal, lifetime } => {
                if self
                    .app_proposals
                    .iter()
                    .any(|pending| pending.id == proposal)
                {
                    self.dialog = Some(Dialog::AccessConfirm { proposal, lifetime });
                    self.touch_page();
                }
            }
            Request::ConfirmAccess => {
                if let Some(Dialog::AccessConfirm { proposal, lifetime }) = self.dialog.take() {
                    if let Err(error) =
                        self.perform(PasswordsEffect::ApproveAccess { proposal, lifetime })
                    {
                        self.toast(error);
                    }
                    self.touch_page();
                }
            }
            Request::DenyAccess(proposal) => {
                if let Err(error) = self.perform(PasswordsEffect::DenyAccess { proposal }) {
                    self.toast(error);
                }
            }
        }
    }

    /// The sidebar row id of the place on screen.
    pub(crate) fn place_id(&self) -> String {
        match self.screen {
            Screen::Vault | Screen::Editor => format!("filter:{}", self.filter.as_str()),
            Screen::Review => "review".into(),
            Screen::AppAccess => "app-access".into(),
            Screen::Transfer => "transfer".into(),
        }
    }

    pub(crate) fn navigate(&mut self, id: &str) {
        if let Some(filter) = id.strip_prefix("filter:") {
            self.select_filter(VaultFilter::parse(filter));
            return;
        }
        let screen = match id {
            "review" => Screen::Review,
            "app-access" => Screen::AppAccess,
            "transfer" => Screen::Transfer,
            _ => return,
        };
        if self.screen == Screen::Editor {
            self.close_editor();
        }
        self.screen = screen;
        self.show_pane = Some(NavPane::Content);
        self.touch();
    }

    /// Whether a draft is open in the editor.
    pub(crate) fn is_editing(&self) -> bool {
        self.screen == Screen::Editor
    }

    /// Answer an effect invoked by the palette, an agent, a script or a
    /// search hit.
    pub(crate) fn perform(&mut self, effect: PasswordsEffect) -> Result<(), String> {
        self.refresh_model();
        let open = |core: &mut Self, id: String| {
            if core.open(&SecretId(id)) {
                Ok(())
            } else {
                Err("That item is no longer in your vault".to_owned())
            }
        };
        match effect {
            PasswordsEffect::ShowAccess => self.navigate("app-access"),
            PasswordsEffect::RevokeAccess { grant } => self.revoke(&grant)?,
            PasswordsEffect::ApproveAccess { proposal, lifetime } => {
                let decision = match lifetime.as_str() {
                    "session" => accounts::AccessDecision::AllowForSession,
                    "persistent" => accounts::AccessDecision::AllowAlways,
                    _ => return Err(crate::CATALOG.t("passwords-request-expired")),
                };
                self.model
                    .as_ref()
                    .ok_or_else(|| crate::CATALOG.t("passwords-request-expired"))?
                    .decide_app_proposal(&proposal, decision)?;
                self.refresh();
            }
            PasswordsEffect::DenyAccess { proposal } => {
                self.model
                    .as_ref()
                    .ok_or_else(|| crate::CATALOG.t("passwords-request-expired"))?
                    .decide_app_proposal(&proposal, accounts::AccessDecision::Deny)?;
                self.refresh();
            }
            PasswordsEffect::Add => self.begin_add(),
            PasswordsEffect::Save => {
                if !self.is_editing() {
                    return Err("No item is being edited".into());
                }
                self.toast = None;
                self.save_draft();
                if self.is_editing() {
                    return Err(self
                        .draft_error
                        .as_ref()
                        .map(|(_, message)| message.clone())
                        .or_else(|| self.toast.clone())
                        .unwrap_or_else(|| "The item was not saved".to_owned()));
                }
            }
            PasswordsEffect::Cancel => self.cancel_edit(),
            PasswordsEffect::Generate => {
                if !self.is_editing() {
                    return Err("No item is being edited".into());
                }
                self.generate_password();
            }
            PasswordsEffect::Edit { item } => {
                open(self, item)?;
                self.begin_edit();
            }
            PasswordsEffect::Copy { item } => {
                open(self, item)?;
                self.copy_secret();
            }
            PasswordsEffect::Reveal { item } => {
                open(self, item)?;
                if self.revealed.is_none() {
                    self.reveal_secret();
                }
            }
            PasswordsEffect::Delete { item } => self.delete(&SecretId(item))?,
            PasswordsEffect::Favorite { item, favorite } => {
                self.set_favorite(&SecretId(item), favorite)?;
            }
            PasswordsEffect::Import => self.request_import(),
            PasswordsEffect::Export => self.request_export(),
            PasswordsEffect::Filter { filter } => {
                let parsed = VaultFilter::parse(&filter);
                if parsed.as_str() != filter {
                    return Err(format!("unknown filter `{filter}`"));
                }
                self.select_filter(parsed);
            }
            PasswordsEffect::Open { item } => open(self, item)?,
        }
        Ok(())
    }

    /// Live vault rows, one per item: what agents, scripts and the palette
    /// may do with it. A row names the item by its opaque id and kind only;
    /// its title, account and addresses stay in the vault.
    pub(crate) fn item_rows(&self) -> Vec<ui::framework::LiveRow> {
        self.items
            .iter()
            .map(crate::actions::item_row)
            .chain(self.app_grants.iter().map(crate::actions::grant_row))
            .chain(self.app_proposals.iter().map(crate::actions::proposal_row))
            .collect()
    }

    /// The live row of item `id` (a published row id).
    pub(crate) fn item_row(&self, id: &str) -> Option<ui::framework::LiveRow> {
        if let Some(id) = id.strip_prefix(crate::actions::PROPOSAL_PREFIX) {
            return self
                .app_proposals
                .iter()
                .find(|proposal| proposal.id == id)
                .map(crate::actions::proposal_row);
        }
        if let Some(id) = id.strip_prefix(crate::actions::GRANT_PREFIX) {
            return self
                .app_grants
                .iter()
                .find(|grant| grant.id() == id)
                .map(crate::actions::grant_row);
        }
        let id = id.strip_prefix(crate::actions::ITEM_PREFIX)?;
        self.items
            .iter()
            .find(|item| item.id.0 == id)
            .map(crate::actions::item_row)
    }
}

fn escape_custom_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn unescape_custom_value(value: &str) -> Result<String, String> {
    let mut output = String::new();
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        match chars.next() {
            Some('n') => output.push('\n'),
            Some('r') => output.push('\r'),
            Some('\\') => output.push('\\'),
            Some(other) => {
                output.push('\\');
                output.push(other);
            }
            None => return Err("A custom field value ends with an unfinished escape".into()),
        }
    }
    Ok(output)
}

pub(crate) fn parse_custom_fields(source: &str) -> Result<Vec<(String, String)>, String> {
    let mut fields = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((label, value)) = line.split_once('=').or_else(|| line.split_once(':')) else {
            return Err(format!(
                "Custom field {} needs a label and a value separated by =",
                index.saturating_add(1)
            ));
        };
        let label = label.trim();
        if label.is_empty() || label.eq_ignore_ascii_case("network") {
            return Err(format!(
                "Choose another label for custom field {}",
                index.saturating_add(1)
            ));
        }
        if fields
            .iter()
            .any(|(existing, _): &(String, String)| existing.eq_ignore_ascii_case(label))
        {
            return Err(format!("Custom field “{label}” appears more than once"));
        }
        fields.push((label.to_string(), unescape_custom_value(value.trim())?));
    }
    Ok(fields)
}
