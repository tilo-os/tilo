//! The Authenticator's content: the account list, its status surfaces and the
//! add sheet.
//!
//! Owns the encrypted device-local [`Vault`] (including base32 secrets) and
//! mirrors only account metadata into the shared Automerge document, so names,
//! order and lifecycle collaborate without credentials entering history. The
//! window's header row, with its search and add buttons, belongs to the
//! framework's navigation shell around this widget (see [`crate::resource`]);
//! this widget paints what stands under it:
//!
//!  * the [`AccountList`] (main screen), with a banner above it when the
//!    vault could not be saved,
//!  * a toast for what just happened,
//!  * the [`AddAccountPanel`] sheet (second screen), in the window's top layer.
//!
//! The list is painted from the root rect rather than through the taffy tree:
//! the codes and their countdown rings change every frame and the rows are
//! faces of the toolkit's list row, ring and button.

use std::collections::HashMap;
use ui::time::{Duration, Instant};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ChangeHash, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::LocalState;
use ui::a11y::{AccessNode, AccessState, Action as A11yAction, Politeness, Role};
use ui::paint::PaintCtx;
use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::OperationKind;
use ui::tokens::TextRole;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    Banner, BannerContent, BannerKind, Button, ButtonFace, ButtonRole, ControlSize, TaffyTreeExt,
    Widget,
};

use crate::add_panel::{AddAccountPanel, AddPanelEvent};
use crate::doc::AuthDoc;
use crate::list::{AccountList, ListEvent};
use crate::model::{AccountId, Vault, demo_accounts};
use crate::storage;
use crate::totp;

/// How long a toast stays on screen.
const TOAST_TTL: Duration = Duration::from_millis(1800);

fn qr_image_request() -> PickRequest {
    PickRequest::open("Choose a QR image", "Open")
        .thread_name("totp-portal-open")
        .filter(
            FileFilter::new("QR images")
                .mimetype("image/png")
                .mimetype("image/jpeg")
                .glob("*.png")
                .glob("*.jpg")
                .glob("*.jpeg"),
        )
        .all_files()
}

/// How the initial vault is populated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SeedMode {
    /// Live session: load the user's persisted vault, persist changes, and
    /// never fabricate sample data.
    Live,
    /// A capture or demo: seed a deterministic sample vault, ignore on-disk
    /// storage, and never write.
    Demo,
    /// A capture of the empty state: start with no accounts, ignore storage.
    Empty,
}

/// The banner's wording when the vault could not be saved: the headline, and
/// what the person can do about it.
const UNSAVED_TITLE: &str = "Changes aren\u{2019}t saved";
const UNSAVED_DETAIL: &str = "Restore keyring or storage access on this device, then retry.";

#[allow(clippy::struct_excessive_bools, reason = "independent UI/frame flags")]
pub struct AuthenticatorApp {
    vault: Vault,
    search_provider: crate::search_provider::TotpSearchProvider,
    list: AccountList,
    add_panel: AddAccountPanel,
    /// An in-flight XDG-portal file-open for QR-image import. When the user
    /// taps "Scan a QR image" the portal round-trip runs on a worker thread
    /// and hands the chosen path back over this channel, drained once per
    /// frame in [`Widget::sync`]. `None` when no picker is in flight — the app
    /// never draws its own picker (HIG §5).
    portal_rx: Option<PortalChannel>,
    /// The header's "Add account" button counts its presses here; the app
    /// opens the sheet whenever the count moves.
    add_requests: Option<LocalState<u64>>,
    seen_add_requests: u64,

    rect: Rect,
    /// The readable column the banner and rows stand in, from the last paint.
    column: Rect,
    banner_rect: Rect,
    retry_rect: Rect,
    root_node: Option<NodeId>,
    /// This tree's node of each of the open sheet's text fields, in tree order.
    field_nodes: Vec<NodeId>,
    /// The open sheet changed the layout tree (it opened or closed).
    structure_dirty: bool,

    last_mouse: Point,
    selected: Option<AccountId>,
    toast: Option<(String, Instant, ToastKind)>,
    /// The exact code string most recently written to the clipboard by a copy
    /// action. Lets callers (and tests) observe what was copied without
    /// round-tripping the system clipboard, which is unavailable headless.
    last_copied: Option<String>,
    seeded: bool,
    /// How the initial vault is populated and whether changes persist. The
    /// live app loads/saves the real vault; a capture uses fixed data and
    /// never touches storage.
    seed_mode: SeedMode,
    /// Local account metadata changed and must be mirrored into the CRDT
    /// document. Secrets are explicitly omitted by [`AuthDoc`].
    /// Gates the per-frame doc write so idle frames (and pointer moves) do
    /// no CRDT work.
    doc_dirty: bool,
    /// The visible (ordered) list must be recomputed — set when the vault
    /// changes.
    list_dirty: bool,
    /// Effective host motion preference. Normal mode uses a smooth countdown;
    /// reduced motion sleeps between whole-second updates.
    reduce_motion: bool,
    /// A local encrypted-vault write failed. We retain the in-memory vault but
    /// never retry in a frame loop; the user/agent explicitly invokes retry.
    persistence_pending: bool,
    vault_recovery_required: bool,
    /// Non-secret, human-readable durability detail shown in the dedicated
    /// persistence banner. This replaces a competing transient error toast.
    persistence_message: Option<String>,
    /// Framework-owned action dispatch. Each on-screen affordance dispatches its
    /// [`TotpAction`](crate::actions::TotpAction) through this: the host brackets
    /// the undo boundary, applies the action through the executor (so it replays
    /// byte-identically on peers, exactly as the Ctrl+K palette does), and records
    /// the last one so a test can prove a button and its
    /// [`ActionSpec`](tilo_actions::ActionSpec) build the identical action.
    actions: crate::resource::ActionSink,
}

impl Default for AuthenticatorApp {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthenticatorApp {
    /// The live app: loads the persisted vault, no sample data.
    pub fn new() -> Self {
        Self::with_mode(SeedMode::Live)
    }

    /// The demo app: seeds a deterministic sample vault and never touches
    /// on-disk storage.
    pub fn demo() -> Self {
        Self::with_mode(SeedMode::Demo)
    }

    /// The empty-state app: no accounts, ignores storage.
    pub fn empty() -> Self {
        Self::with_mode(SeedMode::Empty)
    }

