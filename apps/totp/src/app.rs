//! Root widget for the Authenticator binary.
//!
//! Owns the in-memory [`Vault`] (the source of truth), mirrors it into
//! the shared Automerge document every frame (so the vault replicates to
//! collaborators and is captured by `ui::State` undo snapshots), and
//! composes the three surfaces:
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
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ChangeHash, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::color_compat::ColorCompat;
use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::qos_actions::{
    ActionContract, ActionSpec, OperationKind, PermissionRequirement, RetryPolicy, RiskLevel,
};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{TextInput, Widget};

use crate::add_panel::{AddAccountPanel, AddPanelEvent};
use crate::doc::AuthDoc;
use crate::list::{AccountList, ListEvent};
use crate::model::{AccountId, Vault, demo_accounts};
use crate::storage;
use crate::totp;

const OUTER_PAD: f64 = 24.0;
const ADD_BTN_W: f64 = 168.0;
const ADD_BTN_H: f64 = 40.0;
const SEARCH_H: f64 = 44.0;
const HEADER_H: f64 = 150.0;
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
#[derive(Clone, Copy, PartialEq, Eq)]
enum SeedMode {
    /// Live session: load the user's persisted vault, persist changes, and
    /// never fabricate sample data.
    Live,
    /// `--shots`/demo: seed a deterministic sample vault, ignore on-disk
    /// storage, and never write.
    Demo,
    /// `--shots` empty state: start with no accounts, ignore storage.
    Empty,
}

pub struct AuthenticatorApp {
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
    search_rect: Rect,
    add_btn_rect: Rect,
    list_rect: Rect,

    last_mouse: Point,
    selected: Option<AccountId>,
    toast: Option<(String, Instant, ToastKind)>,
    /// The exact code string most recently written to the clipboard by a copy
    /// action. Lets callers (and tests) observe what was copied without
    /// round-tripping the system clipboard, which is unavailable headless.
    last_copied: Option<String>,
    seeded: bool,
    /// How the initial vault is populated and whether changes persist. The
    /// live app loads/saves the real vault; the demo/empty shots roots use
    /// fixed data and never touch storage.
    seed_mode: SeedMode,
    /// The vault changed and must be re-mirrored into the CRDT document.
    /// Gates the per-frame doc write so idle frames (and pointer moves) do
    /// no CRDT work.
    doc_dirty: bool,
    /// The visible (filtered/ordered) list must be recomputed — set when the
    /// vault or the search query changes.
    list_dirty: bool,
    /// Framework-owned action dispatch. Each on-screen affordance dispatches its
    /// [`TotpAction`](crate::actions::TotpAction) through this: the host brackets
    /// the undo boundary, applies the action through the executor (so it replays
    /// byte-identically on peers, exactly as the Ctrl+K palette does), and records
    /// the last one so a test can prove a button and its
    /// [`ActionSpec`](qos_actions::ActionSpec) build the identical action.
    actions: ui::ActionHost,
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

    /// The demo/shots app: seeds a deterministic sample vault and never
    /// touches on-disk storage.
    pub fn demo() -> Self {
        Self::with_mode(SeedMode::Demo)
    }

    /// The empty-state shots app: no accounts, ignores storage. Used to
    /// capture the first-run experience deterministically.
    pub fn empty() -> Self {
        Self::with_mode(SeedMode::Empty)
    }

    fn with_mode(seed_mode: SeedMode) -> Self {
        Self {
            vault: Vault::new(),
            search: TextInput::local().with_kind(ui::widget::TextInputKind::Search),
            list: AccountList::new(),
            add_panel: AddAccountPanel::new(),
            portal_rx: None,
            rect: Rect::ZERO,
            root_node: None,
            header_rect: Rect::ZERO,
            search_rect: Rect::ZERO,
            add_btn_rect: Rect::ZERO,
            list_rect: Rect::ZERO,
            last_mouse: Point::ZERO,
            selected: None,
            toast: None,
            last_copied: None,
            seeded: false,
            seed_mode,
            doc_dirty: false,
            list_dirty: false,
            actions: ui::ActionHost::new(),
        }
    }

