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
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{Banner, BannerContent, BannerKind, TaffyTreeExt, Widget};

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
    /// How many accounts the list has, for the header row's count.
    account_count: Option<ui::LocalState<usize>>,
    /// A capture's clock was moved to its scenario's step already.
    clock_staged: bool,
}

impl Default for AuthenticatorApp {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthenticatorApp {
    #[cfg(test)]
    pub(crate) fn test_panel(&mut self) -> &mut AddAccountPanel {
        &mut self.add_panel
    }

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
            account_count: None,
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

    /// Report through `signal` how many accounts the list has, for the
    /// header row's count.
    #[must_use]
    pub(crate) fn with_count_signal(mut self, signal: ui::LocalState<usize>) -> Self {
        self.account_count = Some(signal);
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
        if let Some(signal) = &self.account_count {
            signal.set(self.list.len());
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
            // The banner spans the column the rows stand in and holds Retry:
            // beside its text when it fits, under it inside the box when not.
            let content = self.banner_content();
            let banner_h = Banner::height(&content, self.column.width(), ctx.text, theme);
            let y0 = top + f64::from(spacing.s2);
            let banner = Rect::new(self.column.x0, y0, self.column.x1, y0 + banner_h);
            let retry = Banner::layout(&content, banner, ctx.text, theme)
                .action
                .unwrap_or(Rect::ZERO);
            self.banner_rect = banner;
            self.retry_rect = retry;
            top = self.banner_rect.y1 + f64::from(spacing.s2);
        }
        let list_rect = Rect::new(self.rect.x0, top, self.rect.x1, self.rect.y1);
        self.list.set_viewport(list_rect, self.column);
    }

    /// The unsaved-changes banner, with Retry inside it.
    fn banner_content(&self) -> BannerContent<'_> {
        BannerContent::new(BannerKind::Warning, UNSAVED_TITLE)
            .with_message(Some(
                self.persistence_message
                    .as_deref()
                    .unwrap_or(UNSAVED_DETAIL),
            ))
            .action(RETRY_LABEL)
    }