    pub fn demo_provision() -> Self {
        let mut app = Self::with_mode(SeedMode::Demo);
        app.seed();
        app.seeded = true;
        if let Some(account) = app.vault.accounts.first_mut() {
            account.secret.clear();
        }
        app.seed_open_selected_setup();
        app
    }

    pub fn demo_persistence_error() -> Self {
        let mut app = Self::demo();
        app.seed();
        app.seeded = true;
        app.persistence_pending = true;
        app.persistence_message = Some(UNSAVED_DETAIL.into());
        app
    }

    pub fn demo_missing_secret() -> Self {
        let mut app = Self::demo();
        app.seed();
        app.seeded = true;
        if let Some(account) = app.vault.accounts.first_mut() {
            account.secret.clear();
        }
        app
    }

    fn with_mode(seed_mode: SeedMode) -> Self {
        crate::i18n::ensure();
        Self {
            vault: Vault::new(),
            search_provider: crate::search_provider::TotpSearchProvider::new(&Vault::new()),
            list: AccountList::new(),
            add_panel: AddAccountPanel::new(),
            portal_rx: None,
            add_requests: None,
            seen_add_requests: 0,
            rect: Rect::ZERO,
            column: Rect::ZERO,
            banner_rect: Rect::ZERO,
            retry_rect: Rect::ZERO,
            root_node: None,
            field_nodes: Vec::new(),
            structure_dirty: false,
            last_mouse: Point::ZERO,
            selected: None,
            toast: None,
            last_copied: None,
            seeded: false,
            seed_mode,
            doc_dirty: false,
            list_dirty: false,
            reduce_motion: false,
            persistence_pending: false,
            vault_recovery_required: false,
            persistence_message: None,
            actions: crate::resource::ActionSink::default(),
        }
    }

    /// Open the sheet whenever `requests` moves: the header button counts its
    /// presses there.
    #[must_use]
    pub fn with_add_requests(mut self, requests: LocalState<u64>) -> Self {
        self.seen_add_requests = requests.with(|count| *count);
        self.add_requests = Some(requests);
        self
    }

    /// Populate the initial vault. The live app loads the user's persisted
    /// vault (empty on a fresh install, which shows the empty state). A demo or
    /// a capture uses a fixed sample set and ignores on-disk storage entirely,
    /// so captures are reproducible and never leak the real vault.
    fn seed(&mut self) {
        // A capture pins the wall clock; it never reads or writes a real vault.
        if self.seed_mode == SeedMode::Live && ui::time::wall_frozen() {
            self.seed_mode = SeedMode::Demo;
        }
        let mut vault = match self.seed_mode {
            SeedMode::Live => match storage::load_checked() {
                Ok(vault) => vault,
                Err(error) => {
                    tracing::warn!(%error, "authenticator: existing vault could not be loaded");
                    self.vault_recovery_required = true;
                    self.describe_persistence_failure(
                        "The vault could not be loaded. Restore key or storage access, then restart Authenticator before saving.",
                    );
                    Vault::new()
                }
            },
            SeedMode::Demo => {
                let mut v = Vault::new();
                v.accounts = demo_accounts();
                v
            }
            SeedMode::Empty => Vault::new(),
        };
        vault.renumber();
        self.selected = vault.ordered().first().map(|a| a.id);
        self.search_provider.update(&self.vault, &vault);
        self.vault = vault;
        self.doc_dirty = true;
        self.list_dirty = true;
    }

    /// Dispatch one [`TotpAction`](crate::actions::TotpAction) — the single
    /// CRDT-mutation path shared with the Ctrl+K palette. The action is applied
    /// through the Tilo action executor (a pinned actor + committed change, so it
    /// replays byte-identically on peers), exactly as the palette does. The
    /// document heads captured just before the first action of the event are
    /// stashed so the host records **one** undo boundary bracketing it
    /// ([`CommittedActions::take_committed_action`](ui::undo::CommittedActions::take_committed_action)). Callers reload the in-memory vault
    /// from the document afterward for immediate feedback; `sync` reconciles it
    /// again each frame (so a palette dispatch shows up the same way). This app
    /// supplies only its domain: the action and its type.
    fn perform_action(&mut self, doc: &mut AutoCommit, action: &crate::actions::TotpAction) {
        self.actions.dispatch_with_context(
            doc,
            crate::actions::TOTP_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.totp", "Authenticator", action),
        );
    }

    /// Show shared account metadata, keeping secrets that exist only on this device.
    /// Accounts not yet in the document stay in the vault so the next sync can publish them.
    pub(crate) fn adopt_resource_state(&mut self, state: &[crate::model::Account]) {
        if state.is_empty() {
            return;
        }
        let mut next_accounts: Vec<_> = state
            .iter()
            .cloned()
            .map(|mut shared| {
                if let Some(local) = self.vault.get(shared.id) {
                    shared.secret.clone_from(&local.secret);
                }
                shared
            })
            .collect();
        for local in self.vault.ordered() {
            if state.iter().all(|shared| shared.id != local.id) {
                next_accounts.push(local);
            }
        }
        let next = crate::model::Vault {
            accounts: next_accounts,
        };
        if next.ordered() == self.vault.ordered() {
            return;
        }
        self.search_provider.update(&self.vault, &next);
        self.vault = next;
        self.list_dirty = true;
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Totp>,
    ) {
        self.actions.attach(dispatcher);
    }

    /// Merge shared metadata with this device's secret-bearing vault. A remote
    /// account can appear here without a secret (the row stays deliberately
    /// unavailable for copying until it is set up on this device); the document
    /// never has authority to replace or reveal local key material.
    fn reload_from_doc(&mut self, doc: &AutoCommit) -> bool {
        let metadata = AuthDoc::list_accounts(doc); // sorted by order, no secrets
        let next_accounts: Vec<_> = metadata
            .into_iter()
            .map(|mut shared| {
                if let Some(local) = self.vault.get(shared.id) {
                    shared.secret.clone_from(&local.secret);
                }
                shared
            })
            .collect();
        let next = Vault {
            accounts: next_accounts,
        };
        if next.ordered() == self.vault.ordered() {
            return true; // nothing changed
        }
        self.search_provider.update(&self.vault, &next);
        self.vault = next;
        self.list_dirty = true;
        self.persist_vault()
    }

