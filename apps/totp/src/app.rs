//! The Authenticator's surface over the [`Totp`] resource: the account list,
//! its status surfaces and the add sheet.
//!
//! The document holds account metadata (names, order and lifecycle
//! collaborate without credentials entering history); this device's secret
//! store holds each account's seed ([`Seeds`]). The surface joins the two into
//! the accounts it shows and submits every metadata change as a [`TotpAction`]
//! through the framework's dispatcher. The window's header row, with its
//! search and add buttons, belongs to the framework's navigation shell around
//! this widget (see [`crate::resource`]); this widget paints what stands under
//! it:
//!
//!  * the [`AccountList`] (main screen), with a banner above it when a
//!    secret could not be saved on this device,
//!  * a toast for what just happened,
//!  * the [`AddAccountPanel`] sheet (second screen), in the window's top layer.
//!
//! The list is painted from the root rect rather than through the taffy tree:
//! the codes and their countdown rings change every frame and the rows are
//! faces of the toolkit's list row, ring and button.

use std::collections::HashMap;
use std::rc::Rc;
use ui::time::{Duration, Instant};
use ui::widget::{AppModel, ModelPath};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::a11y::{AccessNode, AccessState, Action as A11yAction, Politeness, Role};
use ui::framework::Dispatcher;
use ui::paint::PaintCtx;
use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::secrets::SecretStore;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    Banner, BannerContent, BannerKind, Button, ButtonFace, ButtonRole, ControlSize, TaffyTreeExt,
    Widget,
};

use crate::actions::TotpAction;
use crate::add_panel::{AddAccountPanel, AddPanelEvent};
use crate::list::{AccountList, ListEvent};
use crate::model::{Account, AccountId, Vault};
use crate::resource::{Totp, TotpEffect};
use crate::secrets::Seeds;
use crate::totp;

/// Seconds into a thirty-second period at which a capture shows its codes: the
/// rings are about two thirds full.
const CAPTURE_ELAPSED: u64 = 11;
/// Seconds into the period at which the expiring capture shows them: the last
/// few, when the rings turn to their warning tone.
const CAPTURE_EXPIRING_ELAPSED: u64 = 27;

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

/// The banner's wording when a secret could not be saved on this device: the
/// headline, and what the person can do about it.
const UNSAVED_TITLE: &str = "Changes aren\u{2019}t saved";
const UNSAVED_DETAIL: &str = "Restore keyring or storage access on this device, then retry.";

#[allow(clippy::struct_excessive_bools, reason = "independent UI/frame flags")]
pub struct AuthenticatorApp {
    /// The accounts as the document holds them: metadata, no secrets.
    accounts: Rc<Vec<Account>>,
    /// Each account's seed on this device.
    seeds: Seeds,
    /// The accounts joined with this device's seeds: what the list shows and
    /// codes are computed from.
    vault: Vault,
    list: AccountList,
    add_panel: AddAccountPanel,
    /// An in-flight XDG-portal file-open for QR-image import. When the user
    /// taps "Scan a QR image" the portal round-trip runs on a worker thread
    /// and hands the chosen path back over this channel, drained once per
    /// frame in [`Widget::sync`]. `None` when no picker is in flight — the app
    /// never draws its own picker (HIG §5).
    portal_rx: Option<PortalChannel>,

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
    /// An account this view asked to delete, confirmed once it leaves the
    /// document.
    pending_delete: Option<AccountId>,
    /// The visible (ordered) list must be recomputed — set when the vault
    /// changes.
    list_dirty: bool,
    /// Effective host motion preference. Normal mode uses a smooth countdown;
    /// reduced motion sleeps between whole-second updates.
    reduce_motion: bool,
    /// A secret this device's store refused or could not read: shown as a
    /// banner until a retry the user asks for succeeds, never retried in a
    /// frame loop.
    persistence_message: Option<String>,
    /// Where account metadata changes go.
    dispatch: Option<Dispatcher<Totp>>,
    /// Whether the list has accounts, for the window's header row to follow.
    has_accounts: Option<ui::LocalState<bool>>,
    /// A capture's clock was moved to its scenario's step already.
    clock_staged: bool,
}

