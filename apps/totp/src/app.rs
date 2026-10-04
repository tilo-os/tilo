//! The Authenticator's surface over the [`Totp`] resource.
//!
//! The document holds account metadata (names, order and lifecycle
//! collaborate without credentials entering history); this device's secret
//! store holds each account's seed ([`Seeds`]). The surface joins the two into
//! the accounts it shows, submits every metadata change as a [`TotpAction`]
//! through the framework's dispatcher, and composes the three surfaces:
//!
//!  * a header (title, account count, search field, **+ Add** button),
//!  * the scrollable [`AccountList`] (main screen),
//!  * the [`AddAccountPanel`] modal (second screen).
//!
//! Layout is done by hand from the root rect rather than through the
//! taffy tree: the app registers a single full-window node and positions
//! its children with rect maths, forwarding events to them directly.
//! This keeps the rich, mixed-size composition pixel-exact and decoupled
//! from the global focus order (the modal manages its own field focus).

use std::collections::HashMap;
use std::rc::Rc;
use ui::time::{Duration, Instant};
use ui::widget::{AppModel, ModelPath};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::a11y::{AccessNode, AccessState, Action as A11yAction, Politeness, Role};
use ui::framework::Dispatcher;
use ui::paint::PaintCtx;
use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::secrets::SecretStore;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::tokens::spacing::Spacing;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{Banner, BannerContent, BannerKind, Button, ButtonFace, TextInput, Widget};

use crate::actions::TotpAction;
use crate::add_panel::{AddAccountPanel, AddPanelEvent};
use crate::list::{AccountList, ListEvent};
use crate::model::{Account, AccountId, Vault};
use crate::resource::{Totp, TotpEffect};
use crate::secrets::Seeds;
use crate::totp;

const OUTER_PAD: f64 = 24.0;
const ADD_BTN_W: f64 = 168.0;
const COMPACT_ADD_BTN_W: f64 = 112.0;
const EDIT_BTN_W: f64 = 94.0;
const COMPACT_EDIT_BTN_W: f64 = 64.0;
const ADD_BTN_H: f64 = 40.0;
const SEARCH_H: f64 = 44.0;
const HEADER_H: f64 = 150.0;
/// The unsaved-vault banner: an `s2` breath under the header hairline, the
/// 32 px band itself, then an `s4` section gap before the first card (which
/// the list pads by its own `s2`, so the band the list reserves is `s4 - s2`
/// shorter than the visible gap). Spelled from `Spacing::BASE` because
/// layout runs without a theme.
const BANNER_TOP_GAP: f64 = Spacing::BASE.s2 as f64;
/// The shared `Banner`'s pinned 1× band (its own content-height floor).
const BANNER_H: f64 = 44.0;
const BANNER_BOTTOM_GAP: f64 = (Spacing::BASE.s4 - Spacing::BASE.s2) as f64;
const PERSISTENCE_BANNER_BAND: f64 = BANNER_TOP_GAP + BANNER_H + BANNER_BOTTOM_GAP;
/// Gap between the banner's message and its Retry button (`s2`): the two
/// used to share an edge.
const BANNER_RETRY_GAP: f64 = Spacing::BASE.s2 as f64;
const BANNER_RETRY_W: f64 = 92.0;
/// The header actions, the search field, the persistence banner and the
/// account list share one centred content column no wider than this at
/// normal type. A row is an identity on the left and a code on the right; an
/// unbounded row on a wide window put those ~1400px apart. This is a
/// composition choice for this app (validated with 8-digit codes and long
/// identities), not a framework width rule.
const READABLE_CONTENT_W: f64 = 760.0;
/// Top inset of the header's action row.
const HEADER_BTN_TOP: f64 = 18.0;
/// Gap between the Edit and Add buttons — siblings, so the `s3` step.
const HEADER_BTN_GAP: f64 = Spacing::BASE.s3 as f64;
/// Horizontal padding inside a header button around its measured label (the
/// `s4` spacing step, spelled out because layout runs without a theme).
const HEADER_BTN_PAD: f64 = 16.0;
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

#[allow(clippy::struct_excessive_bools, reason = "independent UI/frame flags")]
pub struct AuthenticatorApp {
    /// The accounts as the document holds them: metadata, no secrets.
    accounts: Rc<Vec<Account>>,
    /// Each account's seed on this device.
    seeds: Seeds,
    /// The accounts joined with this device's seeds: what the list shows and
    /// codes are computed from.
    vault: Vault,
    search: TextInput,
    list: AccountList,
    add_panel: AddAccountPanel,
    /// An in-flight XDG-portal file-open for QR-image import. When the user
    /// taps "Scan a QR image" the portal round-trip runs on a worker thread
    /// and hands the chosen path back over this channel, drained once per
    /// frame in [`Widget::sync`]. `None` when no picker is in flight — the app
    /// never draws its own picker (HIG §5).
    portal_rx: Option<PortalChannel>,