    fn persist_vault(&mut self) -> bool {
        if self.vault_recovery_required {
            return false;
        }
        let persisted = self.seed_mode != SeedMode::Live || storage::save(&self.vault);
        self.persistence_pending = !persisted;
        if persisted {
            self.persistence_message = None;
        } else if self.persistence_message.is_none() {
            self.persistence_message = Some(UNSAVED_DETAIL.into());
        }
        persisted
    }

    fn describe_persistence_failure(&mut self, message: impl Into<String>) {
        self.persistence_pending = true;
        self.persistence_message = Some(message.into());
        // A durable, actionable banner supersedes the transient toast. Keeping
        // both is noisy and obscures the account list, especially on narrow
        // windows.
        self.toast = None;
    }

    fn retry_persist(&mut self) -> bool {
        if self.vault_recovery_required {
            return false;
        }
        if !self.persistence_pending {
            return true;
        }
        let persisted = self.persist_vault();
        if persisted {
            self.toast("Vault saved", ToastKind::Success);
        } else {
            self.describe_persistence_failure(UNSAVED_DETAIL);
        }
        persisted
    }

    /// Live vault-save retry is not in [`crate::resource::Totp`]'s children —
    /// publish it beside the framework tree when hosted.
    fn persistence_retry_resource(&self) -> Option<tilo_actions::Resource> {
        if !self.persistence_pending {
            return None;
        }
        Some(
            tilo_actions::Resource::new("totp.persistence", "totp.persistence", "Local vault save")
                .with_action(crate::actions::retry_persist_operation()),
        )
    }

    /// Recompute the ordered visible set and push it to the list.
    fn refresh_list(&mut self) {
        self.list.set_selected(self.selected);
        self.list.set_accounts(self.vault.ordered());
    }

    fn toast(&mut self, msg: impl Into<String>, kind: ToastKind) {
        self.toast = Some((msg.into(), ui::time::now(), kind));
    }

    fn copy_account(&mut self, id: AccountId) {
        if let Some(acc) = self.vault.get(id).cloned() {
            if !acc.secret_is_valid() {
                self.toast(
                    format!("No key for {} on this device", acc.title()),
                    ToastKind::Error,
                );
                return;
            }
            let code = totp::current_code(&acc);
            ui::text_selection::clipboard_copy(&code);
            self.last_copied = Some(code);
            self.toast(format!("Copied {} code", acc.title()), ToastKind::Success);
        }
    }

    /// The exact code string the most recent copy action placed on the
    /// clipboard, if any. Observability for the copy path (the system
    /// clipboard can't be read back in a headless test).
    pub fn last_copied(&self) -> Option<&str> {
        self.last_copied.as_deref()
    }

    /// The account currently selected, if any. Lets a test observe where a
    /// Ctrl+K activation navigated.
    pub fn selected_account(&self) -> Option<AccountId> {
        self.selected
    }

    pub(crate) fn has_local_secret(&self, id: AccountId) -> bool {
        self.vault
            .get(id)
            .is_some_and(crate::model::Account::secret_is_valid)
    }

    /// Show the add sheet.
    fn open_add(&mut self) {
        self.add_panel.open();
        self.structure_dirty = true;
    }

    /// Open the sheet for `id`: its details when the key is here, the key's
    /// setup when it is not.
    fn open_for(&mut self, id: AccountId) -> bool {
        let Some(account) = self.vault.get(id).cloned() else {
            return false;
        };
        self.selected = Some(id);
        if account.secret_is_valid() {
            self.add_panel.open_for_edit(&account);
        } else {
            self.add_panel.open_for_provision(&account);
        }
        self.structure_dirty = true;
        true
    }

    fn open_selected_for_edit(&mut self) -> bool {
        self.selected.is_some_and(|id| self.open_for(id))
    }

    /// Move the selected account through the same ordered list the pointer
    /// paints. This keeps keyboard selection in step with the list rather
    /// than introducing a second focus model.
    fn move_selection(&mut self, delta: isize) -> bool {
        let visible = self.vault.ordered();
        if visible.is_empty() {
            return false;
        }
        let current = self
            .selected
            .and_then(|id| visible.iter().position(|account| account.id == id))
            .unwrap_or(0);
        let next = (current as isize + delta).clamp(0, visible.len() as isize - 1) as usize;
        if let Some(account) = visible.get(next) {
            self.selected = Some(account.id);
            self.list.set_selected(self.selected);
            self.list.reveal(account.id);
        }
        true
    }

    fn accessible_account_name(account: &crate::model::Account) -> String {
        if account.label.trim().is_empty() {
            account.title().to_string()
        } else {
            format!("{}, {}", account.title(), account.label)
        }
    }

    /// Copy the selected account (or the first one) — the Enter shortcut.
    fn copy_focused(&mut self) {
        let target = self
            .selected
            .filter(|id| self.vault.get(*id).is_some())
            .or_else(|| self.vault.ordered().first().map(|a| a.id));
        if let Some(id) = target {
            self.copy_account(id);
        }
    }

    /// Apply one list interaction. Reorder and delete are document mutations, so
    /// they dispatch a [`TotpAction`](crate::actions::TotpAction) through
    /// [`Self::perform_action`] (the same action the Ctrl+K palette dispatches);
    /// copy and select are presentation and never touch the document.
    fn apply_list_event(&mut self, ev: ListEvent, state: &mut AutoCommit) {
        match ev {
            ListEvent::Copy(id) => {
                self.selected = Some(id);
                self.copy_account(id);
                self.list_dirty = true;
            }
            ListEvent::Select(id) => {
                self.selected = Some(id);
                self.list_dirty = true;
            }
            ListEvent::Edit(id) | ListEvent::SetUp(id) => {
                self.open_for(id);
            }
            ListEvent::Add => self.open_add(),
            ListEvent::MoveUp(id) => {
                self.perform_action(state, &crate::actions::move_up_action(id));
                self.reload_from_doc(state);
            }
            ListEvent::MoveDown(id) => {
                self.perform_action(state, &crate::actions::move_down_action(id));
                self.reload_from_doc(state);
            }
            ListEvent::Delete(id) => {
                self.perform_action(state, &crate::actions::delete_account_action(id));
                let persisted = self.reload_from_doc(state);
                if self.selected == Some(id) {
                    self.selected = self.vault.ordered().first().map(|a| a.id);
                }
                if persisted {
                    self.toast("Account removed", ToastKind::Success);
                } else {
                    self.describe_persistence_failure(
                        "The account was removed, but this device\u{2019}s vault wasn\u{2019}t updated. Restore access, then retry.",
                    );
                }
            }
        }
    }