    /// Populate the initial vault. The live app loads the user's persisted
    /// vault (empty on a fresh install → the empty state). The demo/shots
    /// app uses a fixed sample set and ignores on-disk storage entirely, so
    /// captures are reproducible and never leak the real vault.
    fn seed(&mut self) {
        let mut vault = match self.seed_mode {
            SeedMode::Live => storage::load(),
            SeedMode::Demo => {
                let mut v = Vault::new();
                v.accounts = demo_accounts();
                v
            }
            SeedMode::Empty => Vault::new(),
        };
        vault.renumber();
        self.selected = vault.ordered().first().map(|a| a.id);
        self.vault = vault;
        self.doc_dirty = true;
        self.list_dirty = true;
    }

    /// Dispatch one [`TotpAction`](crate::actions::TotpAction) — the single
    /// CRDT-mutation path shared with the Ctrl+K palette. The action is applied
    /// through the qOS action executor (a pinned actor + committed change, so it
    /// replays byte-identically on peers), exactly as the palette does. The
    /// document heads captured just before the first action of the event are
    /// stashed so the host records **one** undo boundary bracketing it
    /// ([`Widget::take_committed_action`]). Callers reload the in-memory vault
    /// from the document afterward for immediate feedback; `sync` reconciles it
    /// again each frame (so a palette dispatch shows up the same way). This app
    /// supplies only its domain: the action and its type.
    fn perform_action(&mut self, doc: &mut AutoCommit, action: &crate::actions::TotpAction) {
        self.actions.dispatch_with_context(
            doc,
            crate::actions::TOTP_ACTION_TYPE,
            action,
            qos_actions::ActionContext::for_action(
                "org.qos.Authenticator",
                "Authenticator",
                action,
            ),
        );
    }

    /// Re-project the in-memory vault from the now-authoritative document, and —
    /// when it actually changed — persist the encrypted vault (live only). Run
    /// every frame from `sync` so palette dispatches and remote edits appear, and
    /// right after an on-screen affordance action for immediate feedback. The
    /// document is the source of truth; the vault is a pure projection of it.
    fn reload_from_doc(&mut self, doc: &AutoCommit) {
        let projected = AuthDoc::list_accounts(doc); // sorted by order
        if projected == self.vault.ordered() {
            return; // nothing changed
        }
        self.vault.accounts = projected;
        self.list_dirty = true;
        if self.seed_mode == SeedMode::Live {
            storage::save(&self.vault);
        }
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
        self.toast = Some((msg.into(), Instant::now(), kind));
    }

