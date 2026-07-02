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

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};
use vello::Scene;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::color_compat::ColorCompat;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{FilePicker, FilePickerOutcome, TextInput, Widget};

use crate::add_panel::{AddAccountPanel, AddPanelEvent};
use crate::doc::AuthDoc;
use crate::list::{AccountList, ListEvent};
use crate::model::{demo_accounts, AccountId, Vault};
use crate::storage;
use crate::totp;

const OUTER_PAD: f64 = 24.0;
const ADD_BTN_W: f64 = 168.0;
const ADD_BTN_H: f64 = 40.0;
const SEARCH_H: f64 = 44.0;
const HEADER_H: f64 = 150.0;
const TOAST_TTL: Duration = Duration::from_millis(1800);

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
    /// The file chooser for QR-image import, present only while open. It
    /// overlays the add panel and takes input priority until the user
    /// picks an image or cancels.
    file_picker: Option<FilePicker>,

    rect: Rect,
    root_node: Option<NodeId>,
    header_rect: Rect,
    search_rect: Rect,
    add_btn_rect: Rect,
    list_rect: Rect,

    last_mouse: Point,
    selected: Option<AccountId>,
    toast: Option<(String, Instant)>,
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
            search: TextInput::local(),
            list: AccountList::new(),
            add_panel: AddAccountPanel::new(),
            file_picker: None,
            rect: Rect::ZERO,
            root_node: None,
            header_rect: Rect::ZERO,
            search_rect: Rect::ZERO,
            add_btn_rect: Rect::ZERO,
            list_rect: Rect::ZERO,
            last_mouse: Point::ZERO,
            selected: None,
            toast: None,
            seeded: false,
            seed_mode,
            doc_dirty: false,
            list_dirty: false,
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

    /// Persist the vault (live only) and flag it for CRDT re-mirroring.
    fn commit_vault(&mut self) {
        self.doc_dirty = true;
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
                self.list.set_empty_message(format!("No accounts match \u{201c}{}\u{201d}", query.trim()));
            }
        }
        self.list.set_selected(self.selected);
        self.list.set_accounts(visible);
    }

    fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    fn copy_account(&mut self, id: AccountId) {
        if let Some(acc) = self.vault.get(id).cloned() {
            let code = totp::current_code(&acc);
            ui::text_selection::clipboard_copy(&code);
            self.toast(format!("Copied {} code", acc.title()));
        }
    }

    /// Apply one list interaction.
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
            ListEvent::MoveUp(id) => {
                if self.vault.move_up(id) {
                    self.commit_vault();
                }
            }
            ListEvent::MoveDown(id) => {
                if self.vault.move_down(id) {
                    self.commit_vault();
                }
            }
            ListEvent::Delete(id) => {
                self.vault.delete(id);
                if self.selected == Some(id) {
                    self.selected = self.vault.ordered().first().map(|a| a.id);
                }
                self.commit_vault();
                self.toast("Account removed");
            }
        }
    }

    /// Act on one outcome drained from the Add-account modal.
    fn apply_add_panel_event(&mut self, ev: AddPanelEvent) {
        match ev {
            AddPanelEvent::Cancel => {}
            AddPanelEvent::PickImageForQr => self.import_qr_from_image(),
            AddPanelEvent::Save(mut acc) => {
                // Give the new account a distinct colour slot.
                acc.color = (self.vault.len() % 7) as u8;
                let id = acc.id;
                self.vault.upsert(acc);
                self.selected = Some(id);
                self.commit_vault();
                self.toast("Account added");
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

    /// Import a QR code from an image file into the open add panel.
    ///
    /// Opens a real file chooser ([`ui::widget::FilePicker`], the toolkit's
    /// sandbox-aware open dialog) rooted at the user's home directory so a
    /// saved screenshot or photo can be selected; the chosen path is decoded
    /// by the pure-Rust, offline QR reader in [`crate::qr`]. A `TOTP_QR_IMAGE`
    /// environment variable short-circuits the chooser with a fixed path —
    /// the seam the headless `--shots` and test flows drive.
    fn import_qr_from_image(&mut self) {
        if let Some(path) = std::env::var_os("TOTP_QR_IMAGE") {
            self.decode_qr_path(std::path::Path::new(&path));
            return;
        }
        self.open_qr_file_picker();
    }

    /// Rect the QR file chooser occupies — a centred overlay that shrinks to
    /// fit narrow windows.
    fn picker_rect(&self) -> Rect {
        let w = 560.0_f64.min(self.rect.width() - 48.0).max(280.0);
        let h = 560.0_f64.min(self.rect.height() - 48.0).max(300.0);
        let cx = self.rect.center().x;
        let cy = self.rect.center().y;
        Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0)
    }

    /// Open the file chooser for QR import, rooted at the user's home dir.
    fn open_qr_file_picker(&mut self) {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| std::path::PathBuf::from("/"));
        let mut picker = FilePicker::new("Choose a QR image", &home, vec![home.clone()]);
        picker.set_rect(self.picker_rect());
        self.file_picker = Some(picker);
    }

    /// Decode a QR image at `path` into the open add panel, or surface why
    /// it couldn't be read.
    fn decode_qr_path(&mut self, path: &std::path::Path) {
        match crate::qr::decode_image_file(path) {
            Ok(uri) => {
                self.add_panel.apply_uri(&uri);
                self.toast("QR code read");
            }
            Err(reason) => self.toast(reason),
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

    /// Open the Add-account screen with the QR file chooser overlaid — a
    /// `--shots`-only entry point that roots the picker at a small,
    /// deterministic scratch directory so the captured frame is stable and
    /// doesn't depend on the host's home contents. Runtime opens the picker
    /// via [`open_qr_file_picker`], rooted at the real home directory.
    pub fn seed_open_add_picker(&mut self) {
        self.add_panel.open();
        let dir = std::env::temp_dir().join("qos-totp-picker-demo");
        let _ = std::fs::create_dir_all(dir.join("Screenshots"));
        let _ = std::fs::create_dir_all(dir.join("Pictures"));
        for name in ["github-2fa-qr.png", "vault-backup-qr.png", "notes.txt"] {
            let _ = std::fs::write(dir.join(name), b"");
        }
        let mut picker = FilePicker::new("Choose a QR image", &dir, vec![dir.clone()]);
        picker.set_rect(self.picker_rect());
        self.file_picker = Some(picker);
    }

    // ---- header drawing ----------------------------------------------

    fn draw_header(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        // Title.
        text_renderer.draw_text_weighted(
            scene,
            "Authenticator",
            26.0,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((self.rect.x0 + OUTER_PAD, self.rect.y0 + 20.0)),
            None,
            TextWeight::Bold,
        );
        let count = self.vault.len();
        let subtitle = match count {
            0 => "No accounts".to_string(),
            1 => "1 account".to_string(),
            n => format!("{n} accounts"),
        };
        text_renderer.draw_text(
            scene,
            &subtitle,
            13.0,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((self.rect.x0 + OUTER_PAD, self.rect.y0 + 50.0)),
            None,
        );

        // Add button.
        let hovered = self.add_btn_rect.contains(self.last_mouse);
        let shape = RoundedRect::from_rect(self.add_btn_rect, theme.tokens.radii.md as f64);
        let bg = if hovered { theme.accent_hover } else { theme.accent_primary };
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
                    self.add_btn_rect.center().x - tw as f64 / 2.0,
                    self.add_btn_rect.center().y - th as f64 / 2.0,
                )),
            None,
            TextWeight::Semibold,
        );

        // Search field (TextInput draws its own chrome + focus ring).
        self.search.draw(scene, transform, text_renderer, theme, &PresenceCtx::empty());
        if self.search.value().is_empty() {
            let pad = theme.padding_standard as f64;
            text_renderer.draw_text(
                scene,
                "Search accounts\u{2026}",
                16.0,
                theme.tokens.colors.text_dim.with_alpha_factor(0.6),
                transform
                    * Affine::translate((self.search_rect.x0 + pad, self.search_rect.center().y - 10.0)),
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
        let Some((msg, when)) = &self.toast else { return };
        if when.elapsed() > TOAST_TTL {
            return;
        }
        let size = 14.0;
        let (tw, _th) = text_renderer.measure_weighted(msg, size, None, TextWeight::Medium);
        let w = tw as f64 + 40.0;
        let h = 40.0;
        let cx = self.rect.center().x;
        let y1 = self.rect.y1 - 28.0;
        let pill = Rect::new(cx - w / 2.0, y1 - h, cx + w / 2.0, y1);
        let shape = RoundedRect::from_rect(pill, h / 2.0);
        scene.fill(Fill::NonZero, transform, theme.tokens.colors.semantic.success, None, &shape);
        text_renderer.draw_text_weighted(
            scene,
            msg,
            size,
            Color::WHITE,
            transform * Affine::translate((cx - tw as f64 / 2.0, pill.center().y - size as f64 / 2.0 - 1.0)),
            None,
            TextWeight::Medium,
        );
    }
}

impl Widget for AuthenticatorApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if !self.seeded {
            self.seed();
            self.seeded = true;
        }
        // Mirror the vault into the shared document only when it actually
        // changed — an unconditional per-frame rewrite would append redundant
        // ops to the CRDT history on every hover/animation tick.
        if self.doc_dirty {
            AuthDoc::sync_vault(state, &self.vault.ordered());
            self.doc_dirty = false;
        }
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
        node_map.insert(node, self as *mut dyn Widget);
        self.root_node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let x0 = origin.x + layout.location.x as f64;
        let y0 = origin.y + layout.location.y as f64;
        self.rect = Rect::new(
            x0,
            y0,
            x0 + layout.size.width as f64,
            y0 + layout.size.height as f64,
        );

        self.header_rect = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, self.rect.y0 + HEADER_H);
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

        // Keep the QR chooser centred when it is open.
        if self.file_picker.is_some() {
            let pr = self.picker_rect();
            if let Some(picker) = self.file_picker.as_mut() {
                picker.set_rect(pr);
            }
        }
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
        let line = Rect::new(self.rect.x0, self.header_rect.y1 - 1.0, self.rect.x1, self.header_rect.y1);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.border_standard.with_alpha_factor(0.5),
            None,
            &line,
        );

        self.list.draw(scene, transform, text_renderer, theme, presence);
        self.draw_toast(scene, transform, text_renderer, theme);
        self.add_panel.draw(scene, transform, text_renderer, theme);

        // The QR file chooser sits above everything, on its own scrim.
        if self.file_picker.is_some() {
            let overlay_rect = self.rect;
            if let Some(picker) = self.file_picker.as_mut() {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.surfaces.overlay,
                    None,
                    &overlay_rect,
                );
                picker.draw(scene, transform, text_renderer, theme, presence);
            }
        }
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

        // The QR file chooser, when open, owns all input above the add panel.
        if self.file_picker.is_some() {
            *handled = true;
            // Esc closes the chooser without leaving the add panel.
            if let WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key: Key::Named(NamedKey::Escape), state: ElementState::Pressed, .. },
                ..
            } = event
            {
                self.file_picker = None;
                return;
            }
            let outcome = {
                let picker = self.file_picker.as_mut().unwrap();
                let mut throwaway = None;
                picker.handle_event(
                    event, scale_factor, modifiers, state, path, handled, &mut throwaway, node_id, taffy,
                );
                picker.take_outcome()
            };
            if let Some(outcome) = outcome {
                self.file_picker = None;
                if let FilePickerOutcome::Selected(paths) = outcome {
                    if let Some(p) = paths.first() {
                        self.decode_qr_path(&p.clone());
                    }
                }
            }
            return;
        }

        // Modal owns all input while open.
        if self.add_panel.is_open() {
            self.add_panel
                .handle_event(event, scale_factor, modifiers, state, path, handled, node_id, taffy);
            if let Some(ev) = self.add_panel.take_event() {
                self.apply_add_panel_event(ev);
            }
            return;
        }

        let mods = modifiers.state();

        // Global keyboard shortcuts.
        if let WindowEvent::KeyboardInput {
            event: KeyEvent { logical_key, state: ElementState::Pressed, .. },
            ..
        } = event
        {
            // Ctrl/Cmd+N → add account.
            if let Key::Character(c) = logical_key {
                let lc = c.to_lowercase();
                if (mods.control_key() || mods.super_key()) && lc == "n" {
                    self.add_panel.open();
                    self.add_panel.layout(self.rect);
                    *handled = true;
                    return;
                }
                if (mods.control_key() || mods.super_key()) && lc == "f" {
                    self.search.set_focus(true);
                    *handled = true;
                    return;
                }
                // "/" focuses search when not already typing there.
                if c.as_str() == "/" && !self.search.is_focused() {
                    self.search.set_focus(true);
                    *handled = true;
                    return;
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
                        return;
                    }
                }
                Key::Named(NamedKey::Enter) => {
                    self.copy_focused();
                    *handled = true;
                    return;
                }
                _ => {}
            }
        }

        // Add-button + search focus management on click.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            if self.add_btn_rect.contains(self.last_mouse) {
                self.add_panel.open();
                self.add_panel.layout(self.rect);
                *handled = true;
                return;
            }
            let on_search = self.search_rect.contains(self.last_mouse);
            self.search.set_focus(on_search);
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
                event, scale_factor, modifiers, state, path, handled, &mut throwaway, node_id, taffy,
            );
            if self.search.take_dirty().is_some() {
                // A query change reshapes the visible list but not the vault,
                // so only the list needs refreshing — no CRDT rewrite.
                self.list_dirty = true;
            }
        }

        // Route to the list (hover / click-to-copy / reorder / scroll).
        self.list
            .handle_event(event, scale_factor, modifiers, state, path, handled, &mut throwaway, node_id, taffy);
        let events = self.list.take_events();
        for ev in events {
            self.apply_list_event(ev);
        }
        // Keep the list's selection mirror current.
        self.list.set_selected(self.selected);
    }

    fn name(&self) -> &str {
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

    /// Run the app's real layout passes at a concrete window size, the same
    /// cycle `main.rs` drives for the live/offscreen frames.
    fn layout_at(app: &mut AuthenticatorApp, w: f32, h: f32) {
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
        assert!(app.list_rect.width() < wide_list_w, "list reflows narrower on resize");
        assert!(
            app.add_btn_rect.x0 < wide_btn_x0,
            "the add button re-anchors to the narrower right edge"
        );
        assert!(app.add_btn_rect.x1 <= app.rect.x1, "add button stays inside the window");

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
        if crate::storage::vault_path().map(|p| !p.exists()).unwrap_or(true) {
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
        let (mut app, _state) = app_with_state();
        let before = app.vault.len();
        let id = app.vault.ordered()[0].id;
        app.apply_list_event(ListEvent::Delete(id));
        assert_eq!(app.vault.len(), before - 1);
        assert!(app.vault.get(id).is_none());
    }

    #[test]
    fn reorder_event_moves_account_down() {
        let (mut app, _state) = app_with_state();
        let first = app.vault.ordered()[0].id;
        let second = app.vault.ordered()[1].id;
        app.apply_list_event(ListEvent::MoveDown(first));
        assert_eq!(app.vault.ordered()[0].id, second);
        assert_eq!(app.vault.ordered()[1].id, first);
    }

    #[test]
    fn copy_event_selects_the_account_and_confirms_with_a_toast() {
        let (mut app, _state) = app_with_state();
        let id = app.vault.ordered()[0].id;
        app.apply_list_event(ListEvent::Copy(id));
        assert_eq!(app.selected, Some(id), "the copied row becomes selected");
        let (msg, _) = app.toast.clone().expect("copying shows a confirmation toast");
        assert!(msg.starts_with("Copied"), "toast confirms the copy: {msg}");
    }

    #[test]
    fn adding_an_account_through_the_panel_grows_the_vault() {
        let (mut app, _state) = app_with_state();
        let before = app.vault.len();
        app.add_panel.open();
        // Fill via the paste seam, then run the production save path.
        app.add_panel
            .apply_uri("otpauth://totp/Proton:ada@pm.me?secret=JBSWY3DPEHPK3PXP");
        app.add_panel.save_for_test();
        assert!(!app.add_panel.is_open(), "a valid save closes the modal");
        let ev = app.add_panel.take_event().expect("save emits an event");
        app.apply_add_panel_event(ev);
        assert_eq!(app.vault.len(), before + 1, "the account is added to the vault");
        assert!(app.doc_dirty, "the new account is queued for CRDT mirroring");
        assert!(
            app.vault.ordered().iter().any(|a| a.issuer == "Proton"),
            "the added account is present by issuer"
        );
    }

    /// The primary "Scan a QR image" action must reach a real file chooser,
    /// not a dead-end toast.
    #[test]
    fn qr_cta_opens_a_real_file_chooser() {
        let (mut app, _state) = app_with_state();
        app.rect = Rect::new(0.0, 0.0, 1000.0, 800.0);
        app.add_panel.open();
        assert!(app.file_picker.is_none());
        app.open_qr_file_picker();
        assert!(
            app.file_picker.is_some(),
            "the QR action opens a file chooser instead of doing nothing"
        );
    }

    fn cursor_event(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }

    fn left_press_event() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    fn drive(app: &mut AuthenticatorApp, state: &mut ui::State, event: &WindowEvent) {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            event,
            1.0,
            &winit::event::Modifiers::default(),
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
    }

    /// End-to-end proof of the whole import interaction on the real
    /// `handle_event` path: the file chooser is open, the user clicks a QR
    /// image and then Open, and the decoded account fills the add panel.
    #[test]
    fn picking_a_qr_image_fills_the_add_panel_through_handle_event() {
        let uri = "otpauth://totp/Acme:bob@acme.io?secret=JBSWY3DPEHPK3PXP&issuer=Acme";
        let dir = std::env::temp_dir().join(format!("totp-pick-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("acme-qr.png");
        render_qr_png(uri, &img);

        let (mut app, mut state) = app_with_state();
        app.rect = Rect::new(0.0, 0.0, 1000.0, 800.0);
        app.add_panel.open();

        // Open a chooser rooted at the temp dir (runtime roots at $HOME).
        let mut picker = ui::widget::FilePicker::new("Choose a QR image", &dir, vec![dir.clone()]);
        picker.set_rect(app.picker_rect());
        // The single file sorts to index 0 (no sub-directories present).
        let entry_rect = picker.entry_rect(0).expect("the QR image is listed");
        let open_rect = picker.open_rect();
        app.file_picker = Some(picker);

        // Click the entry (selects it), then click Open — all through the
        // production event path.
        drive(&mut app, &mut state, &cursor_event(entry_rect.center()));
        drive(&mut app, &mut state, &left_press_event());
        drive(&mut app, &mut state, &cursor_event(open_rect.center()));
        drive(&mut app, &mut state, &left_press_event());

        assert!(app.file_picker.is_none(), "choosing a file closes the chooser");
        let (issuer, label, secret) = app.add_panel.field_snapshot();
        assert_eq!(issuer, "Acme");
        assert_eq!(label, "bob@acme.io");
        assert_eq!(secret, "JBSWY3DPEHPK3PXP");
        std::fs::remove_dir_all(&dir).ok();
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