    /// Act on one outcome drained from the add sheet. A save is a
    /// document mutation dispatched as an [`TotpAction::AddAccount`]
    /// (`crate::actions::TotpAction`); the colour slot and list order are pinned at
    /// authoring time so replay is deterministic.
    fn apply_add_panel_event(&mut self, ev: AddPanelEvent, state: &mut AutoCommit) {
        self.structure_dirty = true;
        match ev {
            AddPanelEvent::Cancel => {}
            AddPanelEvent::PickImageForQr => self.import_qr_from_image(),
            AddPanelEvent::Save(mut acc) => {
                // Give the new account a distinct colour slot + end-of-list order,
                // baked into the action so peers replay the identical row.
                acc.color = (self.vault.len() % 7) as u8;
                let order = self.vault.next_order();
                let id = acc.id;
                // The secret enters the encrypted local vault before the
                // metadata-only action is broadcast. It is never part of the
                // action envelope or shared document.
                acc.order = order;
                self.vault.upsert(acc.clone());
                let persisted = self.persist_vault();
                self.perform_action(state, &crate::actions::add_account_action(&acc, order));
                self.reload_from_doc(state);
                self.selected = Some(id);
                if persisted {
                    self.toast("Account added", ToastKind::Success);
                } else {
                    self.describe_persistence_failure(
                        "The account was added, but its key isn\u{2019}t saved. Keep this window open, restore access, then retry.",
                    );
                }
            }
            AddPanelEvent::Edit(mut acc) => {
                let order = self
                    .vault
                    .get(acc.id)
                    .map_or(acc.order, |existing| existing.order);
                acc.order = order;
                self.vault.upsert(acc.clone());
                let persisted = self.persist_vault();
                self.perform_action(state, &crate::actions::add_account_action(&acc, order));
                self.reload_from_doc(state);
                self.selected = Some(acc.id);
                if persisted {
                    self.toast("Account updated", ToastKind::Success);
                } else {
                    self.describe_persistence_failure(
                        "The account changed, but this device\u{2019}s vault isn\u{2019}t saved. Restore access, then retry.",
                    );
                }
            }
            AddPanelEvent::Provision(mut acc) => {
                let Some(existing) = self.vault.get(acc.id).cloned() else {
                    self.toast("That account is no longer available", ToastKind::Error);
                    self.list_dirty = true;
                    return;
                };
                // Keep shared identity/order/color immutable during key setup;
                // only the local secret is installed; TOTP parameters were
                // validated against the immutable shared metadata in the form.
                acc.issuer = existing.issuer;
                acc.label = existing.label;
                acc.color = existing.color;
                acc.order = existing.order;
                self.vault.upsert(acc);
                let persisted = self.persist_vault();
                self.list_dirty = true;
                if persisted {
                    self.toast("Key set up on this device", ToastKind::Success);
                } else {
                    self.describe_persistence_failure(
                        "The key works for this session, but this device\u{2019}s vault isn\u{2019}t saved. Restore access, then retry.",
                    );
                }
            }
        }
        self.list_dirty = true;
    }

    /// Resolve a published canvas target to the existing list/app action.
    /// Keeping this beside `apply_list_event` ensures assistive activation
    /// cannot acquire a separate mutation path from pointer input.
    fn perform_a11y_target(
        &mut self,
        target: &str,
        action: &A11yAction,
        state: &mut AutoCommit,
    ) -> bool {
        let Some((verb, raw_id)) = target.rsplit_once(':') else {
            return false;
        };
        let Ok(id) = raw_id.parse() else {
            return false;
        };
        if self.vault.get(id).is_none() {
            return false;
        }
        if matches!(action, A11yAction::Focus) {
            if verb == "totp.select" {
                self.apply_list_event(ListEvent::Select(id), state);
                self.list.set_selected(self.selected);
                return true;
            }
            return false;
        }
        if !matches!(action, A11yAction::Activate) {
            return false;
        }
        if verb == "totp.setup" {
            let Some(account) = self.vault.get(id) else {
                return false;
            };
            if account.secret_is_valid() {
                return false;
            }
            return self.open_for(id);
        }
        let event = match verb {
            "totp.select" => ListEvent::Select(id),
            "totp.copy" => ListEvent::Copy(id),
            "totp.edit" => ListEvent::Edit(id),
            "totp.move-up" => ListEvent::MoveUp(id),
            "totp.move-down" => ListEvent::MoveDown(id),
            "totp.delete" => ListEvent::Delete(id),
            _ => return false,
        };
        self.apply_list_event(event, state);
        self.list.set_selected(self.selected);
        true
    }

    /// Keyboard shortcuts available whenever the add sheet isn't open:
    /// Ctrl/Cmd+N (add), Ctrl/Cmd+E (edit the selected account), the arrow keys
    /// (move the selection) and Enter (copy the selected code). Searching
    /// belongs to the platform (Ctrl+K and `/`). Returns `true` when the event
    /// was fully consumed here.
    fn handle_global_shortcut(
        &mut self,
        event: &WindowEvent,
        mods: winit::keyboard::ModifiersState,
        handled: &mut bool,
    ) -> bool {
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };

        if let Key::Character(c) = logical_key {
            let lc = c.to_lowercase();
            let command = mods.control_key() || mods.super_key();
            if command && lc == "n" {
                self.open_add();
                *handled = true;
                return true;
            }
            if command && lc == "e" && self.open_selected_for_edit() {
                *handled = true;
                return true;
            }
        }
        match logical_key {
            Key::Named(NamedKey::ArrowDown) if self.move_selection(1) => {
                *handled = true;
                true
            }
            Key::Named(NamedKey::ArrowUp) if self.move_selection(-1) => {
                *handled = true;
                true
            }
            Key::Named(NamedKey::Enter) => {
                self.copy_focused();
                *handled = true;
                true
            }
            _ => false,
        }
    }

    /// Import a QR code from an image file into the open add sheet.
    ///
    /// Choosing the screenshot/photo goes through the **host** file chooser
    /// (the XDG Desktop Portal, [`ui::portal`]) — the app never draws its
    /// own picker (HIG §5). The portal round-trip runs on a worker thread and
    /// the chosen path is drained each frame and decoded by the pure-Rust,
    /// offline QR reader in [`crate::qr`]. A `TOTP_QR_IMAGE` environment
    /// variable short-circuits the chooser with a fixed path — the seam the
    /// headless captures and test flows drive.
    fn import_qr_from_image(&mut self) {
        if let Some(path) = std::env::var_os("TOTP_QR_IMAGE") {
            self.decode_qr_path(std::path::Path::new(&path));
            return;
        }
        self.request_qr_via_portal();
    }