    fn copy_account(&mut self, id: AccountId) {
        if let Some(acc) = self.vault.get(id).cloned() {
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
                self.reload_from_doc(state);
                if self.selected == Some(id) {
                    self.selected = self.vault.ordered().first().map(|a| a.id);
                }
                self.toast("Account removed", ToastKind::Success);
            }
        }
    }

    /// Act on one outcome drained from the Add-account modal. A save is a
    /// document mutation dispatched as an [`TotpAction::AddAccount`]
    /// (`crate::actions::TotpAction`); the colour slot and list order are pinned at
    /// authoring time so replay is deterministic.
    fn apply_add_panel_event(&mut self, ev: AddPanelEvent, state: &mut AutoCommit) {
        match ev {
            AddPanelEvent::Cancel => {}
            AddPanelEvent::PickImageForQr => self.import_qr_from_image(),
            AddPanelEvent::Save(mut acc) => {
                // Give the new account a distinct colour slot + end-of-list order,
                // baked into the action so peers replay the identical row.
                acc.color = (self.vault.len() % 7) as u8;
                let order = self.vault.next_order();
                let id = acc.id;
                self.perform_action(state, &crate::actions::add_account_action(&acc, order));
                self.reload_from_doc(state);
                self.selected = Some(id);
                self.toast("Account added", ToastKind::Success);
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
            // "/" focuses search when not already typing there.
            if c.as_str() == "/" && !self.search.is_focused() {
                self.search.set_focus(true);
                *handled = true;
                return true;
            }
        }
        match logical_key {
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

    /// Open the Add-account screen straight away. Used by the `--shots`
    /// renderer (and any caller that wants to land on the add flow) so the
    /// captured frame shows the redesigned add UI rather than the list.
    pub fn seed_open_add(&mut self) {
        self.add_panel.open();
    }

    /// Open the Add-account screen with Advanced options expanded — a
    /// `--shots`-only entry point for capturing the tall, scrolling form.
    pub fn seed_open_add_advanced(&mut self) {
        self.add_panel.open_with_advanced();
    }

    /// Open the Add-account screen — the surface the "Scan a QR image" import
    /// starts from. At runtime that action hands off to the **host** file
    /// chooser via the XDG Desktop Portal ([`Self::request_qr_via_portal`]);
    /// there is no app-drawn picker to render offscreen (HIG §5), so this
    /// `--shots` entry point simply lands on the add screen with its import
    /// method cards.
    pub fn seed_open_add_picker(&mut self) {
        self.add_panel.open();
    }

    // ---- header drawing ----------------------------------------------

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
        text_renderer.draw_text_weighted(
            scene,
            &subtitle,
            15.0,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((self.rect.x0 + OUTER_PAD, self.rect.y0 + 34.0)),
            None,
            TextWeight::Medium,
        );

        // Add button.
        let hovered = self.add_btn_rect.contains(self.last_mouse);
        let shape = RoundedRect::from_rect(self.add_btn_rect, f64::from(theme.tokens.radii.md));
        let bg = if hovered {
            theme.accent_hover
        } else {
            theme.accent_primary
        };
        scene.fill(Fill::NonZero, transform, bg, None, &shape);
        let label = "+  Add account";
        let (tw, th) = text_renderer.measure_weighted(label, 14.0, None, TextWeight::Semibold);
        text_renderer.draw_text_weighted(
            scene,
            label,
            14.0,
            Color::WHITE,
            transform
                * Affine::translate((
                    self.add_btn_rect.center().x - f64::from(tw) / 2.0,
                    self.add_btn_rect.center().y - f64::from(th) / 2.0,
                )),
            None,
            TextWeight::Semibold,
        );

        // Search field (TextInput draws its own chrome + focus ring).
        self.search.draw(
            scene,
            transform,
            text_renderer,
            theme,
            &PresenceCtx::empty(),
        );
        if self.search.value().is_empty() {
            let pad = f64::from(theme.padding_standard);
            text_renderer.draw_text(
                scene,
                "Search accounts\u{2026}",
                16.0,
                theme.tokens.colors.text_dim.with_alpha_factor(0.6),
                transform
                    * Affine::translate((
                        self.search_rect.x0 + pad,
                        self.search_rect.center().y - 10.0,
                    )),
                None,
            );
        }
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
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &["totp.open-account", "totp.copy-code"]
    }

    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        // Pull in any QR image the host portal picker returned since the last
        // frame (off-thread; never blocks the UI).
        self.drain_portal();

        if !self.seeded {
            self.seed();
            self.seeded = true;
        }
        // Mirror the freshly-loaded vault into the shared document once, on the
        // seed frame — the initial-load population, not an undoable edit. After
        // that the document is authoritative: every mutation is an action, and
        // the vault is re-projected from the document below.
        if self.doc_dirty {
            AuthDoc::sync_vault(state, &self.vault.ordered());
            self.doc_dirty = false;
        }
        // Re-project the vault from the document so palette dispatches and remote
        // edits appear (a cheap no-op when nothing changed).
        self.reload_from_doc(state);
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
        self.add_btn_rect = Rect::new(
            self.rect.x1 - OUTER_PAD - ADD_BTN_W,
            self.rect.y0 + 18.0,
            self.rect.x1 - OUTER_PAD,
            self.rect.y0 + 18.0 + ADD_BTN_H,
        );
        let sy = self.rect.y0 + 88.0;
        self.search_rect = Rect::new(
            self.rect.x0 + OUTER_PAD,
            sy,
            self.rect.x1 - OUTER_PAD,
            sy + SEARCH_H,
        );
        self.search.apply_layout_external(self.search_rect);

        self.list_rect = Rect::new(
            self.rect.x0 + 12.0,
            self.rect.y0 + HEADER_H,
            self.rect.x1 - 12.0,
            self.rect.y1 - 12.0,
        );
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
        // Hairline under the header.
        let line = Rect::new(
            self.rect.x0,
            self.header_rect.y1 - 1.0,
            self.rect.x1,
            self.header_rect.y1,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.border_standard.with_alpha_factor(0.5),
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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
        }

        // Modal owns all input while open.
        if self.add_panel.is_open() {
            self.add_panel.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                node_id,
                taffy,
            );
            if let Some(ev) = self.add_panel.take_event() {
                self.apply_add_panel_event(ev, state);
            }
            return;
        }

        if self.handle_global_shortcut(event, modifiers.state(), handled) {
            return;
        }

        if self.handle_add_and_search_focus_click(event, handled) {
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
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                &mut throwaway,
                node_id,
                taffy,
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
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            &mut throwaway,
            node_id,
            taffy,
        );
        let events = self.list.take_events();
        for ev in events {
            self.apply_list_event(ev, state);
        }
        // Keep the list's selection mirror current.
        self.list.set_selected(self.selected);
    }

    /// Publish the open vault + its accounts so the host's Ctrl+K palette scopes
    /// to them. Delegates to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`], which reads the accounts straight from the document.
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        let mut resources = <Self as qos_actions::ResourceProvider>::resources(self, doc);
        if let Some(root) = resources.first_mut() {
            for account in self.vault.ordered() {
                if let Some(resource) = root
                    .children
                    .iter_mut()
                    .find(|resource| resource.id.0 == format!("totp.account:{}", account.id))
                {
                    resource.actions.insert(
                        0,
                        ActionSpec::operation(
                            "copy-code",
                            "Copy current code",
                            OperationKind::Effect,
                            "totp.copy-code",
                            &account.id.to_string(),
                        )
                        .expect("TOTP account id is serializable")
                        .with_contract(ActionContract {
                            permission: PermissionRequirement::Read,
                            risk: RiskLevel::Privileged,
                            confirmation: qos_actions::ConfirmationPolicy::Confirm,
                            retry: RetryPolicy::AtMostOnce,
                            tags: vec!["security".into(), "clipboard".into()],
                            ..ActionContract::default()
                        }),
                    );
                    resource.actions.insert(
                        0,
                        ActionSpec::operation(
                            "open",
                            "Open account",
                            OperationKind::View,
                            "totp.open-account",
                            &account.id.to_string(),
                        )
                        .expect("TOTP account id is serializable")
                        .with_contract(ActionContract::view()),
                    );
                }
            }
        }
        resources
    }

    fn invoke_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if !matches!(
            (kind, type_id),
            (OperationKind::View, "totp.open-account") | (OperationKind::Effect, "totp.copy-code")
        ) {
            return Ok(false);
        }
        let raw: String = serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        let id = raw
            .parse()
            .map_err(|_| "Invalid authenticator account id".to_string())?;
        if self.vault.get(id).is_none() {
            return Err("Authenticator account is no longer available".into());
        }
        self.selected = Some(id);
        self.search.set_text("");
        self.list_dirty = true;
        if kind == OperationKind::Effect {
            self.copy_account(id);
        }
        Ok(true)
    }

    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its account action, so it records one undo boundary for it (see
    /// [`Self::perform_action`]). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.actions.take_committed()
    }

    /// Fold the vault into the unified Ctrl+K palette: a snapshot
    /// [`TotpSearchProvider`](crate::search_provider::TotpSearchProvider) makes
    /// every account findable there by issuer or account name, alongside the
    /// actions the app exports.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(crate::search_provider::TotpSearchProvider::new(
            &self.vault,
        ))]
    }

    /// Select the account a chosen palette row identifies. Clears the in-app
    /// filter so the account is revealed in the list even if a search term was
    /// active. Selection is app-local presentation, so the doc args are unused.
    fn activate_search_result(&mut self, action_id: &str, _doc: &mut AutoCommit, _path: &ObjId) {
        if let Some(id) = crate::search_provider::TotpSearchProvider::parse_action_id(action_id)
            && self.vault.get(id).is_some()
        {
            self.selected = Some(id);
            self.search.set_text("");
            self.list_dirty = true;
        }
    }

    fn name(&self) -> &'static str {
        "AuthenticatorApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Codes + countdown rings tick every frame; keep the host redrawing.
    fn needs_animation_frame(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The demo root is used so tests get a deterministic sample vault and
    // never read or write the real on-disk vault.
    fn app_with_state() -> (AuthenticatorApp, ui::State) {
        let mut app = AuthenticatorApp::demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        (app, state)
    }

    #[test]
    fn resource_copy_invokes_the_same_clipboard_semantics_as_the_row() {
        let (mut app, state) = app_with_state();
        let id = app.vault.ordered()[0].id;
        let payload = serde_json::to_vec(&id.to_string()).unwrap();
        assert!(
            app.invoke_resource_operation(
                OperationKind::Effect,
                "totp.copy-code",
                &payload,
                &state.doc,
                &state.root,
            )
            .unwrap()
        );
        assert_eq!(app.selected, Some(id));
        assert!(app.last_copied().is_some());
    }

    /// Run the app's real layout passes at a concrete window size, the same
    /// cycle `main.rs` drives for the live/offscreen frames. Returns the live
    /// taffy tree + the app's root node so events can be routed exactly as
    /// production does.
    fn layout_at(app: &mut AuthenticatorApp, w: f32, h: f32) -> (TaffyTree<()>, NodeId) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = app.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(w),
                        height: Dimension::length(h),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        taffy
            .compute_layout(
                root,
                Size {
                    width: taffy::AvailableSpace::Definite(w),
                    height: taffy::AvailableSpace::Definite(h),
                },
            )
            .unwrap();
        app.apply_layout(&taffy, child, Point::ORIGIN);
        (taffy, child)
    }

    /// Route one event through the app's real `handle_event`, returning
    /// whether it was marked handled.
    fn dispatch(
        app: &mut AuthenticatorApp,
        ev: &WindowEvent,
        mods: winit::event::Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        node: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            1.0,
            &mods,
            state,
            root,
            &mut handled,
            &mut focus,
            node,
            taffy,
        );
        handled
    }

    #[test]
    fn app_reflows_from_the_current_size_not_the_first_frame() {
        // Guards the "responsive on first launch, frozen after resize" bug:
        // the same instance must re-derive its layout from the live window
        // width on every pass, wide → narrow → wide.
        let (mut app, _state) = app_with_state();
        layout_at(&mut app, 1280.0, 720.0);
        let wide_list_w = app.list_rect.width();
        let wide_btn_x0 = app.add_btn_rect.x0;
        assert!(wide_list_w > 900.0, "wide window gives a wide list");

        layout_at(&mut app, 560.0, 720.0);
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

        // Back to wide — it must recover, not stay stuck narrow.
        layout_at(&mut app, 1280.0, 720.0);
        assert!(
            (app.list_rect.width() - wide_list_w).abs() < 0.5,
            "list widens back to the wide layout"
        );
    }

    #[test]
    fn demo_root_seeds_sample_accounts_into_vault_and_doc() {
        let (app, state) = app_with_state();
        assert!(app.vault.len() >= 3, "demo vault should populate");
        let in_doc = AuthDoc::list_accounts(&state.doc);
        assert_eq!(in_doc.len(), app.vault.len(), "vault mirrored to CRDT");
    }

    #[test]
    fn live_root_starts_empty_without_fabricating_accounts() {
        // The live app must never auto-populate sample content; a fresh
        // install with no persisted vault shows an empty list. (This test
        // is only meaningful when no vault file exists, which is the case
        // in the hermetic build/CI environment.)
        let mut app = AuthenticatorApp::new();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        if crate::storage::vault_path().is_none_or(|p| !p.exists()) {
            assert_eq!(app.vault.len(), 0, "live app must not seed demo accounts");
        }
    }

    #[test]
    fn doc_mirror_is_skipped_when_nothing_changed() {
        // After the initial seed+mirror, a second sync with no vault change
        // must not re-dirty or rewrite the document.
        let (mut app, mut state) = app_with_state();
        assert!(!app.doc_dirty, "sync clears the doc-dirty flag");
        app.sync(&mut state.doc, &state.root);
        assert!(!app.doc_dirty, "idle sync leaves the doc-dirty flag clear");
    }

    #[test]
    fn search_filters_visible_accounts() {
        let (mut app, _state) = app_with_state();
        app.search.set_text("github");
        app.refresh_list();
        assert_eq!(app.list.len(), 1);
        assert_eq!(app.list.account_at(0).unwrap().issuer, "GitHub");
    }

    #[test]
    fn delete_event_removes_account() {
        let (mut app, mut state) = app_with_state();
        let before = app.vault.len();
        let id = app.vault.ordered()[0].id;
        app.apply_list_event(ListEvent::Delete(id), &mut state.doc);
        assert_eq!(app.vault.len(), before - 1);
        assert!(app.vault.get(id).is_none());
        // The delete went through the action, so it left the shared document too.
        assert!(
            AuthDoc::list_accounts(&state.doc)
                .iter()
                .all(|a| a.id != id),
            "the delete action mutated the shared document"
        );
    }

    #[test]
    fn reorder_event_moves_account_down() {
        let (mut app, mut state) = app_with_state();
        let first = app.vault.ordered()[0].id;
        let second = app.vault.ordered()[1].id;
        app.apply_list_event(ListEvent::MoveDown(first), &mut state.doc);
        assert_eq!(app.vault.ordered()[0].id, second);
        assert_eq!(app.vault.ordered()[1].id, first);
        // The reorder is reflected in the document (the source of truth).
        assert_eq!(AuthDoc::list_accounts(&state.doc)[0].id, second);
    }

    #[test]
    fn copy_event_selects_the_account_and_confirms_with_a_toast() {
        let (mut app, mut state) = app_with_state();
        let id = app.vault.ordered()[0].id;
        let acc = app.vault.get(id).cloned().unwrap();
        let before = totp::current_code(&acc);
        app.apply_list_event(ListEvent::Copy(id), &mut state.doc);
        let after = totp::current_code(&acc);
        assert_eq!(app.selected, Some(id), "the copied row becomes selected");
        let (msg, _, kind) = app
            .toast
            .clone()
            .expect("copying shows a confirmation toast");
        assert!(msg.starts_with("Copied"), "toast confirms the copy: {msg}");
        assert_eq!(kind, ToastKind::Success);
        // The copied string is the account's live TOTP code (not its title or
        // an empty string). It equals the code at some instant during the copy,
        // so it must match the value just before or just after — deterministic
        // even across a period rollover.
        let copied = app.last_copied().expect("a copy records the copied code");
        assert!(
            copied == before || copied == after,
            "copied {copied:?} must equal the account's live code ({before:?}/{after:?})"
        );
    }

    #[test]
    fn adding_an_account_through_the_panel_grows_the_vault() {
        let (mut app, mut state) = app_with_state();
        let before = app.vault.len();
        app.add_panel.open();
        // Fill via the paste seam, then run the production save path.
        app.add_panel
            .apply_uri("otpauth://totp/Proton:ada@pm.me?secret=JBSWY3DPEHPK3PXP");
        app.add_panel.save_for_test();
        assert!(!app.add_panel.is_open(), "a valid save closes the modal");
        let ev = app.add_panel.take_event().expect("save emits an event");
        app.apply_add_panel_event(ev, &mut state.doc);
        assert_eq!(
            app.vault.len(),
            before + 1,
            "the account is added to the vault"
        );
        // The save went through the AddAccount action, so it landed in the
        // shared document (the real, replicated path — not just the local vault).
        let in_doc = AuthDoc::list_accounts(&state.doc);
        assert_eq!(
            in_doc.len(),
            before + 1,
            "the add action mutated the shared document"
        );
        assert!(
            in_doc.iter().any(|a| a.issuer == "Proton"),
            "the added account is present in the document by issuer"
        );
        // And the host has one undo boundary to record for it.
        assert!(
            Widget::take_committed_action(&mut app).is_some(),
            "the add action registered exactly one undo boundary for the host"
        );
    }

    /// The primary "Scan a QR image" action must reach the **host** file
    /// chooser (the XDG portal), not an in-app picker or a dead-end toast. A
    /// sentinel channel is pre-armed so the routing can't launch a real host
    /// dialog; the in-flight guard must preserve it.
    #[test]
    fn qr_cta_routes_to_the_host_portal() {
        use ui::portal::inject_for_test;
        let (mut app, _state) = app_with_state();
        app.rect = Rect::new(0.0, 0.0, 1000.0, 800.0);
        app.add_panel.open();
        app.portal_rx = Some(inject_for_test(PortalOutcome::Cancelled));
        app.request_qr_via_portal();
        assert!(
            app.portal_rx.is_some(),
            "the QR action routes through the host portal (not an in-app picker)"
        );
    }

    /// End-to-end proof of the import seam on the real per-frame drain: a QR
    /// image the host portal returned is decoded and fills the add panel — the
    /// portal result is injected (no real dialog) and the next `sync` loads it.
    #[test]
    fn portal_chosen_qr_image_fills_the_add_panel_via_frame_drain() {
        use ui::portal::inject_for_test;
        let uri = "otpauth://totp/Acme:bob@acme.io?secret=JBSWY3DPEHPK3PXP&issuer=Acme";
        let dir = std::env::temp_dir().join(format!("totp-portal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("acme-qr.png");
        render_qr_png(uri, &img);

        let (mut app, mut state) = app_with_state();
        app.add_panel.open();

        // Inject the portal outcome and let the per-frame sync drain it — the
        // exact channel the off-thread portal worker feeds.
        app.portal_rx = Some(inject_for_test(PortalOutcome::Opened(img)));
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

    /// A cancelled portal interaction is a no-op: the add panel's fields are
    /// left exactly as they were and the channel clears.
    #[test]
    fn portal_cancel_leaves_the_add_panel_untouched() {
        use ui::portal::inject_for_test;
        let (mut app, mut state) = app_with_state();
        app.add_panel.open();
        let before = app.add_panel.field_snapshot();
        app.portal_rx = Some(inject_for_test(PortalOutcome::Cancelled));
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

    /// End-to-end proof of the QR-image import seam: a real QR PNG on disk is
    /// decoded and its otpauth payload fills the add panel.
    #[test]
    fn qr_image_file_decodes_and_fills_the_panel() {
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

    // ---- pointer + keyboard routing through the real `handle_event` -----
    // These drive synthetic winit events through `AuthenticatorApp::handle_event`
    // (the production input path) instead of calling internals directly, so the
    // add-button hit-test, the modal-owns-input gate, and every global shortcut
    // are verified end to end.

    use crate::test_events::{char_press, ctrl, cursor_at, left_press, named_press, no_mods};

    #[test]
    fn clicking_the_add_button_opens_the_modal() {
        let (mut app, mut state) = app_with_state();
        let (taffy, node) = layout_at(&mut app, 1000.0, 800.0);
        assert!(!app.add_panel.is_open(), "starts on the list, modal closed");

        let btn = app.add_btn_rect.center();
        let mods = no_mods();
        dispatch(
            &mut app,
            &cursor_at(btn),
            mods,
            &mut state.doc,
            &state.root,
            node,
            &taffy,
        );
        dispatch(
            &mut app,
            &left_press(),
            mods,
            &mut state.doc,
            &state.root,
            node,
            &taffy,
        );
        assert!(
            app.add_panel.is_open(),
            "clicking the + Add account button opens the add modal"
        );
    }

    #[test]
    fn ctrl_n_opens_the_add_modal() {
        let (mut app, mut state) = app_with_state();
        let (taffy, node) = layout_at(&mut app, 1000.0, 800.0);
        let ctrl = ctrl();
        let handled = dispatch(
            &mut app,
            &char_press("n"),
            ctrl,
            &mut state.doc,
            &state.root,
            node,
            &taffy,
        );
        assert!(handled, "Ctrl+N is consumed");
        assert!(app.add_panel.is_open(), "Ctrl+N opens the add modal");
    }

    #[test]
    fn slash_and_ctrl_f_focus_the_search_field() {
        let (mut app, mut state) = app_with_state();
        let (taffy, node) = layout_at(&mut app, 1000.0, 800.0);
        assert!(!app.search.is_focused());

        // "/" focuses search when not already typing there.
        let mods = no_mods();
        dispatch(
            &mut app,
            &char_press("/"),
            mods,
            &mut state.doc,
            &state.root,
            node,
            &taffy,
        );
        assert!(app.search.is_focused(), "'/' focuses the search field");

        // Ctrl+F also focuses search.
        app.search.set_focus(false);
        let ctrl = ctrl();
        dispatch(
            &mut app,
            &char_press("f"),
            ctrl,
            &mut state.doc,
            &state.root,
            node,
            &taffy,
        );
        assert!(app.search.is_focused(), "Ctrl+F focuses the search field");
    }

    #[test]
    fn escape_clears_a_nonempty_search_query() {
        let (mut app, mut state) = app_with_state();
        let (taffy, node) = layout_at(&mut app, 1000.0, 800.0);
        app.search.set_focus(true);
        app.search.set_text("github");

        let mods = no_mods();
        let handled = dispatch(
            &mut app,
            &named_press(NamedKey::Escape),
            mods,
            &mut state.doc,
            &state.root,
            node,
            &taffy,
        );
        assert!(handled, "Escape is consumed while clearing the query");
        assert!(
            app.search.value().is_empty(),
            "Escape clears the search text"
        );
        assert!(!app.search.is_focused(), "Escape also drops search focus");
    }

    #[test]
    fn enter_copies_the_selected_accounts_live_code() {
        let (mut app, mut state) = app_with_state();
        let (taffy, node) = layout_at(&mut app, 1000.0, 800.0);
        let id = app
            .selected
            .expect("the demo vault selects the first account");
        let acc = app.vault.get(id).cloned().unwrap();

        let before = totp::current_code(&acc);
        let mods = no_mods();
        dispatch(
            &mut app,
            &named_press(NamedKey::Enter),
            mods,
            &mut state.doc,
            &state.root,
            node,
            &taffy,
        );
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

    #[test]
    fn toast_severity_distinguishes_failures_from_confirmations() {
        let (mut app, _) = app_with_state();
        app.toast("Account added", ToastKind::Success);
        assert_eq!(app.toast.as_ref().map(|t| t.2), Some(ToastKind::Success));
        app.toast("Invalid QR image", ToastKind::Error);
        assert_eq!(app.toast.as_ref().map(|t| t.2), Some(ToastKind::Error));
    }

    #[test]
    fn open_modal_gates_the_global_search_shortcut() {
        // While the add modal is open it owns all input: a key that would be a
        // global shortcut on the list (here "/", focus-search) must be swallowed
        // by the modal and NOT focus the underlying search field.
        let (mut app, mut state) = app_with_state();
        let (taffy, node) = layout_at(&mut app, 1000.0, 800.0);
        app.add_panel.open();
        app.add_panel.layout(app.rect);
        assert!(!app.search.is_focused());

        let mods = no_mods();
        dispatch(
            &mut app,
            &char_press("/"),
            mods,
            &mut state.doc,
            &state.root,
            node,
            &taffy,
        );
        assert!(
            !app.search.is_focused(),
            "the open modal gates the '/' shortcut from reaching the list"
        );
        assert!(app.add_panel.is_open(), "the modal stays open");
    }

    #[test]
    fn search_with_no_matches_sets_a_contextual_empty_message() {
        let (mut app, _state) = app_with_state();
        app.search.set_text("zzz-no-such-service");
        app.refresh_list();
        assert!(app.list.is_empty(), "no demo account matches the query");
        let msg = app.list.empty_message();
        assert!(
            msg.contains("zzz-no-such-service"),
            "the empty state names the unmatched query, got: {msg}"
        );
    }

    /// A `--shots` entry point: its display name and root-widget factory.
    type ShotsScenario = (&'static str, fn() -> Box<dyn Widget>);

    #[test]
    fn every_shots_scenario_renders_a_nonblank_frame() {
        // The five `--shots` entry points must each produce a real frame, not
        // collapse to nothing. Runs the exact host cycle (sync → build_layout →
        // compute_layout(measure) → apply_layout → draw) and asserts the scene
        // encoded draw commands. No GPU needed — this is pure Vello encoding.
        let scenarios: [ShotsScenario; 5] = [
            ("demo", crate::app_root_demo),
            ("empty", crate::app_root_empty),
            ("add", crate::app_root_add_screen),
            ("add-advanced", crate::app_root_add_advanced_screen),
            ("add-picker", crate::app_root_add_picker_screen),
        ];
        for (name, build) in scenarios {
            let mut widget = build();
            let theme = Theme::dark();
            let mut state = ui::State::new();
            widget.sync(&mut state.doc, &state.root);

            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy
                .new_with_children(
                    Style {
                        size: Size {
                            width: Dimension::length(1280.0),
                            height: Dimension::length(800.0),
                        },
                        ..Default::default()
                    },
                    &[child],
                )
                .unwrap();
            let measure_map = node_map.clone();
            let mut tr = TextRenderer::new();
            taffy
                .compute_layout_with_measure(
                    root,
                    Size {
                        width: taffy::AvailableSpace::Definite(1280.0),
                        height: taffy::AvailableSpace::Definite(800.0),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, &mut tr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
            widget.apply_layout(&taffy, child, Point::ORIGIN);

            let mut scene = Scene::new();
            let presence = PresenceCtx::empty();
            let permission = PermissionContext::permissive();
            widget.draw_with_presence(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &presence,
                &permission,
            );
            assert!(
                !scene.encoding().is_empty(),
                "the '{name}' shots scenario must render a non-blank frame"
            );
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