    rect: Rect,
    root_node: Option<NodeId>,
    header_rect: Rect,
    /// The centred readable column (see [`READABLE_CONTENT_W`]) the header
    /// actions, search, banner and list are laid out in.
    content_rect: Rect,
    search_rect: Rect,
    add_btn_rect: Rect,
    edit_btn_rect: Rect,
    persistence_retry_rect: Rect,
    list_rect: Rect,
    /// Measured widths of the Add and Edit button labels, captured by the
    /// draw pass so `apply_layout` can size the buttons from the text they
    /// carry (a larger text scale widens the button rather than spilling the
    /// caption past it). `None` until the first paint, when the authored
    /// widths stand in.
    header_label_w: Option<(f64, f64)>,

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
    /// The visible (filtered/ordered) list must be recomputed — set when the
    /// vault or the search query changes.
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
        Self {
            accounts: Rc::default(),
            seeds: Seeds::new(SecretStore::memory("so.tilo.totp"), false),
            vault: Vault::new(),
            // The shared field owns its placeholder so the hint shares the
            // typed text's font, gutter and centre line.
            search: TextInput::local()
                .with_kind(ui::widget::TextInputKind::Search)
                .with_placeholder("Search accounts\u{2026}"),
            list: AccountList::new(),
            add_panel: AddAccountPanel::new(),
            portal_rx: None,
            rect: Rect::ZERO,
            root_node: None,
            header_rect: Rect::ZERO,
            content_rect: Rect::ZERO,
            search_rect: Rect::ZERO,
            add_btn_rect: Rect::ZERO,
            edit_btn_rect: Rect::ZERO,
            persistence_retry_rect: Rect::ZERO,
            list_rect: Rect::ZERO,
            header_label_w: None,
            last_mouse: Point::ZERO,
            selected: None,
            toast: None,
            last_copied: None,
            pending_delete: None,
            list_dirty: false,
            reduce_motion: false,
            persistence_message: None,
            dispatch: None,
        }
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
                    "This device's secrets couldn't be read. Unlock the keyring or restore access, then retry.",
                );
            } else if self.seeds.pending() {
                self.describe_persistence_failure(
                    "Secret changes aren't saved on this device. Restore keyring or storage access, then retry.",
                );
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
            self.selected = ids.first().copied();
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

    /// Try this device's secret store again, as the user asked.
    fn retry_persist(&mut self) -> bool {
        if !self.persistence_pending() {
            return true;
        }
        let ids: Vec<AccountId> = self.accounts.iter().map(|account| account.id).collect();
        if self.seeds.retry(&ids) {
            self.persistence_message = None;
            self.join_seeds();
            self.toast("Secrets saved on this device", ToastKind::Success);
            true
        } else {
            self.describe_persistence_failure(
                "Secrets still aren't saved on this device. Restore keyring or storage access, then retry.",
            );
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

    /// Recompute the filtered/ordered visible set and push it to the list.
    fn refresh_list(&mut self) {
        let query = self.search.value().to_string();
        let visible = self.vault.search(&query);
        if visible.is_empty() {
            if query.trim().is_empty() {
                self.list.set_empty_message("No accounts yet");
            } else {
                self.list.set_empty_message(format!(
                    "No accounts match \u{201c}{}\u{201d}",
                    query.trim()
                ));
            }
        }
        self.list.set_selected(self.selected);
        self.list.set_accounts(visible);
    }

    fn toast(&mut self, msg: impl Into<String>, kind: ToastKind) {
        self.toast = Some((msg.into(), ui::time::now(), kind));
    }

    fn copy_account(&mut self, id: AccountId) {
        if let Some(acc) = self.vault.get(id).cloned() {
            if !acc.secret_is_valid() {
                self.toast(
                    format!("No local secret for {}", acc.title()),
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
            ListEvent::MoveUp(id) => self.apply(&crate::actions::move_up_action(id)),
            ListEvent::MoveDown(id) => self.apply(&crate::actions::move_down_action(id)),
            ListEvent::Delete(id) => {
                // Deleting asks first; the account (and with it this device's
                // secret) goes once the document drops it.
                self.pending_delete = Some(id);
                self.apply(&crate::actions::delete_account_action(id));
            }
        }
    }

    /// Act on one outcome drained from the Add-account modal. The secret goes
    /// to this device's store; only metadata goes to the document, with the
    /// colour slot and list order pinned at authoring time so replay is
    /// deterministic.
    fn apply_add_panel_event(&mut self, ev: AddPanelEvent) {
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
                    "Account was added, but its secret isn't saved on this device. Keep this session open, restore access, then retry.",
                );
                self.apply(&crate::actions::add_account_action(&acc, order));
                self.selected = Some(id);
                if saved {
                    self.toast("Account added", ToastKind::Success);
                }
            }
            AddPanelEvent::Edit(mut acc) => {
                let Some(before) = self.vault.get(acc.id).cloned() else {
                    self.toast("Account is no longer available", ToastKind::Error);
                    self.list_dirty = true;
                    return;
                };
                // The form edits identity, secret and code parameters; the
                // list owns colour and order.
                acc.color = before.color;
                acc.order = before.order;
                let saved = acc.secret == before.secret
                    || self.keep_seed(
                        acc.id,
                        &acc.secret,
                        "Account changed, but its new secret isn't saved on this device. Restore access, then retry.",
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
                    self.toast("Account is no longer available", ToastKind::Error);
                    self.list_dirty = true;
                    return;
                }
                // Only the local secret is installed; the shared identity,
                // order and colour stay as the document has them, and the
                // form checked the code parameters against them.
                let saved = self.keep_seed(
                    acc.id,
                    &acc.secret,
                    "Secret is available only for this session; it isn't saved on this device. Restore access, then retry.",
                );
                self.join_seeds();
                if saved {
                    self.toast("Secret set up on this device", ToastKind::Success);
                }
            }
        }
        self.list_dirty = true;
    }

    /// Copy the selected account (or the first visible one) — the Enter
    /// shortcut.
    fn copy_focused(&mut self) {
        let target = self
            .selected
            .filter(|id| self.vault.get(*id).is_some())
            .or_else(|| self.vault.search(self.search.value()).first().map(|a| a.id));
        if let Some(id) = target {
            self.copy_account(id);
        }
    }

    fn open_selected_for_edit(&mut self) -> bool {
        let Some(account) = self.selected.and_then(|id| self.vault.get(id)).cloned() else {
            return false;
        };
        if account.secret_is_valid() {
            self.add_panel.open_for_edit(&account);
        } else {
            self.add_panel.open_for_provision(&account);
        }
        self.add_panel.layout(self.rect);
        true
    }

    /// Move the selected account through the same visible, filtered order the
    /// pointer list paints. This keeps keyboard selection in step with the
    /// list rather than introducing a second focus model.
    fn move_selection(&mut self, delta: isize) -> bool {
        let visible = self.vault.search(self.search.value());
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
        }
        self.list.set_selected(self.selected);
        true
    }

    fn accessible_account_name(account: &crate::model::Account) -> String {
        if account.label.trim().is_empty() {
            account.title().to_string()
        } else {
            format!("{}, {}", account.title(), account.label)
        }
    }

    /// Resolve a published canvas target to the existing list/app action.
    /// Keeping this beside `apply_list_event` ensures assistive activation
    /// cannot acquire a separate mutation path from pointer input.
    fn perform_a11y_target(&mut self, target: &str, action: &A11yAction) -> bool {
        if target == "totp.add" {
            if matches!(action, A11yAction::Activate) {
                self.add_panel.open();
                self.add_panel.layout(self.rect);
                return true;
            }
            return false;
        }
        if target == "totp.edit" {
            return matches!(action, A11yAction::Activate) && self.open_selected_for_edit();
        }
        if target == "totp.search" {
            if matches!(action, A11yAction::Activate | A11yAction::Focus) {
                self.search.set_focus(true);
                return true;
            }
            return false;
        }
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
            let Some(account) = self.vault.get(id).cloned() else {
                return false;
            };
            if account.secret_is_valid() {
                return false;
            }
            self.selected = Some(id);
            self.add_panel.open_for_provision(&account);
            self.add_panel.layout(self.rect);
            return true;
        }
        let event = match verb {
            "totp.select" => ListEvent::Select(id),
            "totp.copy" => ListEvent::Copy(id),
            "totp.move-up" => ListEvent::MoveUp(id),
            "totp.move-down" => ListEvent::MoveDown(id),
            "totp.delete" => ListEvent::Delete(id),
            _ => return false,
        };
        self.apply_list_event(event);
        self.list.set_selected(self.selected);
        true
    }

    /// Global keyboard shortcuts available whenever the add-panel modal
    /// isn't open: Ctrl/Cmd+N (add), Ctrl/Cmd+F and `/` (focus search),
    /// Escape (clear + unfocus search) and Enter (copy the focused code).
    /// Returns `true` when the event was fully consumed here — `handled`
    /// is already set in that case, so [`Widget::handle_event`] should stop
    /// routing further.
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

        // Ctrl/Cmd+N → add account.
        if let Key::Character(c) = logical_key {
            let lc = c.to_lowercase();
            if (mods.control_key() || mods.super_key()) && lc == "n" {
                self.add_panel.open();
                self.add_panel.layout(self.rect);
                *handled = true;
                return true;
            }
            if (mods.control_key() || mods.super_key()) && lc == "f" {
                self.search.set_focus(true);
                *handled = true;
                return true;
            }
            if (mods.control_key() || mods.super_key())
                && lc == "e"
                && self.open_selected_for_edit()
            {
                *handled = true;
                return true;
            }
            // "/" focuses search when not already typing there.
            if c.as_str() == "/" && !self.search.is_focused() {
                self.search.set_focus(true);
                *handled = true;
                return true;
            }
        }
        match logical_key {
            Key::Named(NamedKey::ArrowDown) if !self.search.is_focused() => {
                if self.move_selection(1) {
                    *handled = true;
                    return true;
                }
            }
            Key::Named(NamedKey::ArrowUp) if !self.search.is_focused() => {
                if self.move_selection(-1) {
                    *handled = true;
                    return true;
                }
            }
            Key::Named(NamedKey::Escape) => {
                if self.search.is_focused() {
                    self.search.set_focus(false);
                    if !self.search.value().is_empty() {
                        self.search.set_text("");
                        self.refresh_list();
                    }
                    *handled = true;
                    return true;
                }
            }
            Key::Named(NamedKey::Enter) => {
                self.copy_focused();
                *handled = true;
                return true;
            }
            _ => {}
        }
        false
    }

    /// Add-button + search-field focus management on a left click. Returns
    /// `true` when the click opened the add modal — fully consumed, so
    /// [`Widget::handle_event`] should stop routing further; a click that
    /// only moves search focus returns `false` so the event still reaches
    /// the search field and list below.
    fn handle_add_and_search_focus_click(
        &mut self,
        event: &WindowEvent,
        handled: &mut bool,
    ) -> bool {
        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        else {
            return false;
        };
        if self.add_btn_rect.contains(self.last_mouse) {
            self.add_panel.open();
            self.add_panel.layout(self.rect);
            *handled = true;
            return true;
        }
        if self.persistence_retry_rect.contains(self.last_mouse) && self.retry_persist() {
            *handled = true;
            return true;
        }
        if self.edit_btn_rect.contains(self.last_mouse) && self.open_selected_for_edit() {
            *handled = true;
            return true;
        }
        let on_search = self.search_rect.contains(self.last_mouse);
        self.search.set_focus(on_search);
        false
    }

    /// Import a QR code from an image file into the open add panel.
    ///
    /// Choosing the screenshot/photo goes through the **host** file chooser
    /// (the XDG Desktop Portal, [`ui::portal`]) — the app never draws its
    /// own picker (HIG §5). The portal round-trip runs on a worker thread and
    /// the chosen path is drained each frame and decoded by the pure-Rust,
    /// offline QR reader in [`crate::qr`]. A `TOTP_QR_IMAGE` environment
    /// variable short-circuits the chooser with a fixed path — the seam the
    /// headless `--shots` and test flows drive.
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
    /// [`Widget::sync`]: decodes the chosen image into the open add panel,
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
                    // A cancel is a no-op — the add panel is left as it was.
                    PortalOutcome::Cancelled => {}
                    PortalOutcome::Failed(reason) => {
                        self.toast(
                            format!("Couldn't open the file chooser: {reason}"),
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

    /// Decode a QR image at `path` into the open add panel, or surface why
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

    /// Stage the view state a screenshot scenario shows.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        match scenario {
            // The QR import hands off to the host's file chooser (HIG §5),
            // which the app never draws: its scenario is the add screen the
            // import starts from.
            Some("add" | "add-picker") => self.add_panel.open(),
            Some("add-advanced") => self.add_panel.open_with_advanced(),
            Some("provision") => {
                self.selected = self.vault.ordered().first().map(|account| account.id);
                let _ = self.open_selected_for_edit();
            }
            Some("persistence-error") => self.describe_persistence_failure(
                "Secrets on this device aren't saved. Restore keyring or storage access, then retry.",
            ),
            _ => {}
        }
        self.add_panel.layout(self.rect);
    }

    /// Answer an effect invoked by a control, the palette, an agent or a
    /// script.
    pub(crate) fn perform_effect(&mut self, effect: TotpEffect) -> Result<(), String> {
        match effect {
            TotpEffect::AddAccount => {
                self.add_panel.open();
                self.add_panel.layout(self.rect);
            }
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
                let Some(account) = self.vault.get(id).cloned() else {
                    return Err("Authenticator account is no longer available".into());
                };
                self.selected = Some(id);
                self.add_panel.open_for_provision(&account);
                self.add_panel.layout(self.rect);
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

    /// Select `id` and clear the search so its row shows.
    fn reveal(&mut self, id: AccountId) {
        self.selected = Some(id);
        self.search.set_text("");
        self.list_dirty = true;
    }

    // ---- header drawing ----------------------------------------------

    #[allow(
        clippy::too_many_lines,
        reason = "the header uses shared rects for drawing and hit testing"
    )]
    fn draw_header(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        // The OS owns the window title/app-name — the header shows only the
        // contextual account count (functional status, not an app wordmark).
        let count = self.vault.len();
        let subtitle = match count {
            0 => "No accounts".to_string(),
            1 => "1 account".to_string(),
            n => format!("{n} accounts"),
        };

        // Measure both button labels first and re-place the buttons from
        // them, so the rects painted below are the rects the pointer hits.
        let compact = self.rect.width() < 420.0;
        let selected_has_secret = self
            .selected
            .and_then(|id| self.vault.get(id))
            .is_some_and(crate::model::Account::secret_is_valid);
        let edit_label = if selected_has_secret {
            if compact { "Edit" } else { "Edit selected" }
        } else if compact {
            "Set up"
        } else {
            "Set up selected"
        };
        let add_label = if compact { "+ Add" } else { "+  Add account" };
        let (edit_w, _) =
            text_renderer.measure_weighted(edit_label, 14.0, None, TextWeight::Medium);
        let (tw, _) = text_renderer.measure_weighted(add_label, 14.0, None, TextWeight::Semibold);
        self.header_label_w = Some((f64::from(tw), f64::from(edit_w)));
        self.layout_header_buttons();

        // The caption sits on the action row's centre line, measured with the
        // style it is painted with, instead of at a fixed offset.
        let (_, sub_h) = text_renderer.measure_weighted(&subtitle, 15.0, None, TextWeight::Medium);
        text_renderer.draw_text_weighted(
            scene,
            &subtitle,
            15.0,
            theme.tokens.colors.text_primary,
            transform
                * Affine::translate((
                    self.content_rect.x0 + OUTER_PAD,
                    self.add_btn_rect.center().y - f64::from(sub_h) / 2.0,
                )),
            None,
            TextWeight::Medium,
        );

        // Edit and Add are the shared button face (HIG §8): the neutral rest
        // edge, the disabled treatment, the primary fill and the label's
        // centring all come from `Button::paint` rather than a local copy
        // that stroked `border_standard` over a raised fill (invisible on the
        // dark preset) and painted its label literal white.
        let can_edit = self.selected.is_some_and(|id| self.vault.get(id).is_some());
        let edit_hovered = can_edit && self.edit_btn_rect.contains(self.last_mouse);
        Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ButtonFace::new(self.edit_btn_rect, edit_label)
                .text_size(14.0)
                .hovered(edit_hovered)
                .disabled(!can_edit),
        );
        let hovered = self.add_btn_rect.contains(self.last_mouse);
        Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ButtonFace::new(self.add_btn_rect, add_label)
                .primary()
                .text_size(14.0)
                .weight(TextWeight::Semibold)
                .hovered(hovered),
        );

        // Search field (TextInput draws its own chrome, focus ring and
        // placeholder).
        self.search.draw(
            scene,
            transform,
            text_renderer,
            theme,
            &PresenceCtx::empty(),
        );
    }

    /// The centred readable column the header actions, search, banner and
    /// list share: the full window when it is narrower than
    /// [`READABLE_CONTENT_W`], else that width centred in the window.
    fn readable_column(rect: Rect) -> Rect {
        let w = rect.width().min(READABLE_CONTENT_W);
        let x0 = rect.x0 + (rect.width() - w) / 2.0;
        Rect::new(x0, rect.y0, x0 + w, rect.y1)
    }

    /// Place the Add and Edit buttons on the header's action row inside the
    /// readable column. Widths come from the measured labels once the draw
    /// pass has supplied them and from the authored fallbacks before that.
    /// Both `apply_layout` and `draw_header` route through here, so the rect
    /// that is hit-tested is always the rect that was painted.
    fn layout_header_buttons(&mut self) {
        let compact = self.rect.width() < 420.0;
        let (add_w, edit_w) = match self.header_label_w {
            Some((add_lw, edit_lw)) => (
                add_lw + 2.0 * HEADER_BTN_PAD,
                edit_lw + 2.0 * HEADER_BTN_PAD,
            ),
            None if compact => (COMPACT_ADD_BTN_W, COMPACT_EDIT_BTN_W),
            None => (ADD_BTN_W, EDIT_BTN_W),
        };
        let c = self.content_rect;
        let top = self.rect.y0 + HEADER_BTN_TOP;
        self.add_btn_rect = Rect::new(
            c.x1 - OUTER_PAD - add_w,
            top,
            c.x1 - OUTER_PAD,
            top + ADD_BTN_H,
        );
        self.edit_btn_rect = Rect::new(
            self.add_btn_rect.x0 - HEADER_BTN_GAP - edit_w,
            top,
            self.add_btn_rect.x0 - HEADER_BTN_GAP,
            top + ADD_BTN_H,
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

impl Widget for AuthenticatorApp {
    fn sync(&mut self, _state: &mut AppModel, _path: &ModelPath) {
        // Pull in any QR image the host portal picker returned since the last
        // frame (off-thread; never blocks the UI).
        self.drain_portal();

        // Recompute the visible set only when the vault or query changed.
        if self.list_dirty {
            self.refresh_list();
            self.list_dirty = false;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        #[allow(
            clippy::unwrap_used,
            reason = "taffy 0.10's new_leaf always inserts and returns Ok; see ui::widget::TaffyTreeExt's doc comment"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                flex_grow: 1.0,
                ..Default::default()
            })
            .unwrap();
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

        self.header_rect = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + HEADER_H,
        );
        // Everything the user reads or acts on lives in one readable column;
        // the header band and its hairline still span the whole window.
        self.content_rect = Self::readable_column(self.rect);
        let c = self.content_rect;
        self.layout_header_buttons();
        let sy = self.rect.y0 + 88.0;
        self.search_rect = Rect::new(c.x0 + OUTER_PAD, sy, c.x1 - OUTER_PAD, sy + SEARCH_H);
        self.search.apply_layout_external(self.search_rect);

        self.list_rect = Rect::new(
            c.x0 + 12.0,
            self.rect.y0
                + HEADER_H
                + if self.persistence_pending() {
                    PERSISTENCE_BANNER_BAND
                } else {
                    0.0
                },
            c.x1 - 12.0,
            self.rect.y1 - 12.0,
        );
        self.persistence_retry_rect = if self.persistence_pending() {
            let banner_top = self.rect.y0 + HEADER_H + BANNER_TOP_GAP;
            Rect::new(
                c.x1 - OUTER_PAD - BANNER_RETRY_W,
                banner_top,
                c.x1 - OUTER_PAD,
                banner_top + BANNER_H,
            )
        } else {
            Rect::ZERO
        };
        self.list.set_viewport(self.list_rect);

        self.add_panel.layout(self.rect);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Window background.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        self.draw_header(scene, transform, text_renderer, theme);
        if self.persistence_pending() {
            // The banner stops an `s2` short of Retry (they used to share an
            // edge) and is the shared `Banner` face (HIG §8): semantic tint,
            // edge, leading bar and icon from the toolkit, with the app's
            // Retry beside it.
            let banner = Rect::new(
                self.content_rect.x0 + OUTER_PAD,
                self.persistence_retry_rect.y0,
                self.persistence_retry_rect.x0 - BANNER_RETRY_GAP,
                self.persistence_retry_rect.y1,
            );
            let banner_message = if self.rect.width() < 480.0 {
                "Vault not saved — retry"
            } else {
                self.persistence_message
                    .as_deref()
                    .unwrap_or("Local vault changes aren't saved — restore access, then retry.")
            };
            Banner::paint(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                banner,
                &BannerContent {
                    kind: BannerKind::Error,
                    icon: BannerKind::Error.default_icon(),
                    title: banner_message,
                    message: None,
                    dismissible: false,
                },
                self.last_mouse,
            );
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(self.persistence_retry_rect, "Retry")
                    .primary()
                    .text_size(13.0)
                    .weight(TextWeight::Semibold)
                    .radius(f64::from(theme.tokens.radii.sm)),
            );
        }
        // Hairline under the header: derived against the base it is drawn on
        // (HIG §9.5) — a half-alpha `border_standard` vanished on dark.
        let line = Rect::new(
            self.rect.x0,
            self.header_rect.y1 - 1.0,
            self.rect.x1,
            self.header_rect.y1,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            ui::surface::edge_on(theme.tokens.colors.surfaces.base),
            None,
            &line,
        );

        self.list
            .draw(scene, transform, text_renderer, theme, presence);
        self.draw_toast(scene, transform, text_renderer, theme);
        self.add_panel.draw(scene, transform, text_renderer, theme);
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
        // Root rect is the whole window — always paint.
        self.draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }

        // Modal owns all input while open.
        if self.add_panel.is_open() {
            self.add_panel.handle_event(event, ctx);
            if let Some(ev) = self.add_panel.take_event() {
                self.apply_add_panel_event(ev);
            }
            return;
        }

        if self.handle_global_shortcut(event, ctx.modifiers.state(), ctx.handled) {
            return;
        }

        if self.handle_add_and_search_focus_click(event, ctx.handled) {
            return;
        }

        // Route to the search field (caret placement + typing) when it
        // is focused or being clicked.
        let mut throwaway = None;
        let route_to_search = self.search.is_focused()
            || matches!(
                event,
                WindowEvent::MouseInput { .. } if self.search_rect.contains(self.last_mouse)
            )
            || matches!(event, WindowEvent::CursorMoved { .. });
        if route_to_search {
            self.search.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    focus_target: &mut throwaway,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
            if self.search.take_dirty().is_some() {
                // A query change reshapes the visible list but not the vault,
                // so only the list needs refreshing — no CRDT rewrite.
                self.list_dirty = true;
            }
        }

        // Route to the list (hover / click-to-copy / reorder / scroll).
        self.list.handle_event(
            event,
            &mut ui::widget::EventCtx {
                focus_target: &mut throwaway,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        let events = self.list.take_events();
        for ev in events {
            self.apply_list_event(ev);
        }
        // Keep the list's selection mirror current.
        self.list.set_selected(self.selected);
    }

    fn name(&self) -> &'static str {
        "AuthenticatorApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the root publishes one coherent semantic tree"
    )]
    fn accessibility(&self) -> Option<AccessNode> {
        if self.add_panel.is_open() {
            return Some(
                AccessNode::new(Role::Window)
                    .with_name("Authenticator")
                    .with_bounds(self.rect)
                    .with_children(vec![self.add_panel.accessibility()]),
            );
        }
        let visible = self.vault.search(self.search.value());
        let total = visible.len();
        let controls = vec![
            AccessNode::new(Role::Button)
                .with_name("Add account")
                .with_description("Add an authenticator account")
                .with_state(AccessState::new().focusable(true))
                .with_bounds(self.add_btn_rect)
                .with_action_target("totp.add"),
            AccessNode::new(Role::Button)
                .with_name(
                    if self.selected.is_some_and(|id| self.has_local_secret(id)) {
                        "Edit selected account"
                    } else {
                        "Set up selected account"
                    },
                )
                .with_description(
                    if self.selected.is_some_and(|id| self.has_local_secret(id)) {
                        "Edit the selected account; shortcut Control+E"
                    } else {
                        "Set up the selected account's local secret; shortcut Control+E"
                    },
                )
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .disabled(self.selected.is_none()),
                )
                .with_bounds(self.edit_btn_rect)
                .with_action_target("totp.edit"),
            AccessNode::new(Role::TextInput)
                .with_name("Search accounts")
                .with_value(self.search.value())
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.search.is_focused()),
                )
                .with_bounds(self.search_rect)
                .with_action_target("totp.search"),
        ];
        let mut children = vec![
            AccessNode::new(Role::Toolbar)
                .with_name("Authenticator actions")
                .with_children(controls),
        ];
        if self.persistence_pending() {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name("Retry saving secrets")
                    .with_description(
                        self.persistence_message
                            .as_deref()
                            .unwrap_or("Retry saving this device's secrets"),
                    )
                    .with_state(AccessState::new().focusable(true))
                    .with_bounds(self.persistence_retry_rect)
                    .with_action_target("totp.retry-persist"),
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
                            "Set up this account's secret on this device before copying a code"
                        })
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .disabled(!account.secret_is_valid()),
                        )
                        .with_action_target(format!("totp.copy:{id}")),
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
                                "Paste or scan the local secret without sharing it with collaborators",
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
                    "No matching accounts".to_string()
                } else {
                    format!("{total} visible accounts. Arrow keys change the selected account.")
                })
                .with_bounds(self.list_rect)
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
            AccessNode::new(Role::Window)
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
        if self.add_panel.is_open() {
            let handled = self.add_panel.perform_a11y_target(target, action);
            if let Some(event) = self.add_panel.take_event() {
                self.apply_add_panel_event(event);
            }
            return handled;
        }
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
mod tests {
    use super::*;
    use crate::model::demo_accounts;
    use crate::test_events::{char_press, ctrl, cursor_at, left_press, named_press, no_mods};
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