impl Default for AuthenticatorApp {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthenticatorApp {
    /// The surface before the framework attaches it to its resource and this
    /// device's secret store.
    pub fn new() -> Self {
        crate::i18n::ensure();
        Self {
            accounts: Rc::default(),
            seeds: Seeds::new(SecretStore::memory("so.tilo.totp"), false),
            vault: Vault::new(),
            list: AccountList::new(),
            add_panel: AddAccountPanel::new(),
            portal_rx: None,
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
            pending_delete: None,
            list_dirty: false,
            reduce_motion: false,
            persistence_message: None,
            dispatch: None,
            has_accounts: None,
            clock_staged: false,
        }
    }

    /// Report through `signal` whether the list has accounts, so the header
    /// row's actions can step aside for the empty state.
    #[must_use]
    pub(crate) fn with_account_signal(mut self, signal: ui::LocalState<bool>) -> Self {
        self.has_accounts = Some(signal);
        self
    }

    /// Edit through `dispatch`, with the seeds in `store`. A view that is not
    /// `live` (a past version, a screenshot) never writes or forgets one.
    pub(crate) fn attach_framework(
        &mut self,
        dispatch: Dispatcher<Totp>,
        store: SecretStore,
        live: bool,
    ) {
        self.dispatch = Some(dispatch);
        self.seeds = Seeds::new(store, live);
    }

    /// Submit one account change through the framework: checked, applied,
    /// undoable and replicated like the same action from the palette.
    fn apply(&self, action: &TotpAction) {
        if let Some(dispatch) = &self.dispatch {
            dispatch.apply(action);
        }
    }

    /// Show the accounts the document holds now, with this device's seeds.
    pub(crate) fn present_accounts(&mut self, state: &Rc<Vec<Account>>) {
        self.accounts = Rc::clone(state);
        let ids: Vec<AccountId> = state.iter().map(|account| account.id).collect();
        self.seeds.follow(&ids);
        if self.persistence_message.is_none() {
            if self.seeds.unreadable() {
                self.describe_persistence_failure(
                    "This device\u{2019}s keys couldn\u{2019}t be read. Unlock the keyring or restore access, then retry.",
                );
            } else if self.seeds.pending() {
                self.describe_persistence_failure(UNSAVED_DETAIL);
            }
        }
        self.join_seeds();
        if let Some(id) = self.pending_delete
            && !ids.contains(&id)
        {
            self.pending_delete = None;
            self.toast("Account removed", ToastKind::Success);
        }
        if self.selected.is_none_or(|id| !ids.contains(&id)) {
            self.selected = self.vault.ordered().first().map(|account| account.id);
        }
    }

    /// Join the document's accounts with this device's seeds.
    fn join_seeds(&mut self) {
        let accounts = self
            .accounts
            .iter()
            .cloned()
            .map(|mut account| {
                if let Some(seed) = self.seeds.get(account.id) {
                    seed.clone_into(&mut account.secret);
                }
                account
            })
            .collect();
        self.vault = Vault { accounts };
        self.list_dirty = true;
    }

    /// The accounts as the document holds them, without secrets.
    pub(crate) fn accounts(&self) -> &[Account] {
        &self.accounts
    }

    /// Whether a secret change is not saved on this device.
    pub(crate) fn persistence_pending(&self) -> bool {
        self.persistence_message.is_some()
    }

    fn describe_persistence_failure(&mut self, message: impl Into<String>) {
        self.persistence_message = Some(message.into());
        // A durable, actionable banner supersedes the transient toast. Keeping
        // both is noisy and obscures the account list, especially on narrow
        // windows.
        self.toast = None;
    }

