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
use ui::widget::{TextInput, Widget};

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

pub struct AuthenticatorApp {
    vault: Vault,
    search: TextInput,
    list: AccountList,
    add_panel: AddAccountPanel,

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
}

impl Default for AuthenticatorApp {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthenticatorApp {
    pub fn new() -> Self {
        Self {
            vault: Vault::new(),
            search: TextInput::local(),
            list: AccountList::new(),
            add_panel: AddAccountPanel::new(),
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
        }
    }

    /// Load the persisted vault, falling back to the demo set on first
    /// run so the list is populated out of the box (and the `--shots`
    /// frame shows real rotating codes).
    fn seed(&mut self) {
        let mut vault = storage::load();
        if vault.is_empty() {
            vault.accounts = demo_accounts();
        }
        vault.renumber();
        self.selected = vault.ordered().first().map(|a| a.id);
        self.vault = vault;
        self.refresh_list();
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
            }
            ListEvent::Select(id) => self.selected = Some(id),
            ListEvent::MoveUp(id) => {
                if self.vault.move_up(id) {
                    storage::save(&self.vault);
                }
            }
            ListEvent::MoveDown(id) => {
                if self.vault.move_down(id) {
                    storage::save(&self.vault);
                }
            }
            ListEvent::Delete(id) => {
                self.vault.delete(id);
                if self.selected == Some(id) {
                    self.selected = self.vault.ordered().first().map(|a| a.id);
                }
                storage::save(&self.vault);
                self.toast("Account removed");
            }
        }
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
    /// The compositor's file picker (gated by the cluster's file-access
    /// capability) is the user-facing chooser; it hands the app a path,
    /// which arrives here via the `TOTP_QR_IMAGE` environment variable in
    /// the headless / seeded flows. The decode itself is pure-Rust and
    /// offline — see [`crate::qr::decode_image_file`]. A successful decode
    /// auto-fills the panel via [`AddAccountPanel::apply_uri`]; anything
    /// else surfaces a calm toast rather than failing silently.
    fn import_qr_from_image(&mut self) {
        let Some(path) = std::env::var_os("TOTP_QR_IMAGE") else {
            self.toast("Choose a screenshot of the QR code");
            return;
        };
        match crate::qr::decode_image_file(std::path::Path::new(&path)) {
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
        // Mirror the vault into the shared document (idempotent).
        AuthDoc::sync_vault(state, &self.vault.ordered());
        self.refresh_list();
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
            self.add_panel
                .handle_event(event, scale_factor, modifiers, state, path, handled, node_id, taffy);
            if let Some(ev) = self.add_panel.take_event() {
                match ev {
                    AddPanelEvent::Cancel => {}
                    AddPanelEvent::PickImageForQr => self.import_qr_from_image(),
                    AddPanelEvent::Save(mut acc) => {
                        // Give the new account a distinct colour slot.
                        acc.color = (self.vault.len() % 7) as u8;
                        let id = acc.id;
                        self.vault.upsert(acc);
                        self.selected = Some(id);
                        storage::save(&self.vault);
                        self.toast("Account added");
                    }
                }
                self.refresh_list();
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
                self.refresh_list();
            }
        }

        // Route to the list (hover / click-to-copy / reorder / scroll).
        self.list
            .handle_event(event, scale_factor, modifiers, state, path, handled, &mut throwaway, node_id, taffy);
        let events = self.list.take_events();
        if !events.is_empty() {
            for ev in events {
                self.apply_list_event(ev);
            }
            self.refresh_list();
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

    fn app_with_state() -> (AuthenticatorApp, ui::State) {
        let mut app = AuthenticatorApp::new();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        (app, state)
    }

    #[test]
    fn seeds_demo_accounts_into_vault_and_doc() {
        let (app, state) = app_with_state();
        assert!(app.vault.len() >= 3, "demo vault should populate");
        let in_doc = AuthDoc::list_accounts(&state.doc);
        assert_eq!(in_doc.len(), app.vault.len(), "vault mirrored to CRDT");
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
}