    /// Ask the host desktop's file chooser for a QR image. Per HIG §5 the app
    /// never draws its own picker: the portal round-trip runs on a worker
    /// thread ([`ui::portal::PickRequest`]) and the chosen path comes back
    /// over a channel drained each frame in [`Self::drain_portal`]. A second
    /// request while one is in flight is ignored, so a double-tap can't stack
    /// two host dialogs.
    fn request_qr_via_portal(&mut self) {
        if self.portal_rx.is_some() {
            return;
        }
        self.portal_rx = Some(qr_image_request().send());
    }

    /// Drain a completed portal request, if any. Called once per frame from
    /// [`Widget::sync`]: decodes the chosen image into the open add sheet,
    /// surfaces a decode/cancel/failure via the toast, and clears the
    /// in-flight channel. Never blocks — a still pending picker leaves the
    /// channel installed for the next frame.
    fn drain_portal(&mut self) {
        let Some(rx) = self.portal_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                self.portal_rx = None;
                match outcome {
                    PortalOutcome::Opened(path) => self.decode_qr_path(&path),
                    // A cancel is a no-op — the sheet is left as it was.
                    PortalOutcome::Cancelled => {}
                    PortalOutcome::Failed(reason) => {
                        self.toast(
                            format!("Couldn\u{2019}t open the file chooser: {reason}"),
                            ToastKind::Error,
                        );
                    }
                }
            }
            // Still waiting on the host dialog, or the worker vanished.
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.portal_rx = None;
            }
        }
    }

    /// Decode a QR image at `path` into the open add sheet, or surface why
    /// it couldn't be read.
    fn decode_qr_path(&mut self, path: &std::path::Path) {
        match crate::qr::decode_image_file(path) {
            Ok(uri) => {
                self.add_panel.apply_uri(&uri);
                self.toast("QR code read", ToastKind::Success);
            }
            Err(reason) => self.toast(reason, ToastKind::Error),
        }
    }

    /// Open the add sheet straight away. Used by captures (and any caller that
    /// wants to land on the add flow).
    pub fn seed_open_add(&mut self) {
        self.open_add();
    }

    /// Open the add sheet with Advanced options expanded.
    pub fn seed_open_add_advanced(&mut self) {
        self.add_panel.open_with_advanced();
        self.structure_dirty = true;
    }

    pub(crate) fn seed_open_selected_setup(&mut self) {
        self.selected = self.vault.ordered().first().map(|account| account.id);
        let _ = self.open_selected_for_edit();
    }

    /// Stage a capture scenario: a deterministic demo vault, in the state the
    /// scenario shows. Never called in a live window.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        self.seed_mode = if scenario == Some("empty") {
            SeedMode::Empty
        } else {
            SeedMode::Demo
        };
        self.seed();
        self.seeded = true;
        self.vault_recovery_required = false;
        self.persistence_pending = false;
        self.persistence_message = None;
        self.toast = None;
        if matches!(scenario, Some("missing-secret" | "provision"))
            && let Some(account) = self.vault.accounts.first_mut()
        {
            account.secret.clear();
        }
        match scenario {
            Some("add") => self.seed_open_add(),
            Some("add-advanced") => self.seed_open_add_advanced(),
            Some("provision") => self.seed_open_selected_setup(),
            Some("persistence-error") => {
                self.persistence_pending = true;
                self.persistence_message = Some(UNSAVED_DETAIL.into());
            }
            _ => {}
        }
        self.refresh_list();
        // The states a pointer causes: a hovered row shows its actions, a
        // hovered code is where a click copies.
        let account_at = |index: usize| self.vault.ordered().get(index).map(|a| a.id);
        match scenario {
            Some("hover") => self.list.stage_hover(2, false),
            Some("copied") => {
                if let Some(id) = account_at(1) {
                    self.selected = Some(id);
                    self.list.set_selected(self.selected);
                    self.list.stage_hover(1, true);
                    self.copy_account(id);
                    // A capture is not on the toast's clock: it stays up.
                    if let Some((_, shown, _)) = &mut self.toast {
                        *shown = ui::time::now() + Duration::from_secs(60 * 60);
                    }
                }
            }
            _ => {}
        }
    }

    // ---- painting --------------------------------------------------------

    /// The readable strip the banner and rows stand in: the window less its
    /// gutters, no wider than a sheet carrying a form, and centred.
    fn readable_column(rect: Rect, theme: &Theme) -> Rect {
        let gutter = f64::from(theme.tokens.spacing.s6);
        let width = (rect.width() - 2.0 * gutter)
            .min(f64::from(theme.tokens.metrics.panel_width_sheet) + 2.0 * gutter)
            .max(0.0);
        let x0 = rect.x0 + (rect.width() - width) / 2.0;
        Rect::new(x0, rect.y0, x0 + width, rect.y1)
    }

    /// Place the banner and the list for this frame, from the theme's tokens.
    fn place(&mut self, ctx: &mut PaintCtx<'_>) {
        let theme = ctx.theme;
        let spacing = &theme.tokens.spacing;
        self.column = Self::readable_column(self.rect, theme);
        let mut top = self.rect.y0;
        self.banner_rect = Rect::ZERO;
        self.retry_rect = Rect::ZERO;
        if self.persistence_pending {
            let gap = f64::from(spacing.s3);
            let retry_h = ButtonFace::height(ControlSize::Regular, theme);
            let retry = ButtonFace::new(Rect::ZERO, RETRY_LABEL).role(ButtonRole::Primary);
            let retry_w = retry.intrinsic_width(ctx.text, theme);
            let banner_w = (self.column.width() - retry_w - gap).max(0.0);
            // The banner is as tall as its two lines need at this width: the
            // headline, then what the person can do about it.
            let metrics = &theme.tokens.metrics;
            let text_w =
                (banner_w - 2.0 * f64::from(spacing.s4) - f64::from(metrics.icon_md) - gap)
                    .max(1.0);
            let (_, headline_h) = ctx.text.measure_style(
                UNSAVED_TITLE,
                Some(ui::num::f32_of_f64(text_w)),
                &theme.text(TextRole::Body).strong(),
            );
            let (_, detail_h) = ctx.text.measure_style(
                self.persistence_message
                    .as_deref()
                    .unwrap_or(UNSAVED_DETAIL),
                Some(ui::num::f32_of_f64(text_w)),
                &theme.text(TextRole::Caption),
            );
            let banner_h =
                (f64::from(headline_h) + f64::from(spacing.s1) + f64::from(detail_h) + 2.0 * gap)
                    .max(f64::from(metrics.panel_row_height))
                    .max(retry_h);
            let y0 = top + f64::from(spacing.s2);
            self.banner_rect =
                Rect::new(self.column.x0, y0, self.column.x0 + banner_w, y0 + banner_h);
            self.retry_rect = Rect::new(
                self.column.x1 - retry_w,
                y0 + (banner_h - retry_h) / 2.0,
                self.column.x1,
                y0 + f64::midpoint(banner_h, retry_h),
            );
            top = y0 + banner_h + f64::from(spacing.s2);
        }
        let list_rect = Rect::new(self.rect.x0, top, self.rect.x1, self.rect.y1);
        self.list.set_viewport(list_rect, self.column);
    }

    fn paint_banner(&self, ctx: &mut PaintCtx<'_>) {
        if !self.persistence_pending {
            return;
        }
        Banner::paint(
            ctx,
            self.banner_rect,
            &BannerContent {
                kind: BannerKind::Error,
                icon: BannerKind::Error.default_icon(),
                title: UNSAVED_TITLE,
                message: Some(
                    self.persistence_message
                        .as_deref()
                        .unwrap_or(UNSAVED_DETAIL),
                ),
                dismissible: false,
            },
            self.last_mouse,
        );
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            &ButtonFace::new(self.retry_rect, RETRY_LABEL)
                .role(ButtonRole::Primary)
                .hovered(self.retry_rect.contains(self.last_mouse)),
        );
    }

    fn draw_toast(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some((msg, when, kind)) = &self.toast else {
            return;
        };
        if when.elapsed() > TOAST_TTL {
            return;
        }
        paint_transient(
            scene,
            transform,
            text_renderer,
            theme,
            self.rect,
            ToastAnchor::BottomCenter,
            &ToastCard::new(msg, *kind),
        );
    }
}