    /// Try this device's secret store again, as the person asked.
    fn retry_persist(&mut self) -> bool {
        if !self.persistence_pending() {
            return true;
        }
        let ids: Vec<AccountId> = self.accounts.iter().map(|account| account.id).collect();
        if self.seeds.retry(&ids) {
            self.persistence_message = None;
            self.join_seeds();
            self.toast("Keys saved on this device", ToastKind::Success);
            true
        } else {
            self.describe_persistence_failure(UNSAVED_DETAIL);
            false
        }
    }

    /// Keep `seed` as the secret of `id` on this device, reporting a store
    /// that refused it with `failure`.
    fn keep_seed(&mut self, id: AccountId, seed: &str, failure: &str) -> bool {
        let saved = self.seeds.put(id, seed);
        if !saved {
            self.describe_persistence_failure(failure);
        }
        saved
    }

    /// Recompute the ordered visible set and push it to the list.
    fn refresh_list(&mut self) {
        self.list.set_selected(self.selected);
        self.list.set_accounts(self.vault.ordered());
        if let Some(signal) = &self.has_accounts {
            signal.set(!self.list.is_empty());
        }
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

    /// Apply one list interaction. Reorder and delete are document mutations,
    /// submitted as the same [`TotpAction`] the published account actions
    /// build; copy and select are presentation and never touch the document.
    fn apply_list_event(&mut self, ev: ListEvent) {
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
            ListEvent::MoveUp(id) => self.apply(&crate::actions::move_up_action(id)),
            ListEvent::MoveDown(id) => self.apply(&crate::actions::move_down_action(id)),
            ListEvent::Delete(id) => {
                // Deleting asks first; the account (and with it this device's
                // key) goes once the document drops it.
                self.pending_delete = Some(id);
                self.apply(&crate::actions::delete_account_action(id));
            }
        }
    }

    /// Act on one outcome drained from the add sheet. The key goes to this
    /// device's store; only metadata goes to the document, with the colour
    /// slot and list order pinned at authoring time so replay is
    /// deterministic.
    fn apply_add_panel_event(&mut self, ev: AddPanelEvent) {
        self.structure_dirty = true;
        match ev {
            AddPanelEvent::Cancel => {}
            AddPanelEvent::PickImageForQr => self.import_qr_from_image(),
            AddPanelEvent::Save(mut acc) => {
                acc.color = (self.vault.len() % 7) as u8;
                let order = self.vault.next_order();
                acc.order = order;
                let id = acc.id;
                let saved = self.keep_seed(
                    id,
                    &acc.secret,
                    "The account was added, but its key isn\u{2019}t saved. Keep this window open, restore access, then retry.",
                );
                self.apply(&crate::actions::add_account_action(&acc, order));
                self.selected = Some(id);
                if saved {
                    self.toast("Account added", ToastKind::Success);
                }
            }
            AddPanelEvent::Edit(mut acc) => {
                let Some(before) = self.vault.get(acc.id).cloned() else {
                    self.toast("That account is no longer available", ToastKind::Error);
                    self.list_dirty = true;
                    return;
                };
                // The form edits identity, key and code parameters; the list
                // owns colour and order.
                acc.color = before.color;
                acc.order = before.order;
                let saved = acc.secret == before.secret
                    || self.keep_seed(
                        acc.id,
                        &acc.secret,
                        "The account changed, but its new key isn\u{2019}t saved on this device. Restore access, then retry.",
                    );
                let edits = crate::actions::edit_actions(&before, &acc);
                if let Some(dispatch) = &self.dispatch {
                    dispatch.group(|| {
                        for edit in &edits {
                            dispatch.apply(edit);
                        }
                    });
                }
                self.join_seeds();
                self.selected = Some(acc.id);
                if saved {
                    self.toast("Account updated", ToastKind::Success);
                }
            }
            AddPanelEvent::Provision(acc) => {
                if self.vault.get(acc.id).is_none() {
                    self.toast("That account is no longer available", ToastKind::Error);
                    self.list_dirty = true;
                    return;
                }
                // Only the local key is installed; the shared identity, order
                // and colour stay as the document has them, and the form
                // checked the code parameters against them.
                let saved = self.keep_seed(
                    acc.id,
                    &acc.secret,
                    "The key works for this session, but it isn\u{2019}t saved on this device. Restore access, then retry.",
                );
                self.join_seeds();
                if saved {
                    self.toast("Key set up on this device", ToastKind::Success);
                }
            }
        }
        self.list_dirty = true;
    }