    /// Run the next frame at a concrete window size.
    fn layout_at(app: &mut Framed<AuthenticatorApp>, w: u32, h: u32) {
        Framed::harness_mut(app)
            .relayout_at((w, h))
            .expect("layout settles");
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
                &mut state.doc,
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
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn accessibility_names_accounts_and_protects_the_modal() {
        // Case `accessibility_tree_names_accounts_without_exposing_codes`.
        {
            let app = framed(app_with_state(), 640, 700);
            let tree = app.accessibility().expect("root has semantic window");
            assert_eq!(tree.role, Role::Window);
            assert_eq!(tree.name.as_deref(), Some("Authenticator"));
            let list = tree
                .children
                .iter()
                .find(|node| node.role == Role::List)
                .expect("named account list");
            assert!(!list.children.is_empty(), "demo accounts are exported");
            let account = &list.children[0];
            assert_eq!(account.role, Role::ListItem);
            assert!(account.name.as_deref().is_some_and(|name| !name.is_empty()));
            assert!(
                account.value.is_none(),
                "a TOTP credential is never a value"
            );
            assert!(account.children.iter().any(|node| {
                node.name
                    .as_deref()
                    .is_some_and(|name| name.starts_with("Copy code for"))
            }));
            assert!(
                tree.children.iter().all(|node| node.semantic_key.is_none()),
                "root regions are consistently position-addressed"
            );
            let toolbar = tree
                .children
                .iter()
                .find(|node| node.role == Role::Toolbar)
                .expect("authenticator actions toolbar");
            assert!(
                toolbar
                    .children
                    .iter()
                    .all(|node| node.semantic_key.is_some())
            );
        }

        // Case `modal_accessibility_is_modal_protected_and_routes_text_actions`.
        {
            let mut framed = framed(app_with_state(), 640, 700);
            let (app, state) = Framed::split(&mut framed);
            app.add_panel.open();
            app.add_panel.layout(app.rect);
            let tree = app.accessibility().expect("semantic root");
            assert_eq!(tree.children.len(), 1, "background controls are inert");
            let dialog = &tree.children[0];
            assert_eq!(dialog.role, Role::Dialog);
            assert!(dialog.state.modal);
            let secret = dialog
                .children
                .iter()
                .find(|node| node.name.as_deref() == Some("Secret key"))
                .expect("protected secret field");
            assert!(secret.state.protected);
            assert!(secret.value.is_none());
            assert!(app.perform_semantic_action(
                "totp.dialog.issuer",
                &A11yAction::SetValue("Acme".into()),
                &mut state.doc,
                &state.root
            ));
            assert_eq!(app.add_panel.field_snapshot().0, "Acme");
            assert!(app.perform_semantic_action(
                "totp.dialog.save",
                &A11yAction::Activate,
                &mut state.doc,
                &state.root
            ));
            let dialog = &app
                .accessibility()
                .expect("still open on validation failure")
                .children[0];
            assert!(
                dialog
                    .children
                    .iter()
                    .any(|node| node.live == Some(Politeness::Assertive))
            );
        }

        // Case `modal_a11y_exposes_advanced_algorithm_and_digit_choices`.
        {
            let mut framed = framed(app_with_state(), 640, 700);
            let (app, state) = Framed::split(&mut framed);
            app.add_panel.open();
            app.add_panel.layout(app.rect);
            for target in [
                "totp.dialog.advanced",
                "totp.dialog.algorithm:1",
                "totp.dialog.digits:8",
            ] {
                assert!(app.perform_semantic_action(
                    target,
                    &A11yAction::Activate,
                    &mut state.doc,
                    &state.root
                ));
            }
            let dialog = &app.accessibility().expect("dialog tree").children[0];
            assert!(
                dialog
                    .children
                    .iter()
                    .any(|node| { node.name.as_deref() == Some("SHA256") && node.state.selected })
            );
            assert!(
                dialog.children.iter().any(|node| {
                    node.name.as_deref() == Some("8 digits") && node.state.selected
                })
            );
        }
    }

    /// Narrow, missing-key and resized layouts keep targets inside one centred column.
    #[test]
    fn narrow_and_resized_layouts_keep_one_readable_column() {
        // Case `narrow_header_keeps_the_add_target_inside_the_window`.
        {
            let app = framed(app_with_state(), 320, 640);
            assert!(app.add_btn_rect.width() <= COMPACT_ADD_BTN_W);
            assert!(app.add_btn_rect.x0 >= app.rect.x0);
            assert!(app.add_btn_rect.x1 <= app.rect.x1);
        }

        // Case `missing_key_narrow_root_starts_at_top_with_an_accessible_first_row`.
        {
            let app = framed(app_with_seeds(1), 390, 844);
            assert!(
                app.list.scroll_offset().abs() <= f64::EPSILON,
                "a fresh view starts at the top"
            );
            let tree = app.accessibility().expect("narrow root has semantics");
            let list = tree
                .children
                .iter()
                .find(|node| node.role == Role::List)
                .expect("account list stays exposed at the initial scroll position");
            assert!(
                list.children
                    .first()
                    .is_some_and(|row| row.bounds.is_some()),
                "the first shared account is both visible and reachable to AT"
            );
        }

        // Case `app_reflows_from_the_current_size_not_the_first_frame`.
        {
            // The same instance must re-derive its layout from the live
            // window width on every pass, wide → narrow → wide.
            let mut app = framed(app_with_state(), 1280, 720);
            let wide_list_w = app.list_rect.width();
            let wide_btn_x0 = app.add_btn_rect.x0;
            assert!(
                (wide_list_w - (READABLE_CONTENT_W - 24.0)).abs() < 0.5,
                "wide window gives the readable-column list, got {wide_list_w}"
            );

            layout_at(&mut app, 560, 720);
            assert!(
                app.list_rect.width() < wide_list_w,
                "list reflows narrower on resize"
            );
            assert!(
                app.add_btn_rect.x0 < wide_btn_x0,
                "the add button re-anchors to the narrower right edge"
            );
            assert!(
                app.add_btn_rect.x1 <= app.rect.x1,
                "add button stays inside the window"
            );

            layout_at(&mut app, 1280, 720);
            assert!(
                (app.list_rect.width() - wide_list_w).abs() < 0.5,
                "list widens back to the wide layout"
            );
        }

        // Case `header_search_and_list_share_one_centred_readable_column`.
        {
            let mut app = framed(app_with_state(), 1920, 1080);
            let c = app.content_rect;
            assert!((c.width() - READABLE_CONTENT_W).abs() < 0.5);
            assert!(
                (c.center().x - app.rect.center().x).abs() < 0.5,
                "the column is centred in the window"
            );
            assert!((app.add_btn_rect.x1 - (c.x1 - OUTER_PAD)).abs() < 0.5);
            assert!((app.search_rect.x0 - (c.x0 + OUTER_PAD)).abs() < 0.5);
            assert!((app.search_rect.x1 - (c.x1 - OUTER_PAD)).abs() < 0.5);
            assert!((app.list_rect.x1 - (c.x1 - 12.0)).abs() < 0.5);
            assert!(app.edit_btn_rect.x1 < app.add_btn_rect.x0);

            layout_at(&mut app, 640, 800);
            assert!((app.content_rect.width() - 640.0).abs() < 0.5);
            assert!((app.search_rect.x1 - (640.0 - OUTER_PAD)).abs() < 0.5);
        }
    }

    /// Search filtering, including the contextual empty message.
    #[test]
    fn search_filters_the_visible_accounts() {
        let (mut app, _state) = app_with_state();
        app.search.set_text("github");
        app.refresh_list();
        assert_eq!(app.list.len(), 1);
        assert_eq!(app.list.account_at(0).unwrap().issuer, "GitHub");

        app.search.set_text("zzz-no-such-service");
        app.refresh_list();
        assert!(app.list.is_empty(), "no demo account matches the query");
        let msg = app.list.empty_message();
        assert!(
            msg.contains("zzz-no-such-service"),
            "the empty state names the unmatched query, got: {msg}"
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
            "the durable banner replaces a duplicate toast"
        );
        assert!(app.persistence_retry_rect.x0 >= app.rect.x0);
        assert!(app.persistence_retry_rect.x1 <= app.rect.x1);
        assert!(app.persistence_retry_rect.y0 >= app.header_rect.y1);
        assert!(
            app.list_rect.y0 > app.header_rect.y1,
            "banner reserves list space"
        );

        let tree = app
            .accessibility()
            .expect("persistence state has semantics");
        let toolbar = tree
            .children
            .iter()
            .find(|node| node.role == Role::Toolbar)
            .expect("toolbar remains available");
        assert!(toolbar.children.iter().any(|node| {
            node.name.as_deref() == Some("Edit selected account")
                && node.semantic_key.as_deref() == Some("totp.edit")
        }));
        let retry = tree
            .children
            .iter()
            .find(|node| node.semantic_key.as_deref() == Some("totp.retry-persist"))
            .expect("retry is a separate semantic control");
        assert_eq!(
            retry.bounds.map(ui::a11y::AccessBounds::to_rect),
            Some(app.persistence_retry_rect)
        );
        assert!(retry.state.focusable);

        assert!(app.perform_semantic_action(
            "totp.retry-persist",
            &A11yAction::Activate,
            &mut state.doc,
            &state.root,
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

            let (mut app, mut state) = app_with_state();
            app.add_panel.open();

            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(img));
            ui::host::scoped(&services, || app.request_qr_via_portal());
            app.sync(&mut state.doc, &state.root);

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
            let (mut app, mut state) = app_with_state();
            app.add_panel.open();
            let before = app.add_panel.field_snapshot();
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Cancelled);
            ui::host::scoped(&services, || app.request_qr_via_portal());
            app.sync(&mut state.doc, &state.root);
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

    /// Add button, Ctrl+N, slash/Ctrl+F, Escape and the modal's gating of global search.
    #[test]
    fn shortcuts_and_clicks_open_the_modal_and_focus_search() {
        // Case `clicking_the_add_button_opens_the_modal`.
        {
            let mut app = framed(app_with_state(), 1000, 800);
            assert!(!app.add_panel.is_open(), "starts on the list, modal closed");
            let btn = app.add_btn_rect.center();
            dispatch(&mut app, &cursor_at(btn), no_mods());
            dispatch(&mut app, &left_press(), no_mods());
            assert!(
                app.add_panel.is_open(),
                "clicking the + Add account button opens the add modal"
            );
        }

        // Case `ctrl_n_opens_the_add_modal`.
        {
            let mut app = framed(app_with_state(), 1000, 800);
            let handled = dispatch(&mut app, &char_press("n"), ctrl());
            assert!(handled, "Ctrl+N is consumed");
            assert!(app.add_panel.is_open(), "Ctrl+N opens the add modal");
        }

        // Case `slash_and_ctrl_f_focus_the_search_field`.
        {
            let mut app = framed(app_with_state(), 1000, 800);
            assert!(!app.search.is_focused());
            dispatch(&mut app, &char_press("/"), no_mods());
            assert!(app.search.is_focused(), "'/' focuses the search field");
            app.search.set_focus(false);
            dispatch(&mut app, &char_press("f"), ctrl());
            assert!(app.search.is_focused(), "Ctrl+F focuses the search field");
        }

        // Case `escape_clears_a_nonempty_search_query`.
        {
            let mut app = framed(app_with_state(), 1000, 800);
            app.search.set_focus(true);
            app.search.set_text("github");
            let handled = dispatch(&mut app, &named_press(NamedKey::Escape), no_mods());
            assert!(handled, "Escape is consumed while clearing the query");
            assert!(
                app.search.value().is_empty(),
                "Escape clears the search text"
            );
            assert!(!app.search.is_focused(), "Escape also drops search focus");
        }

        // Case `open_modal_gates_the_global_search_shortcut`.
        {
            // While the add modal is open it owns all input: a key that would be a
            // global shortcut on the list must not focus the underlying search.
            let mut app = framed(app_with_state(), 1000, 800);
            app.add_panel.open();
            let rect = app.rect;
            app.add_panel.layout(rect);
            assert!(!app.search.is_focused());
            dispatch(&mut app, &char_press("/"), no_mods());
            assert!(
                !app.search.is_focused(),
                "the open modal gates the '/' shortcut from reaching the list"
            );
            assert!(app.add_panel.is_open(), "the modal stays open");
        }
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