/// The banner's retry button.
const RETRY_LABEL: &str = "Retry";

impl AuthenticatorApp {
    fn answer_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        let (raw, expected_kind) = match operation {
            crate::actions::TotpOperation::OpenAddAccount => {
                if kind != OperationKind::View {
                    return Err("Authenticator add screen requires a view broker route".into());
                }
                self.open_add();
                return Ok(true);
            }
            crate::actions::TotpOperation::SetUpLocal { id } => {
                if kind != OperationKind::View {
                    return Err("Authenticator setup requires a view broker route".into());
                }
                let account_id = id
                    .parse()
                    .map_err(|_| "Invalid authenticator account id".to_string())?;
                if self.vault.get(account_id).is_none() {
                    return Err("Authenticator account is no longer available".into());
                }
                self.open_for(account_id);
                return Ok(true);
            }
            crate::actions::TotpOperation::RetryPersist => {
                if kind != OperationKind::Effect {
                    return Err("Authenticator retry requires an effect broker route".into());
                }
                self.retry_persist();
                return Ok(true);
            }
            crate::actions::TotpOperation::OpenAccount { id } => (id, OperationKind::View),
            crate::actions::TotpOperation::CopyCode { id } => (id, OperationKind::Effect),
        };
        if kind != expected_kind {
            return Err(format!(
                "Authenticator operation requires a {} broker route",
                expected_kind.as_str()
            ));
        }
        let id = raw
            .parse()
            .map_err(|_| "Invalid authenticator account id".to_string())?;
        if self.vault.get(id).is_none() {
            return Err("Authenticator account is no longer available".into());
        }
        self.selected = Some(id);
        self.list.reveal(id);
        self.list_dirty = true;
        if expected_kind == OperationKind::Effect {
            self.copy_account(id);
        }
        Ok(true)
    }

    /// Hand the sheet's outcome, if any, to the app.
    fn drain_add_panel(&mut self, state: &mut AutoCommit) {
        self.add_panel.pump();
        while let Some(event) = self.add_panel.take_event() {
            self.apply_add_panel_event(event, state);
        }
        if !self.add_panel.is_open() && self.add_panel.widget().is_some() {
            self.add_panel.close();
            self.structure_dirty = true;
        }
    }
}