    /// Resolve a published canvas target to the existing list/app action.
    /// Keeping this beside `apply_list_event` ensures assistive activation
    /// cannot acquire a separate mutation path from pointer input.
    fn perform_a11y_target(&mut self, target: &str, action: &A11yAction) -> bool {
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
                self.apply_list_event(ListEvent::Select(id));
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
        self.apply_list_event(event);
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

    /// Move a capture's pinned clock into the step of the codes' period the
    /// scenario shows, so the countdown rings are part-way round (and, for
    /// the expiring scenario, in their last seconds). The capture pins the
    /// clock to the top of a minute, where every thirty-second period has just
    /// begun; a live window and a test run on the real clock and are left alone.
    fn stage_clock(&mut self, scenario: Option<&str>) {
        if !ui::time::wall_frozen() || self.clock_staged {
            return;
        }
        self.clock_staged = true;
        let elapsed = if scenario == Some("expiring") {
            CAPTURE_EXPIRING_ELAPSED
        } else {
            CAPTURE_ELAPSED
        };
        if let Some(at) = ui::time::wall().checked_add(Duration::from_secs(elapsed)) {
            ui::time::freeze_wall(at);
        }
    }

    /// Stage the view state a capture scenario shows. The scenario's accounts
    /// reached the document through its actions, and the capture's own store
    /// holds their keys (all but the first for "provision" and
    /// "missing-secret"). Never called in a live window.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        self.toast = None;
        self.stage_clock(scenario);
        match scenario {
            Some("add") => self.seed_open_add(),
            Some("add-advanced") => self.seed_open_add_advanced(),
            Some("provision") => self.seed_open_selected_setup(),
            Some("persistence-error") => self.describe_persistence_failure(UNSAVED_DETAIL),
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

    /// Answer an effect invoked by a control, the palette, an agent or a
    /// script.
    pub(crate) fn perform_effect(&mut self, effect: TotpEffect) -> Result<(), String> {
        match effect {
            TotpEffect::AddAccount => self.open_add(),
            TotpEffect::Open { account } => {
                let id = self.known_account(&account)?;
                self.reveal(id);
            }
            TotpEffect::CopyCode { account } => {
                let id = self.known_account(&account)?;
                self.reveal(id);
                if !self.has_local_secret(id) {
                    return Err("Set up this account on this device before copying a code".into());
                }
                self.copy_account(id);
            }
            TotpEffect::SetUpLocal { account } => {
                let id = self.known_account(&account)?;
                self.open_for(id);
            }
            TotpEffect::RetrySave => {
                if !self.retry_persist() {
                    return Err(self.persistence_message.clone().unwrap_or_default());
                }
            }
        }
        Ok(())
    }

    fn known_account(&self, raw: &str) -> Result<AccountId, String> {
        let id = raw
            .parse()
            .map_err(|_| "Invalid authenticator account id".to_string())?;
        if self.vault.get(id).is_none() {
            return Err("Authenticator account is no longer available".into());
        }
        Ok(id)
    }

    /// Select `id` and scroll its row into view.
    fn reveal(&mut self, id: AccountId) {
        self.selected = Some(id);
        self.list.reveal(id);
        self.list_dirty = true;
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
        if self.persistence_pending() {
            let metrics = &theme.tokens.metrics;
            let pad = f64::from(spacing.s4);
            let gap = f64::from(spacing.s3);
            let detail = self
                .persistence_message
                .as_deref()
                .unwrap_or(UNSAVED_DETAIL);
            let headline_style = theme.text(TextRole::Body).strong();
            let detail_style = theme.text(TextRole::Caption);
            let retry_h = ButtonFace::height(ControlSize::Regular, theme);
            let retry_w = ButtonFace::new(Rect::ZERO, RETRY_LABEL)
                .role(ButtonRole::Secondary)
                .intrinsic_width(ctx.text, theme);
            // The banner spans the column the rows stand in. Its text starts
            // after the icon; Retry sits inside the box at its trailing edge
            // when both lines fit beside it, and under the box otherwise.
            let text_x = pad + f64::from(metrics.icon_md) + gap;
            let (headline_w, _) = ctx.text.measure_style(UNSAVED_TITLE, None, &headline_style);
            let (detail_w, _) = ctx.text.measure_style(detail, None, &detail_style);
            let natural_w = f64::from(headline_w.max(detail_w));
            let beside = text_x + natural_w + gap + retry_w + pad <= self.column.width();
            let text_w = (self.column.width() - text_x - pad).max(1.0);
            let (_, headline_h) = ctx.text.measure_style(
                UNSAVED_TITLE,
                Some(ui::num::f32_of_f64(text_w)),
                &headline_style,
            );
            let (_, detail_h) =
                ctx.text
                    .measure_style(detail, Some(ui::num::f32_of_f64(text_w)), &detail_style);
            let banner_h =
                (f64::from(headline_h) + f64::from(spacing.s1) + f64::from(detail_h) + 2.0 * gap)
                    .max(f64::from(metrics.panel_row_height));
            let y0 = top + f64::from(spacing.s2);
            self.banner_rect = Rect::new(self.column.x0, y0, self.column.x1, y0 + banner_h);
            let retry_origin = if beside {
                Point::new(
                    self.column.x1 - pad - retry_w,
                    y0 + (banner_h - retry_h) / 2.0,
                )
            } else {
                Point::new(
                    self.column.x1 - retry_w,
                    y0 + banner_h + f64::from(spacing.s2),
                )
            };
            self.retry_rect = Rect::from_origin_size(retry_origin, (retry_w, retry_h));
            top = if beside {
                self.banner_rect.y1
            } else {
                self.retry_rect.y1
            } + f64::from(spacing.s2);
        }
        let list_rect = Rect::new(self.rect.x0, top, self.rect.x1, self.rect.y1);
        self.list.set_viewport(list_rect, self.column);
    }

    fn paint_banner(&self, ctx: &mut PaintCtx<'_>) {
        if !self.persistence_pending() {
            return;
        }
        Banner::paint(
            ctx,
            self.banner_rect,
            &BannerContent {
                kind: BannerKind::Warning,
                icon: BannerKind::Warning.default_icon(),
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
                .role(ButtonRole::Secondary)
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
    /// Hand the sheet's outcome, if any, to the app.
    fn drain_add_panel(&mut self) {
        self.add_panel.pump();
        while let Some(event) = self.add_panel.take_event() {
            self.apply_add_panel_event(event);
        }
        if !self.add_panel.is_open() && self.add_panel.widget().is_some() {
            self.add_panel.close();
            self.structure_dirty = true;
        }
    }
}

impl Widget for AuthenticatorApp {
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        // Pull in any QR image the host portal picker returned since the last
        // frame (off-thread; never blocks the UI).
        self.drain_portal();
        // Sheets that left the tree last frame can go now.
        if self.add_panel.reap() {
            self.structure_dirty = true;
        }
        self.drain_add_panel();
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
            self.drain_add_panel();
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
            self.apply_list_event(ev);
        }
        // Keep the list's selection mirror current.
        self.list.set_selected(self.selected);
        if self.list_dirty {
            self.refresh_list();
            self.list_dirty = false;
        }
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
        if self.persistence_pending() {
            children.push(
                AccessNode::new(Role::Toolbar)
                    .with_name("Key status")
                    .with_children(vec![
                        AccessNode::new(Role::Button)
                            .with_name("Retry saving keys")
                            .with_description(
                                self.persistence_message
                                    .as_deref()
                                    .unwrap_or("Retry saving this device\u{2019}s keys"),
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
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if target == "totp.retry-persist" {
            return matches!(action, A11yAction::Activate) && self.retry_persist();
        }
        self.perform_a11y_target(target, action)
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

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;