    fn paint_banner(&self, ctx: &mut PaintCtx<'_>) {
        if !self.persistence_pending() {
            return;
        }
        Banner::paint(
            ctx,
            self.banner_rect,
            &self.banner_content(),
            self.last_mouse,
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
        if ui::widget::toast::transient_expired(*when, TOAST_TTL) {
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
    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &automerge::ObjId) {
        // Hosted controls can enqueue a local request after event routing.
        // Settle the retained sheet first, then consume those requests in
        // the same frame even when no document action changed the vault.
        if let Some(sheet) = self.add_panel.widget_mut() {
            sheet.settle(edits, path);
        }
        self.drain_add_panel();
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
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
        self.add_panel.set_window_height(layout.size.height);
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
        self.list.handle_event(event, ctx);
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
        // Omit the virtual account actions while the sheet owns the window.
        // This ancestor remains visible so its retained ModalDialog and focus
        // trap are reachable; hiding it would prune the sheet's fields too.
        if self.add_panel.is_open() {
            return Some(
                AccessNode::new(Role::Group)
                    .with_name("Authenticator")
                    .with_bounds(self.rect),
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
        _state: &mut ui::widget::Edits<'_>,
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
        !self.reduce_motion || self.list.needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        if !self.reduce_motion {
            return None;
        }
        let subsec_nanos = ui::time::unix_now_ns() % 1_000_000_000;
        let until_next_second = Duration::from_nanos(1_000_000_000 - subsec_nanos);
        let tick = ui::time::now() + until_next_second;
        Some(
            self.list
                .next_wake_deadline()
                .map_or(tick, |fade| fade.min(tick)),
        )
    }

    fn wants_pointer_capture(&self) -> bool {
        self.list.is_dragging()
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.reduce_motion = reduced;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::add_panel::tests::{click, hosted, nodes, panel, rect, settle, type_into};
    use crate::model::demo_accounts;
    use crate::resource::{Totp, TotpEffect};
    use crate::test_events::{named_press, no_mods};
    use ui::headless::Framed;

    /// The surface over the demo accounts, every seed on this device, as the
    /// framework presents them: for tests of layout, input and semantics,
    /// which change nothing in the document.
    fn app_with_state() -> (AuthenticatorApp, ui::State) {
        app_with_seeds(0)
    }

    /// As [`app_with_state`], without seeds for the first `missing` accounts.
    fn app_with_seeds(missing: usize) -> (AuthenticatorApp, ui::State) {
        let mut app = AuthenticatorApp::new();
        let mut seeds = Seeds::new(SecretStore::memory("so.tilo.totp"), true);
        for account in demo_accounts().iter().skip(missing) {
            seeds.put(account.id, &account.secret);
        }
        app.seeds = seeds;
        let metadata: Vec<Account> = demo_accounts()
            .into_iter()
            .map(|mut account| {
                account.secret.clear();
                account
            })
            .collect();
        app.present_accounts(&Rc::new(metadata));
        (app, ui::State::new())
    }

    /// Mount `app` over `state` in the shared frame harness and run its
    /// first frame at `w`x`h`: the production sync, layout and draw passes.
    fn framed(
        (app, state): (AuthenticatorApp, ui::State),
        w: u32,
        h: u32,
    ) -> Framed<AuthenticatorApp> {
        Framed::new(state, app, (w, h), Theme::dark())
    }

    /// Route one event through the app's real `handle_event`, returning
    /// whether it was marked handled.
    fn dispatch(
        app: &mut Framed<AuthenticatorApp>,
        ev: &WindowEvent,
        mods: winit::event::Modifiers,
    ) -> bool {
        Framed::harness_mut(app).dispatch_to_root_with(ev, &mods)
    }

    /// Reduced motion wakes on second boundaries instead of every frame.
    #[test]
    fn reduced_motion_replaces_vsync_with_a_second_boundary_wake() {
        let mut app = AuthenticatorApp::new();
        assert!(app.needs_animation_frame());
        assert!(app.next_wake_deadline().is_none());

        Widget::set_motion_reduced(&mut app, true);

        assert!(!app.needs_animation_frame());
        let deadline = app
            .next_wake_deadline()
            .expect("the discrete countdown schedules its next second");
        let delay = deadline.saturating_duration_since(ui::time::now());
        assert!(delay > Duration::ZERO);
        assert!(delay <= Duration::from_secs(1));
    }

    /// Accessibility, event and Enter copies share the row's clipboard semantics; toasts distinguish failures.
    #[test]
    fn copy_paths_share_row_semantics_and_toast_severity() {
        // Case `accessibility_copy_uses_the_existing_list_event_path`.
        {
            let mut framed = framed(app_with_state(), 640, 700);
            let (app, state) = Framed::split(&mut framed);
            let id = app.vault.ordered()[0].id;
            assert!(app.perform_semantic_action(
                &format!("totp.copy:{id}"),
                &A11yAction::Activate,
                &mut ui::widget::Edits::lend(&mut state.doc),
                &state.root,
            ));
            assert_eq!(app.selected_account(), Some(id));
            assert!(app.last_copied().is_some());
        }

        // Case `copy_event_selects_the_account_and_confirms_with_a_toast`.
        {
            let (mut app, _state) = app_with_state();
            let id = app.vault.ordered()[0].id;
            let acc = app.vault.get(id).cloned().unwrap();
            let before = totp::current_code(&acc);
            app.apply_list_event(ListEvent::Copy(id));
            let after = totp::current_code(&acc);
            assert_eq!(app.selected, Some(id), "the copied row becomes selected");
            let (msg, _, kind) = app
                .toast
                .clone()
                .expect("copying shows a confirmation toast");
            assert!(msg.starts_with("Copied"), "toast confirms the copy: {msg}");
            assert_eq!(kind, ToastKind::Success);
            // The copied string is the account's live TOTP code. It equals the
            // code at some instant during the copy, so it must match the value
            // just before or just after — deterministic even across a period
            // rollover.
            let copied = app.last_copied().expect("a copy records the copied code");
            assert!(
                copied == before || copied == after,
                "copied {copied:?} must equal the account's live code ({before:?}/{after:?})"
            );
        }

        // Case `enter_copies_the_selected_accounts_live_code`.
        {
            let mut app = framed(app_with_state(), 1000, 800);
            let id = app
                .selected
                .expect("the first account is selected when shown");
            let acc = app.vault.get(id).cloned().unwrap();

            let before = totp::current_code(&acc);
            dispatch(&mut app, &named_press(NamedKey::Enter), no_mods());
            let after = totp::current_code(&acc);

            let copied = app.last_copied().expect("Enter copies the selected code");
            assert!(
                copied == before || copied == after,
                "Enter copies the account's live TOTP code ({copied:?} vs {before:?}/{after:?})"
            );
            let (msg, _, kind) = app.toast.clone().expect("Enter shows a copy toast");
            assert!(msg.starts_with("Copied"), "toast confirms the copy: {msg}");
            assert_eq!(kind, ToastKind::Success);
        }

        // Case `copy_without_a_local_secret_is_an_error_toast`.
        {
            let (mut app, _state) = app_with_seeds(demo_accounts().len());
            let id = app.vault.ordered()[0].id;
            app.apply_list_event(ListEvent::Copy(id));
            assert!(app.last_copied().is_none(), "no secret, no code");
            assert_eq!(app.toast.as_ref().map(|t| t.2), Some(ToastKind::Error));
        }
    }

    /// The accessibility tree names accounts without codes and keeps the add modal protected and complete.
    #[test]

    fn accessibility_names_accounts_and_protects_the_modal() {
        let mut app = hosted((640, 700), None);
        let tree = nodes(&mut app);
        let account = tree
            .iter()
            .find(|n| {
                n.role == Role::ListItem && n.name.as_deref().is_some_and(|s| s.contains("GitHub"))
            })
            .expect("named account row");
        assert!(
            account.value.is_none(),
            "credential is never exported as value"
        );
        assert!(tree.iter().any(|n| {
            n.name
                .as_deref()
                .is_some_and(|s| s.starts_with("Copy code for"))
        }));
        app.effect(&TotpEffect::AddAccount).unwrap();
        settle(&mut app);
        let actual = app.harness().a11y_nodes();
        assert!(
            actual
                .iter()
                .any(|(_, n)| format!("{:?}", n.role()) == "Dialog")
        );
        assert!(
            !actual
                .iter()
                .any(|(_, n)| n.label().is_some_and(|s| s.starts_with("Copy code for"))),
            "modal removes background actions"
        );
        let focus_order = app.harness().focus_access_nodes();
        assert!(
            focus_order.iter().any(|node| node.role == Role::TextInput),
            "the visible modal exposes its fields to keyboard focus: {focus_order:?}"
        );
        assert!(
            !focus_order.iter().any(|node| matches!(
                node.semantic_key.as_deref(),
                Some(
                    "search-open" | "add-account" | "nav-shell-more-content" | "nav-shell-history"
                )
            )),
            "the modal trap excludes background header controls: {focus_order:?}"
        );
        let secret = nodes(&mut app)
            .into_iter()
            .find(|n| n.role == Role::TextInput && n.state.protected)
            .expect("protected secret field");
        assert!(secret.value.is_none());
        type_into(&mut app, "totp-issuer", "Acme");
        assert_eq!(panel(&mut app, |p| p.field_snapshot().0), "Acme");
        app.harness().key("Enter");
        settle(&mut app);
        assert!(
            panel(&mut app, |p| p.is_open()),
            "validation failure keeps form open"
        );
        assert!(
            panel(&mut app, |p| p.problem().is_some()),
            "validation communicates failure"
        );
        assert!(
            nodes(&mut app)
                .iter()
                .any(|node| node.live == Some(Politeness::Assertive)),
            "invalid Save announces its validation error immediately"
        );
        click(&mut app, "Advanced options");
        click(&mut app, "SHA256");
        click(&mut app, "8");
        let selected = nodes(&mut app);
        assert!(
            selected
                .iter()
                .any(|n| n.name.as_deref() == Some("SHA256") && n.state.checked == Some(true))
        );
        assert!(
            selected
                .iter()
                .any(|n| n.name.as_deref() == Some("8") && n.state.checked == Some(true))
        );
    }

    /// Narrow, missing-key and resized layouts keep targets inside one centred column.
    #[test]
    fn narrow_and_resized_layouts_keep_one_readable_column() {
        let mut app = hosted((320, 640), None);
        let add = rect(&mut app, "Add account");
        assert!(
            add.x0 >= 0.0 && add.x1 <= 320.0,
            "shared header target inside narrow window"
        );
        let mut missing = hosted((390, 844), Some("missing-secret"));
        let tree = nodes(&mut missing);
        assert!(
            tree.iter()
                .any(|n| n.role == Role::ListItem && n.bounds.is_some()),
            "initial row is accessible with missing key"
        );
        app.harness().resize((1920, 1080)).unwrap();
        settle(&mut app);
        let wide = app
            .harness()
            .with_widget_mut(|w: &mut AuthenticatorApp| w.column)
            .unwrap();
        let expected =
            AuthenticatorApp::readable_column(Rect::new(0.0, 0.0, 1920.0, 1080.0), &Theme::dark());
        assert!((wide.width() - expected.width()).abs() < 0.5);
        assert!(
            (wide.center().x - 960.0).abs() < 0.5,
            "readable content centered"
        );
        let wide_add = rect(&mut app, "Add account");
        app.harness().resize((560, 720)).unwrap();
        settle(&mut app);
        let narrow = app
            .harness()
            .with_widget_mut(|w: &mut AuthenticatorApp| w.column)
            .unwrap();
        assert!(narrow.width() < wide.width(), "same live instance reflows");
        let narrow_add = rect(&mut app, "Add account");
        assert!(narrow_add.x0 < wide_add.x0 && narrow_add.x1 <= 560.0);
        app.harness().resize((1920, 1080)).unwrap();
        settle(&mut app);
        let restored = app
            .harness()
            .with_widget_mut(|w: &mut AuthenticatorApp| w.column)
            .unwrap();
        assert!(
            (restored.width() - wide.width()).abs() < 0.5,
            "recovers wide geometry"
        );
    }

    /// Search filtering, including the contextual empty message.
    #[test]
    fn search_filters_the_visible_accounts() {
        use ui::search::SearchProvider as _;
        use ui::tilo_resource::Resource as _;
        let mut app = hosted((1000, 800), None);
        let count = app.state().len();
        let entries = Totp::search(&app.state());
        let provider = ui::framework::testing::search_source::<Totp>(entries);
        let matches = provider.search_page(&ui::search::SearchQuery::new("github"), None, 100, 100);
        assert_eq!(
            matches.results.len(),
            1,
            "canonical provider returns matching account"
        );
        assert_eq!(
            matches.results.first().expect("one canonical match").title,
            "GitHub"
        );
        let empty = provider.search_page(
            &ui::search::SearchQuery::new("zzz-no-such-service"),
            None,
            100,
            100,
        );
        assert!(
            empty.results.is_empty(),
            "canonical provider reports no matches"
        );
        app.harness()
            .open_search(&ui::search::SearchRequest::this_app())
            .unwrap();
        settle(&mut app);
        assert_eq!(
            app.harness()
                .focused_access_node()
                .unwrap()
                .children
                .into_iter()
                .find(|node| node.name.as_deref() == Some("Search"))
                .expect("the hosted palette exposes its query")
                .role,
            ui::a11y::Role::TextInput,
            "platform search owns its query field"
        );
        app.harness().type_text("github");
        settle(&mut app);
        assert_eq!(
            app.harness()
                .focused_access_node()
                .unwrap()
                .children
                .into_iter()
                .find(|node| node.name.as_deref() == Some("Search"))
                .expect("query child")
                .value
                .as_deref(),
            Some("github")
        );
        assert_eq!(
            app.state().len(),
            count,
            "system search does not filter or rewrite the shared vault"
        );
        app.harness().key("Escape");
        settle(&mut app);
        assert!(
            app.harness().inspection_json().unwrap().contains("GitHub"),
            "closing search restores account content"
        );
    }

    /// The persistence banner has its own bounded click and accessibility retry.
    #[test]
    fn persistence_banner_has_its_own_bounded_retry() {
        let (mut app, state) = app_with_state();
        app.stage_scenario(Some("persistence-error"));
        let mut framed = Framed::new(state, app, (390, 720), Theme::dark());
        let (app, state) = Framed::split(&mut framed);
        assert!(
            app.toast.is_none(),
            "durable banner replaces duplicate toast"
        );
        assert!(app.retry_rect.x0 >= app.rect.x0 && app.retry_rect.x1 <= app.rect.x1);
        assert!(
            app.banner_rect.contains_rect(app.retry_rect),
            "retry is inside banner"
        );
        assert!(
            app.list
                .account_bounds(app.vault.ordered().first().expect("seeded account").id)
                .expect("first row painted")
                .y0
                >= app.banner_rect.y1,
            "banner reserves list space"
        );
        let tree = app.accessibility().unwrap();
        let retry = tree
            .children
            .iter()
            .flat_map(|n| &n.children)
            .find(|n| n.action_target.as_deref() == Some("totp.retry-persist"))
            .expect("separate accessible retry");
        assert_eq!(
            retry.bounds.map(ui::a11y::AccessBounds::to_rect),
            Some(app.retry_rect)
        );
        assert!(retry.state.focusable);
        assert!(app.perform_semantic_action(
            "totp.retry-persist",
            &A11yAction::Activate,
            &mut ui::widget::Edits::lend(&mut state.doc),
            &state.root
        ));
        assert!(!app.persistence_pending());
    }

    /// QR import: host portal routing, frame drain, cancel and file decoding.
    #[test]
    fn qr_import_routes_through_the_host_portal_and_fills_the_panel() {
        // Case `qr_cta_routes_to_the_host_portal`.
        // The primary "Scan a QR image" action must reach the **host** file
        // chooser (the XDG portal), not an in-app picker or a dead-end toast. A
        // recording host answers the request so no real dialog opens; the
        // in-flight guard keeps a second tap from stacking another dialog.
        {
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            let (mut app, _state) = app_with_state();
            app.rect = Rect::new(0.0, 0.0, 1000.0, 800.0);
            app.add_panel.open();
            app.request_qr_via_portal();
            assert!(
                app.portal_rx.is_some(),
                "the QR action routes through the host portal (not an in-app picker)"
            );
            app.request_qr_via_portal();
            let requests = recorder.pick_requests();
            assert_eq!(requests.len(), 1, "an in-flight request is not stacked");
            assert_eq!(requests[0].title(), "Choose a QR image");
        }

        // Case `portal_chosen_qr_image_fills_the_add_panel_via_frame_drain`.
        {
            let uri = "otpauth://totp/Acme:bob@acme.io?secret=JBSWY3DPEHPK3PXP&issuer=Acme";
            let dir = std::env::temp_dir().join(format!("totp-portal-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let img = dir.join("acme-qr.png");
            render_qr_png(uri, &img);

            let (mut app, state) = app_with_state();
            app.add_panel.open();

            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(img));
            ui::host::scoped(&services, || app.request_qr_via_portal());
            app.sync(&state.doc, &state.root);

            assert!(
                app.portal_rx.is_none(),
                "the portal channel clears after delivering"
            );
            let (issuer, label, secret) = app.add_panel.field_snapshot();
            assert_eq!(issuer, "Acme");
            assert_eq!(label, "bob@acme.io");
            assert_eq!(secret, "JBSWY3DPEHPK3PXP");
            std::fs::remove_dir_all(&dir).ok();
        }

        // Case `portal_cancel_leaves_the_add_panel_untouched`.
        {
            let (mut app, state) = app_with_state();
            app.add_panel.open();
            let before = app.add_panel.field_snapshot();
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Cancelled);
            ui::host::scoped(&services, || app.request_qr_via_portal());
            app.sync(&state.doc, &state.root);
            assert!(
                app.portal_rx.is_none(),
                "the portal channel clears after a cancel"
            );
            assert_eq!(
                app.add_panel.field_snapshot(),
                before,
                "cancel changes nothing"
            );
        }

        // Case `qr_image_file_decodes_and_fills_the_panel`.
        {
            let uri = "otpauth://totp/Acme:bob@acme.io?secret=JBSWY3DPEHPK3PXP&issuer=Acme";
            let path = std::env::temp_dir().join(format!("totp-qr-{}.png", uuid::Uuid::new_v4()));
            render_qr_png(uri, &path);
            let decoded = crate::qr::decode_image_file(&path).expect("decode a real QR png file");
            let mut panel = crate::add_panel::AddAccountPanel::new();
            panel.open();
            panel.apply_uri(&decoded);
            let (issuer, label, secret) = panel.field_snapshot();
            assert_eq!(issuer, "Acme");
            assert_eq!(label, "bob@acme.io");
            assert_eq!(secret, "JBSWY3DPEHPK3PXP");
            std::fs::remove_file(&path).ok();
        }
    }

    /// Add button, Ctrl+N, Escape and the modal's gating of platform search.
    #[test]
    fn shortcuts_and_clicks_open_the_modal_and_focus_search() {
        let mut app = hosted((1000, 800), None);
        assert!(!panel(&mut app, |p| p.is_open()));
        click(&mut app, "Add account");
        assert!(panel(&mut app, |p| p.is_open()));
        app.harness().key("Escape");
        settle(&mut app);
        assert!(!panel(&mut app, |p| p.is_open()));
        app.harness().set_modifiers(&["ctrl".into()]);
        app.harness().key("n");
        app.harness().set_modifiers(&[]);
        settle(&mut app);
        assert!(
            panel(&mut app, |p| p.is_open()),
            "Ctrl+N opens canonical add flow"
        );
        assert!(
            app.harness()
                .open_search(&ui::search::SearchRequest::this_app())
                .is_err(),
            "modal isolates platform search"
        );
        assert!(panel(&mut app, |p| p.is_open()));
        app.harness().key("Escape");
        settle(&mut app);
        app.harness()
            .open_search(&ui::search::SearchRequest::this_app())
            .unwrap();
        settle(&mut app);
        assert_eq!(
            app.harness()
                .focused_access_node()
                .unwrap()
                .children
                .into_iter()
                .find(|node| node.name.as_deref() == Some("Search"))
                .expect("the hosted palette exposes its query")
                .role,
            ui::a11y::Role::TextInput
        );
        app.harness().type_text("github");
        settle(&mut app);
        assert_eq!(
            app.harness()
                .focused_access_node()
                .unwrap()
                .children
                .into_iter()
                .find(|node| node.name.as_deref() == Some("Search"))
                .expect("query child")
                .value
                .as_deref(),
            Some("github")
        );
        app.harness().key("Escape");
        settle(&mut app);
        app.harness()
            .open_search(&ui::search::SearchRequest::this_app())
            .expect("Escape dismissed the preceding search");
        app.harness().key("Escape");
        settle(&mut app);
    }

    /// Rasterise `data` as a QR code into a greyscale PNG on disk, scaling
    /// modules up with a quiet zone so the decoder has clean finder patterns.
    fn render_qr_png(data: &str, path: &std::path::Path) {
        use image::{GrayImage, Luma};
        use qrcode::{EcLevel, QrCode};
        let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::M).unwrap();
        let modules = code.to_colors();
        let dim = (modules.len() as f64).sqrt() as usize;
        let scale = 6usize;
        let quiet = 4usize;
        let side = ((dim + quiet * 2) * scale) as u32;
        let mut img = GrayImage::from_pixel(side, side, Luma([255]));
        for y in 0..dim {
            for x in 0..dim {
                if modules[y * dim + x] == qrcode::Color::Dark {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let px = ((x + quiet) * scale + dx) as u32;
                            let py = ((y + quiet) * scale + dy) as u32;
                            img.put_pixel(px, py, Luma([0]));
                        }
                    }
                }
            }
        }
        img.save(path).unwrap();
    }
}