impl Widget for AuthenticatorApp {
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::TOTP_OPERATION_TYPE]
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, _) = ui::doc::document(state, path);
        // Pull in any QR image the host portal picker returned since the last
        // frame (off-thread; never blocks the UI).
        self.drain_portal();

        // A capture stages its own vault ([`Self::stage_scenario`]); it never
        // seeds one first, so what the scenario shows is all the document holds.
        let capturing = self.seed_mode == SeedMode::Live && ui::time::wall_frozen();
        if !self.seeded && !capturing {
            self.seed();
            self.seeded = true;
        }
        // Publish the freshly-loaded vault once, on the seed frame. Each new
        // account is an `AddAccount`, hosted or standalone, so the first mirror
        // is the same replayable write as a later add. Accounts already in the
        // document are left alone: deleting one this device has not stored
        // would be a destructive collaboration bug.
        if self.doc_dirty {
            let existing: std::collections::HashSet<_> = AuthDoc::list_accounts(state)
                .into_iter()
                .map(|account| account.id)
                .collect();
            for account in self.vault.ordered() {
                if existing.contains(&account.id) {
                    continue;
                }
                self.perform_action(
                    state,
                    &crate::actions::add_account_action(&account, account.order),
                );
            }
            self.doc_dirty = false;
        }
        // Re-project the vault from the document so palette dispatches and remote
        // edits appear (a cheap no-op when nothing changed).
        self.reload_from_doc(state);
        // Sheets that left the tree last frame can go now.
        if self.add_panel.reap() {
            self.structure_dirty = true;
        }
        // The header's add button asked for the sheet.
        if let Some(requests) = &self.add_requests {
            let count = requests.with(|count| *count);
            if count != self.seen_add_requests {
                self.seen_add_requests = count;
                if !self.add_panel.is_open() {
                    self.open_add();
                }
            }
        }
        self.drain_add_panel(state);
        if let Some(sheet) = self.add_panel.widget_mut() {
            sheet.sync(state, path);
        }
        // Recompute the visible set only when the vault changed.
        if self.list_dirty {
            self.refresh_list();
            self.list_dirty = false;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The sheet draws in the window's top layer, but its fields join the
        // window's tree here: an absolute layer over the list.
        let mut layers = Vec::new();
        if let Some(sheet) = self.add_panel.widget_mut() {
            let layer = sheet.build_layout(taffy, theme, node_map);
            let mut style = taffy.style(layer).cloned().unwrap_or_default();
            style.position = taffy::Position::Absolute;
            let _ = taffy.set_style(layer, style);
            layers.push(layer);
        }
        // The sheet lays its content out apart from this tree too; keep this
        // tree's node of each text field to answer a click's focus request.
        self.field_nodes = self.add_panel.host_field_nodes(node_map);
        let node = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                flex_grow: 1.0,
                ..Default::default()
            },
            &layers,
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.root_node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let x0 = origin.x + f64::from(layout.location.x);
        let y0 = origin.y + f64::from(layout.location.y);
        self.rect = Rect::new(
            x0,
            y0,
            x0 + f64::from(layout.size.width),
            y0 + f64::from(layout.size.height),
        );
        let layers = taffy.children(node).unwrap_or_default();
        if let (Some(sheet), Some(layer)) = (self.add_panel.widget_mut(), layers.first()) {
            sheet.apply_layout(taffy, *layer, Point::new(x0, y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // The window ground is the framework's; this paints what stands on it.
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        self.place(&mut ctx);
        self.paint_banner(&mut ctx);
        self.list.draw(&mut ctx);
        self.draw_toast(scene, transform, text_renderer, theme);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        // Root rect is the whole pane — always paint.
        self.draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }

        // The sheet owns all input while it is open.
        if self.add_panel.is_open() {
            let path = ctx.path;
            let node = ctx.node_id;
            if matches!(event, WindowEvent::MouseWheel { .. }) {
                self.add_panel
                    .handle_wheel(event, &mut ctx.reborrow(path, node));
            } else if let Some(sheet) = self.add_panel.widget_mut() {
                sheet.handle_event(event, &mut ctx.reborrow(path, node));
            }
            // A press on a text field puts the caret in it: the window owns
            // focus, so it is asked for the field's node in its own tree.
            if matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            ) && let Some(field) = self.add_panel.field_under(self.last_mouse)
                && let Some(node) = self.field_nodes.get(field)
            {
                *ctx.focus_target = Some(*node);
            }
            self.drain_add_panel(ctx.state);
            *ctx.handled = true;
            return;
        }

        if self.handle_global_shortcut(event, ctx.modifiers.state(), ctx.handled) {
            return;
        }

        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) && self.retry_rect.contains(self.last_mouse)
            && self.retry_persist()
        {
            *ctx.handled = true;
            return;
        }

        // Route to the list (hover / click-to-copy / reorder / scroll).
        self.list
            .handle_event(event, ctx.scale_factor, &mut *ctx.handled);
        let events = self.list.take_events();
        for ev in events {
            self.apply_list_event(ev, ctx.state);
        }
        // Keep the list's selection mirror current.
        self.list.set_selected(self.selected);
        if self.list_dirty {
            self.refresh_list();
            self.list_dirty = false;
        }
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn name(&self) -> &'static str {
        "AuthenticatorApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        let sheet = self.add_panel.take_structure_dirty();
        std::mem::take(&mut self.structure_dirty) | sheet
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.add_panel.widget().into_iter().collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if let Some(sheet) = self.add_panel.widget() {
            visitor(sheet);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.add_panel.widget_mut().into_iter().collect()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the root publishes one coherent semantic tree"
    )]
    fn accessibility(&self) -> Option<AccessNode> {
        // While the sheet is open it owns the window: what is behind it is
        // not reachable.
        if self.add_panel.is_open() {
            return Some(
                AccessNode::new(Role::Group)
                    .with_name("Authenticator")
                    .with_bounds(self.rect)
                    .with_state(AccessState {
                        hidden: true,
                        ..AccessState::default()
                    }),
            );
        }
        let visible = self.vault.ordered();
        let total = visible.len();
        let mut children = Vec::new();
        if self.persistence_pending {
            children.push(
                AccessNode::new(Role::Toolbar)
                    .with_name("Vault status")
                    .with_children(vec![
                        AccessNode::new(Role::Button)
                            .with_name("Retry saving local vault")
                            .with_description(
                                self.persistence_message
                                    .as_deref()
                                    .unwrap_or("Retry the failed encrypted local-vault save"),
                            )
                            .with_state(AccessState::new().focusable(true))
                            .with_bounds(self.retry_rect)
                            .with_action_target("totp.retry-persist"),
                    ]),
            );
        }

        let account_rows = visible
            .iter()
            .enumerate()
            .filter_map(|(index, account)| {
                let row = self.list.account_bounds(account.id)?;
                let name = Self::accessible_account_name(account);
                let id = account.id;
                let mut row_children = vec![
                    AccessNode::new(Role::Button)
                        .with_name(format!("Copy code for {name}"))
                        .with_description(if account.secret_is_valid() {
                            "Copies the current one-time code to the clipboard"
                        } else {
                            "Set up this account's key on this device before copying a code"
                        })
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .disabled(!account.secret_is_valid()),
                        )
                        .with_action_target(format!("totp.copy:{id}")),
                    AccessNode::new(Role::Button)
                        .with_name(format!("Edit {name}"))
                        .with_state(AccessState::new().focusable(true))
                        .with_action_target(format!("totp.edit:{id}")),
                    AccessNode::new(Role::Button)
                        .with_name(format!("Move {name} up"))
                        .with_state(AccessState::new().focusable(true).disabled(index == 0))
                        .with_action_target(format!("totp.move-up:{id}")),
                    AccessNode::new(Role::Button)
                        .with_name(format!("Move {name} down"))
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .disabled(index + 1 == total),
                        )
                        .with_action_target(format!("totp.move-down:{id}")),
                    AccessNode::new(Role::Button)
                        .with_name(format!("Delete {name}"))
                        .with_state(AccessState::new().focusable(true))
                        .with_action_target(format!("totp.delete:{id}")),
                ];
                if !account.secret_is_valid() {
                    row_children.push(
                        AccessNode::new(Role::Button)
                            .with_name(format!("Set up {name} on this device"))
                            .with_description(
                                "Paste or scan the key without sharing it with collaborators",
                            )
                            .with_state(AccessState::new().focusable(true))
                            .with_action_target(format!("totp.setup:{id}")),
                    );
                }
                // The rotating code is a credential. It is deliberately not
                // exported as a value/live region; callers opt in by using
                // the explicit Copy control.
                Some(
                    AccessNode::new(Role::ListItem)
                        .with_name(name)
                        .with_description("Authenticator account")
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .selected(self.selected == Some(id)),
                        )
                        .with_position(index + 1, total)
                        .with_bounds(row)
                        .with_action_target(format!("totp.select:{id}"))
                        .with_children(row_children),
                )
            })
            .collect();
        children.push(
            AccessNode::new(Role::List)
                .with_name("Authenticator accounts")
                .with_description(if total == 0 {
                    "No accounts".to_string()
                } else {
                    format!("{total} accounts. Arrow keys change the selected account.")
                })
                .with_bounds(self.rect)
                .with_children(account_rows),
        );
        if let Some((message, when, _)) = &self.toast
            && when.elapsed() <= TOAST_TTL
        {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(message)
                    .with_live(Politeness::Polite),
            );
        }
        Some(
            AccessNode::new(Role::Group)
                .with_name("Authenticator")
                .with_bounds(self.rect)
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &A11yAction,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, _) = ui::doc::document(state, path);
        if target == "totp.retry-persist" {
            return matches!(action, A11yAction::Activate) && self.retry_persist();
        }
        self.perform_a11y_target(target, action, state)
    }

    /// Smooth countdown rings tick every frame. Reduced motion is driven by a
    /// one-second deadline instead, so the app remains current without a vsync
    /// loop.
    fn needs_animation_frame(&self) -> bool {
        !self.reduce_motion
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        if !self.reduce_motion {
            return None;
        }
        let now = ui::time::wall()
            .duration_since(ui::time::UNIX_EPOCH)
            .unwrap_or_default();
        let until_next_second =
            Duration::from_nanos(1_000_000_000_u64.saturating_sub(u64::from(now.subsec_nanos())));
        Some(ui::time::now() + until_next_second)
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.reduce_motion = reduced;
    }
}

impl ui::search::capability::SearchSource for AuthenticatorApp {
    /// Fold the vault into the unified Ctrl+K palette through an O(1) handle to
    /// the retained
    /// [`TotpSearchProvider`](crate::search_provider::TotpSearchProvider), which
    /// makes every account findable there by issuer or account name alongside
    /// the actions the app exports.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_provider.clone())]
    }
}

impl ui::resources::PublishesResources for AuthenticatorApp {
    /// Publish the open vault + its accounts so the host's Ctrl+K palette scopes
    /// to them. Delegates to the
    /// [`ResourceProvider`](tilo_actions::ResourceProvider) impl in
    /// [`crate::actions`], which reads the accounts straight from the document.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        // The framework root publishes the vault and its accounts. Keep only
        // the live persistence retry when hosted — it is not in the resource.
        if self.actions.is_hosted() {
            return self.persistence_retry_resource().into_iter().collect();
        }
        let accounts =
            <crate::resource::Totp as ui::tilo_resource::Resource>::read(doc).unwrap_or_default();
        let mut root = ui::tilo_resource::publish::<crate::resource::Totp>(
            "totp.vault",
            "Accounts",
            &accounts,
        );
        let ordered = self.vault.ordered();
        for child in &mut root.children {
            if child.kind != "totp.account" {
                continue;
            }
            let Some(raw) = child.id.as_str().strip_prefix("totp.account:") else {
                continue;
            };
            let ready = raw.parse::<crate::model::AccountId>().is_ok_and(|id| {
                ordered
                    .iter()
                    .find(|account| account.id == id)
                    .is_some_and(crate::model::Account::secret_is_valid)
            });
            crate::actions::overlay_local_secret(child, ready);
        }
        let published: std::collections::HashSet<String> = root
            .children
            .iter()
            .map(|child| child.id.as_str().to_owned())
            .collect();
        let count = ordered.len();
        for (index, account) in ordered.iter().enumerate() {
            let id = crate::actions::account_resource_id(account.id);
            if published.contains(id.as_str()) {
                continue;
            }
            root.children.push(crate::actions::provide_account_resource(
                account,
                index,
                count,
                account.secret_is_valid(),
            ));
        }
        if self.persistence_pending {
            root = root.with_action(crate::actions::retry_persist_operation());
        }
        vec![root]
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if id == "totp.persistence" {
            return self.persistence_retry_resource();
        }
        if id == "totp.vault" {
            // When hosted the framework root owns totp.vault; this surface
            // only adds the persistence retry beside it.
            if self.actions.is_hosted() {
                return None;
            }
            return self.resources(doc).pop();
        }
        let raw_id = id.strip_prefix("totp.account:")?;
        if self.actions.is_hosted() {
            let accounts = <crate::resource::Totp as ui::tilo_resource::Resource>::read(doc)
                .unwrap_or_default();
            let root = ui::tilo_resource::publish::<crate::resource::Totp>(
                "totp.vault",
                "Accounts",
                &accounts,
            );
            if let Some(mut child) = root
                .children
                .into_iter()
                .find(|child| child.id.as_str() == id)
            {
                let ready = raw_id
                    .parse::<crate::model::AccountId>()
                    .is_ok_and(|account_id| {
                        self.vault
                            .ordered()
                            .iter()
                            .find(|account| account.id == account_id)
                            .is_some_and(crate::model::Account::secret_is_valid)
                    });
                crate::actions::overlay_local_secret(&mut child, ready);
                return Some(child);
            }
        } else if let Some(found) = self.resources(doc).into_iter().find_map(|root| {
            root.children
                .into_iter()
                .find(|child| child.id.as_str() == id)
        }) {
            return Some(found);
        }
        let account_id = uuid::Uuid::parse_str(raw_id).ok()?;
        let ordered = self.vault.ordered();
        let index = ordered
            .iter()
            .position(|account| account.id == account_id)?;
        ordered.get(index).map(|account| {
            crate::actions::provide_account_resource(
                account,
                index,
                ordered.len(),
                account.secret_is_valid(),
            )
        })
    }
}

impl ui::undo::CommittedActions for AuthenticatorApp {
    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its account action, so it records one undo boundary for it (see
    /// `Self::perform_action`). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.actions.take_committed()
    }
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;
