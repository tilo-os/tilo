//! Root widget for the Themes app. Owns the theme catalog, the three
//! panels (gallery / preview / customizer) and the top bar, and runs the
//! per-frame reconciliation that keeps the CRDT, the editor controls and
//! the selected [`ThemeSpec`] in sync.
//!
//! ## Responsive layout
//!
//! The gallery is the app's **primary navigation sidebar**. On a wide window
//! it stays docked beside the preview + customizer (the original
//! side-by-side studio layout). When the window is narrower than
//! [`BREAKPOINT`] the gallery collapses into an overlay *flap*: the preview +
//! customizer take the full width and the gallery, closed by default, slides
//! in over a dimming scrim when summoned by the top-bar hamburger. The
//! breakpoint is driven by the app's own allocated width and the eased
//! reveal uses the shared [`FlapState`] kernel (the same algorithm every
//! other adaptive-sidebar app in the suite shares), so no collapse logic is
//! hand-rolled here.

use std::collections::HashMap;
use std::time::Instant;

use automerge::{AutoCommit, ChangeHash, ObjId};
use qos_actions::{ActionEnvelope, ActionTransport, EncodedAction};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::color_compat::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{FlapState, SplitLayout, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::customizer::{self, Customizer};
use crate::doc;
use crate::draw;
use crate::gallery::Gallery;
use crate::model::{self, default_catalog, demo_catalog, presets, ThemeSpec, TokenId, FONTS};
use crate::preview::PreviewPane;
use crate::store;
use crate::widgets::{Btn, BtnVariant};

/// Standalone Themes has no peer/host wiring: an action's own committed change
/// is content-addressed identical to a peer's replay and dedupes on ordinary
/// CRDT sync, so the local affordance path has nothing extra to replicate — this
/// sink drops the envelope. A collaborative session swaps in a peer-aware
/// transport at the host's Ctrl+K dispatch.
struct DiscardTransport;
impl ActionTransport for DiscardTransport {
    fn replicate(&mut self, _env: &ActionEnvelope) {}
}

const TOPBAR_H: f32 = 56.0;
const GALLERY_W: f32 = 272.0;
const CUSTOMIZER_W: f32 = 344.0;
/// Fixed height the preview takes when the layout collapses to a narrow
/// vertical stack (preview above customizer inside a scroll region). Sized
/// to fit the preview's full specimen (window card through the two charts)
/// so it doesn't overrun the customizer stacked beneath it.
const NARROW_PREVIEW_H: f32 = 560.0;

/// Allocated-width breakpoint (logical px). At or above this the gallery is a
/// docked sidebar beside the preview + customizer; below it the gallery
/// becomes an overlay flap. Chosen so the gallery, a usable preview, and the
/// customizer stop fitting side by side (`GALLERY_W + CUSTOMIZER_W` plus a
/// readable preview column).
const BREAKPOINT: f64 = 840.0;

/// The hamburger / close toggle that summons or dismisses the overlay gallery.
/// Lives at the left of the top bar; only shown when the gallery is collapsed.
const TOGGLE_SIZE: f64 = 34.0;

pub struct ThemesApp {
    catalog: Vec<ThemeSpec>,
    topbar_buttons: Vec<Btn>,
    gallery: Gallery,
    preview: PreviewPane,
    customizer: Customizer,

    rect: Rect,
    topbar_node: Option<NodeId>,
    topbar_actions_node: Option<NodeId>,
    content_node: Option<NodeId>,
    gallery_node: Option<NodeId>,
    preview_node: Option<NodeId>,
    customizer_node: Option<NodeId>,

    /// Eased open/closed state of the gallery sidebar (shared toolkit kernel).
    gallery_flap: FlapState,
    /// Layout class resolved last frame, used to detect breakpoint crossings
    /// and to know whether to rebuild the taffy tree.
    last_layout: Option<SplitLayout>,
    /// Width fed into the *next* `build_layout`. Comes from the previous
    /// frame's allocated width (live) or a shots viewport seed (headless), so
    /// the taffy structure can collapse the gallery column before its rect is
    /// known.
    build_width: f64,
    /// Wall-clock of the last reveal tick (frame-rate-independent easing).
    last_tick: Instant,
    /// On-screen rect of the hamburger toggle (overlay mode only), for hit
    /// testing.
    toggle_rect: Rect,
    /// On-screen rect of the gallery flap while it overlays the content.
    gallery_overlay_rect: Rect,
    /// Last known cursor position, for hit-testing the toggle + scrim.
    last_cursor: Point,

    bound_key: Option<(String, TokenId)>,
    status: String,
    structure_dirty: bool,

    /// Vertical scroll of the collapsed (narrow) preview+customizer stack.
    scroll: f64,
    /// Total stacked content height + its viewport, cached in `apply_layout`
    /// so the wheel handler and scrollbar can clamp / size against them.
    scroll_content_h: f64,
    scroll_viewport_h: f64,
    /// Scene-space viewport rect of the scrolling content region (below the
    /// top bar), in the narrow layout.
    content_rect: Rect,

    // --- `--shots` scenario seeds (headless single-frame capture) ---
    /// Force a selection (e.g. a preset with a distinctive font) for a shot.
    shot_select: Option<String>,
    /// Open the sidebar actions menu on the selected row for a shot.
    shot_menu: bool,
    /// Scroll the narrow stack partway so the shot proves it scrolls.
    shot_scroll: bool,
    /// When set, first-run seeding uses the demo catalog (presets + one
    /// editable custom theme) instead of the live presets-only default — for
    /// `--shots` and "try it" walkthroughs, never the live path.
    seed_demo: bool,
}

impl ThemesApp {
    pub fn new() -> Self {
        Self {
            catalog: default_catalog(),
            topbar_buttons: vec![
                Btn::new("new", "New", BtnVariant::Ghost),
                Btn::new("export", "Export JSON", BtnVariant::Ghost),
                Btn::new("apply", "Apply theme", BtnVariant::Primary),
            ],
            gallery: Gallery::new(),
            preview: PreviewPane::new(),
            customizer: Customizer::new(),
            rect: Rect::ZERO,
            topbar_node: None,
            topbar_actions_node: None,
            content_node: None,
            gallery_node: None,
            preview_node: None,
            customizer_node: None,
            // Open-when-docked is the natural navigation-sidebar default: the
            // gallery shows on a wide window and is closed (overlay) when narrow.
            gallery_flap: FlapState::open_when_docked(),
            last_layout: None,
            // Seed wide so the first frame (and the default 1280px shot) builds
            // the docked layout without a structure round-trip.
            build_width: BREAKPOINT,
            last_tick: Instant::now(),
            toggle_rect: Rect::ZERO,
            gallery_overlay_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            bound_key: None,
            status: String::new(),
            structure_dirty: false,
            scroll: 0.0,
            scroll_content_h: 0.0,
            scroll_viewport_h: 0.0,
            content_rect: Rect::ZERO,
            shot_select: None,
            shot_menu: false,
            shot_scroll: false,
            seed_demo: false,
        }
    }

    /// Like [`Self::new`] but seeds the demo catalog (presets + one editable
    /// custom theme) on first run — for `--shots` and demo walkthroughs so a
    /// capture shows the customizer editing a real custom theme. Never the live
    /// first-run path.
    pub fn new_demo() -> Self {
        let mut app = Self::new();
        app.seed_demo = true;
        app.catalog = demo_catalog();
        app
    }

    /// Seed the responsive state for a deterministic headless screenshot.
    /// `--shots` renders a single frame with no event loop, so the layout
    /// class (which normally settles over the first live frame) is seeded from
    /// the requested viewport width here, and the flap is snapped to its
    /// endpoint so the overlay shows without an in-flight animation.
    ///
    /// * width `>= BREAKPOINT` → docked gallery (the wide studio layout).
    /// * width `<  BREAKPOINT` → overlay gallery, snapped **open** so the shot
    ///   captures the collapsed sidebar floating over the scrim.
    pub fn seed_shot_viewport(&mut self, width: f64) {
        self.seed_shot(width, "");
    }

    /// Configure a deterministic headless screenshot for one of the named
    /// scenarios (empty = default). The single-frame renderer has no live
    /// first frame, so the layout class + flap endpoint + any scenario state
    /// are seeded here.
    ///
    /// * `""`      — default: docked (wide) or the revealed overlay flap (narrow).
    /// * `"menu"`  — open the sidebar actions menu on the selected custom row.
    /// * `"scroll"`— narrow, flap closed, scrolled partway to prove the stack scrolls.
    /// * `"font"`  — select a preset with a distinctive (monospace) font so the
    ///               preview specimen visibly renders in it.
    pub fn seed_shot(&mut self, width: f64, scenario: &str) {
        self.build_width = width;
        let layout = SplitLayout::from_width(width, BREAKPOINT);
        self.last_layout = Some(layout);
        match scenario {
            "scroll" => {
                self.gallery_flap.snap(false);
                self.shot_scroll = true;
            }
            "menu" => {
                self.gallery_flap.snap(true);
                self.shot_menu = true;
                self.shot_select = Some("sunset".to_string());
            }
            "font" => {
                self.gallery_flap.snap(true);
                self.shot_select = Some("dracula".to_string());
            }
            _ => self.gallery_flap.snap(true),
        }
    }

    /// Resolve the current layout class purely from allocated width.
    fn layout_class(&self) -> SplitLayout {
        SplitLayout::from_width(self.rect.width(), BREAKPOINT)
    }

    /// Whether the hamburger toggle should be shown: only when the gallery is
    /// collapsible (overlay mode).
    fn show_toggle(&self) -> bool {
        SplitLayout::from_width(self.build_width, BREAKPOINT) == SplitLayout::Overlay
    }

    /// Draw the hamburger / close toggle at the left of the top bar and record
    /// its rect for hit testing. Shows a hamburger when the gallery is hidden
    /// and a close "X" once it's revealed.
    fn draw_toggle(&mut self, scene: &mut Scene, t: Affine, _tr: &mut TextRenderer, theme: &Theme, tb: Rect) {
        let s = TOGGLE_SIZE;
        let bx = tb.x0 + 16.0;
        let by = tb.center().y - s / 2.0;
        let btn = Rect::new(bx, by, bx + s, by + s);
        self.toggle_rect = btn;

        draw::fill_rrect(scene, t, btn, theme.tokens.radii.md as f64, theme.bg_app);
        draw::stroke_rrect(scene, t, btn, theme.tokens.radii.md as f64, theme.border_standard, 1.0);

        let cx = btn.center().x;
        let cy = btn.center().y;
        let open = self.gallery_flap.reveal() > 0.5;
        if open {
            // Close "X".
            let r = 6.0;
            draw::line(scene, t, Point::new(cx - r, cy - r), Point::new(cx + r, cy + r), theme.text_primary, 1.8);
            draw::line(scene, t, Point::new(cx - r, cy + r), Point::new(cx + r, cy - r), theme.text_primary, 1.8);
        } else {
            // Three-line hamburger.
            let w = 7.0;
            for dy in [-5.0, 0.0, 5.0] {
                draw::line(
                    scene,
                    t,
                    Point::new(cx - w, cy + dy),
                    Point::new(cx + w, cy + dy),
                    theme.text_primary,
                    1.8,
                );
            }
        }
    }

    /// Draw the vertical scroll indicator for the collapsed content stack,
    /// on the right edge of the content viewport. No-op when content fits.
    fn draw_scroll_indicator(&self, scene: &mut Scene, t: Affine, theme: &Theme) {
        if self.scroll_content_h <= self.scroll_viewport_h + 1.0 {
            return;
        }
        let vp = self.content_rect;
        let x1 = vp.x1 - 3.0;
        let x0 = x1 - 6.0;
        let track = Rect::new(x0, vp.y0 + 4.0, x1, vp.y1 - 4.0);
        draw::fill_rrect(scene, t, track, 3.0, theme.border_standard.multiply_alpha(0.35));
        let ratio = (self.scroll_viewport_h / self.scroll_content_h).clamp(0.08, 1.0);
        let thumb_h = (track.height() * ratio).max(28.0);
        let max_scroll = (self.scroll_content_h - self.scroll_viewport_h).max(1.0);
        let frac = (self.scroll / max_scroll).clamp(0.0, 1.0);
        let thumb_y = track.y0 + (track.height() - thumb_h) * frac;
        let thumb = Rect::new(x0, thumb_y, x1, thumb_y + thumb_h);
        draw::fill_rrect(scene, t, thumb, 3.0, theme.text_dim.multiply_alpha(0.85));
    }

    fn new_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    fn max_order(catalog: &[ThemeSpec]) -> u32 {
        catalog.iter().map(|s| s.order).max().unwrap_or(0)
    }

    fn process_command(&mut self, cmd: &str, catalog: &mut Vec<ThemeSpec>, state: &mut AutoCommit, path: &ObjId) {
        if let Some(id) = cmd.strip_prefix("select:") {
            doc::put_str(state, path, doc::KEY_SELECTED, id);
            self.status.clear();
            return;
        }
        if let Some(key) = cmd.strip_prefix("edittoken:") {
            doc::put_str(state, path, doc::KEY_EDIT_TOKEN, key);
            return;
        }
        // Context-menu commands carry the target theme id explicitly (the row
        // right-clicked need not be the selected one).
        if let Some(id) = cmd.strip_prefix("duplicate:") {
            self.duplicate_theme(id, catalog, state, path);
            return;
        }
        if let Some(id) = cmd.strip_prefix("delete:") {
            self.delete_theme(id, catalog, state, path);
            return;
        }
        if let Some(id) = cmd.strip_prefix("rename:") {
            // Select the theme, then hand focus to the sidebar's inline editor.
            doc::put_str(state, path, doc::KEY_SELECTED, id);
            self.gallery.request_name_focus();
            self.status.clear();
            return;
        }
        let selected = doc::get_str(state, path, doc::KEY_SELECTED).unwrap_or_default();
        match cmd {
            "new" => {
                let mut s = presets()[0].clone();
                s.id = Self::new_id();
                s.name = "New theme".to_string();
                s.builtin = false;
                s.order = Self::max_order(catalog) + 1;
                let id = s.id.clone();
                catalog.push(s);
                doc::put_str(state, path, doc::KEY_SELECTED, &id);
                self.status = "Created a new theme".to_string();
                self.structure_dirty = true;
            }
            "duplicate" => self.duplicate_theme(&selected, catalog, state, path),
            "delete" => self.delete_theme(&selected, catalog, state, path),
            "apply" => {
                if let Some(src) = catalog.iter().find(|s| s.id == selected).cloned() {
                    // Persist the selected theme as the active OS theme in the
                    // shared settings store the compositor + apps read. Live
                    // in-session propagation to already-running apps still needs
                    // a compositor broadcast (see `store` docs); this makes it
                    // the look on next launch and updates the in-app "Active"
                    // badge immediately.
                    match store::save(&src) {
                        Ok(_) => {
                            doc::put_str(state, path, doc::KEY_ACTIVE, &src.id);
                            self.status = format!("Applied “{}” as the system theme", src.name);
                        }
                        Err(e) => self.status = format!("Couldn't apply theme: {e}"),
                    }
                }
            }
            "export" => {
                if let Some(src) = catalog.iter().find(|s| s.id == selected) {
                    doc::put_text(state, path, doc::KEY_EXPORT, &src.to_json());
                    self.status = format!("Exported “{}” as JSON", src.name);
                }
            }
            "reset" => {
                if let Some(idx) = catalog.iter().position(|s| s.id == selected) {
                    if !catalog[idx].builtin {
                        let base = presets()[0].clone();
                        let keep_id = catalog[idx].id.clone();
                        let keep_name = catalog[idx].name.clone();
                        let keep_order = catalog[idx].order;
                        let mut reset = base;
                        reset.id = keep_id;
                        reset.name = keep_name;
                        reset.builtin = false;
                        reset.order = keep_order;
                        catalog[idx] = reset;
                        self.status = "Reset to defaults".to_string();
                        // Force a control reload next frame.
                        self.bound_key = None;
                    }
                }
            }
            _ => {}
        }
    }

    /// Duplicate the theme `id` into a new editable custom theme and select it.
    fn duplicate_theme(&mut self, id: &str, catalog: &mut Vec<ThemeSpec>, state: &mut AutoCommit, path: &ObjId) {
        if let Some(src) = catalog.iter().find(|s| s.id == id).cloned() {
            let mut s = src.clone();
            s.id = Self::new_id();
            s.name = format!("{} copy", src.name);
            s.builtin = false;
            s.order = Self::max_order(catalog) + 1;
            let new_id = s.id.clone();
            catalog.push(s);
            doc::put_str(state, path, doc::KEY_SELECTED, &new_id);
            self.status = format!("Duplicated “{}”", src.name);
            self.structure_dirty = true;
        }
    }

    /// Delete the custom theme `id` (presets are protected; never empties the
    /// catalog) and move the selection to a surviving theme.
    fn delete_theme(&mut self, id: &str, catalog: &mut Vec<ThemeSpec>, state: &mut AutoCommit, path: &ObjId) {
        if let Some(idx) = catalog.iter().position(|s| s.id == id) {
            if !catalog[idx].builtin && catalog.len() > 1 {
                let name = catalog[idx].name.clone();
                catalog.remove(idx);
                let next = catalog.get(idx.min(catalog.len() - 1)).map(|s| s.id.clone()).unwrap_or_default();
                doc::put_str(state, path, doc::KEY_SELECTED, &next);
                self.status = format!("Deleted “{name}”");
                self.structure_dirty = true;
            }
        }
    }
}

impl Widget for ThemesApp {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        // 1. Seed the document on first run. Live seeds the presets-only
        // default (the OS's shipped themes, never a fabricated "your theme"); a
        // demo/`--shots` app seeds the demo catalog with its editable custom
        // theme. The demo focuses its editable custom theme (so a capture shows
        // real editing); the live install focuses the first preset.
        if doc::load_catalog(state, path).is_none() {
            let cat = if self.seed_demo { demo_catalog() } else { default_catalog() };
            doc::save_catalog(state, path, &cat);
            let sel = if self.seed_demo {
                cat.last().map(|s| s.id.clone()).unwrap_or_default()
            } else {
                cat.first().map(|s| s.id.clone()).unwrap_or_default()
            };
            doc::put_str(state, path, doc::KEY_SELECTED, &sel);
            doc::put_str(state, path, doc::KEY_EDIT_TOKEN, "accent");
            if let Some(s) = cat.iter().find(|s| s.id == sel) {
                doc::put_f64(state, path, doc::KEY_ED_RADIUS, s.radius_norm as f64);
                doc::put_f64(state, path, doc::KEY_ED_SPACING, s.spacing_norm as f64);
                doc::put_str(state, path, doc::KEY_ED_FONT, &s.font);
                doc::put_str(state, path, doc::KEY_ED_NAME, &s.name);
            }
            // The applied OS theme: adopt whatever is already persisted in the
            // shared store, else default to the seeded selection.
            let active = store::load_active_id().unwrap_or_else(|| sel.clone());
            doc::put_str(state, path, doc::KEY_ACTIVE, &active);
            doc::put_str(state, path, doc::KEY_COMMAND, "");
        }

        // `--shots` scenario: force a specific selection for the capture.
        if let Some(id) = self.shot_select.take() {
            doc::put_str(state, path, doc::KEY_SELECTED, &id);
        }

        // 2. Load the persisted catalog.
        let mut catalog = doc::load_catalog(state, path).unwrap_or_else(default_catalog);

        // 3. Drain and dispatch any pending command.
        let cmd = doc::take_command(state, path);
        if !cmd.is_empty() {
            self.process_command(&cmd, &mut catalog, state, path);
        }

        // 4. Resolve selection + edit token.
        let mut selected = doc::get_str(state, path, doc::KEY_SELECTED).unwrap_or_default();
        if !catalog.iter().any(|s| s.id == selected) {
            selected = catalog.first().map(|s| s.id.clone()).unwrap_or_default();
            doc::put_str(state, path, doc::KEY_SELECTED, &selected);
        }
        let edit_token = doc::get_str(state, path, doc::KEY_EDIT_TOKEN)
            .and_then(|k| TokenId::from_key(&k))
            .unwrap_or(TokenId::Accent);
        let idx = catalog.iter().position(|s| s.id == selected).unwrap_or(0);
        let editable = !catalog[idx].builtin;

        // 5. Two-way reconcile the editor controls.
        let bound_key = (selected.clone(), edit_token);
        let changed = self.bound_key.as_ref() != Some(&bound_key);
        if changed {
            // LOAD: spec → controls; rebuild the colour picker.
            let spec = catalog[idx].clone();
            doc::put_f64(state, path, doc::KEY_ED_RADIUS, spec.radius_norm as f64);
            doc::put_f64(state, path, doc::KEY_ED_SPACING, spec.spacing_norm as f64);
            doc::put_str(state, path, doc::KEY_ED_FONT, &spec.font);
            doc::put_str(state, path, doc::KEY_ED_NAME, &spec.name);
            self.customizer.load_picker(spec.token(edit_token));
            self.bound_key = Some(bound_key);
        } else if editable {
            // SAVE: controls → spec.
            if let Some(r) = doc::get_f64(state, path, doc::KEY_ED_RADIUS) {
                catalog[idx].radius_norm = (r as f32).clamp(0.0, 1.0);
            }
            if let Some(s) = doc::get_f64(state, path, doc::KEY_ED_SPACING) {
                catalog[idx].spacing_norm = (s as f32).clamp(0.0, 1.0);
            }
            if let Some(f) = doc::get_str(state, path, doc::KEY_ED_FONT) {
                if FONTS.contains(&f.as_str()) {
                    catalog[idx].font = f;
                }
            }
            // Inline sidebar rename: a non-empty edited name replaces the
            // theme's name (empty is ignored so a mid-edit clear can't wipe it).
            if let Some(n) = doc::get_str(state, path, doc::KEY_ED_NAME) {
                let n = n.trim();
                if !n.is_empty() {
                    catalog[idx].name = n.to_string();
                }
            }
            let pc = self.customizer.picker_color();
            catalog[idx].set_token(edit_token, pc);
        } else {
            // Built-in preset: pin the controls to the read-only values.
            let spec = catalog[idx].clone();
            doc::put_f64(state, path, doc::KEY_ED_RADIUS, spec.radius_norm as f64);
            doc::put_f64(state, path, doc::KEY_ED_SPACING, spec.spacing_norm as f64);
            doc::put_str(state, path, doc::KEY_ED_FONT, &spec.font);
            doc::put_str(state, path, doc::KEY_ED_NAME, &spec.name);
        }

        // 6. Persist + cache.
        doc::save_catalog(state, path, &catalog);
        self.catalog = catalog;

        // 7. Push view state into the panels.
        let sel_spec = self.catalog[idx].clone();
        let active_id = doc::get_str(state, path, doc::KEY_ACTIVE).unwrap_or_default();
        self.gallery.set_view(&self.catalog, &selected, &active_id, editable);
        self.preview.set_spec(&sel_spec);
        self.customizer.set_view(sel_spec, edit_token, editable);

        // 8. Forward sync so embedded controls read their scratch fields.
        self.customizer.sync(state, path);
        self.gallery.sync(state, path);
        for b in &mut self.topbar_buttons {
            b.sync(state, path);
        }
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, theme: &Theme, node_map: &mut HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        // Top bar actions (right-aligned row of buttons).
        let btn_nodes: Vec<NodeId> = self.topbar_buttons.iter_mut().map(|b| b.build_layout(taffy, theme, node_map)).collect();
        let actions = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Row,
                    align_items: Some(taffy::AlignItems::Center),
                    gap: Size { width: length(8.0), height: length(0.0) },
                    flex_shrink: 0.0,
                    ..Default::default()
                },
                &btn_nodes,
            )
            .unwrap();
        self.topbar_actions_node = Some(actions);
        let topbar = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Row,
                    justify_content: Some(taffy::JustifyContent::FlexEnd),
                    align_items: Some(taffy::AlignItems::Center),
                    padding: taffy::Rect { left: length(18.0), right: length(18.0), top: length(0.0), bottom: length(0.0) },
                    size: Size { width: Dimension::percent(1.0), height: Dimension::length(TOPBAR_H) },
                    flex_shrink: 0.0,
                    ..Default::default()
                },
                &[actions],
            )
            .unwrap();
        self.topbar_node = Some(topbar);

        // Panes. The gallery is the primary sidebar: docked in the content row
        // on a wide window, or pulled out into a fixed-width overlay subtree on
        // a narrow one so the preview + customizer can take the full width.
        let overlay = SplitLayout::from_width(self.build_width, BREAKPOINT) == SplitLayout::Overlay;

        let gallery = self.gallery.build_layout(taffy, theme, node_map);
        let _ = taffy.set_style(
            gallery,
            Style {
                size: Size { width: Dimension::length(GALLERY_W), height: Dimension::percent(1.0) },
                flex_shrink: 0.0,
                ..taffy.style(gallery).cloned().unwrap_or_default()
            },
        );
        self.gallery_node = Some(gallery);

        let preview = self.preview.build_layout(taffy, theme, node_map);
        if overlay {
            // Collapsed: the preview takes the full width at a fixed height so
            // it stacks above the customizer inside the vertical scroll.
            // `flex_basis: auto` is required — the docked style pins it to 0
            // (grow-from-zero), which would collapse the fixed height here.
            let _ = taffy.set_style(
                preview,
                Style {
                    size: Size { width: Dimension::percent(1.0), height: Dimension::length(NARROW_PREVIEW_H) },
                    flex_grow: 0.0,
                    flex_shrink: 0.0,
                    flex_basis: Dimension::auto(),
                    ..taffy.style(preview).cloned().unwrap_or_default()
                },
            );
        }
        self.preview_node = Some(preview);

        let customizer = self.customizer.build_layout(taffy, theme, node_map);
        let cust_style = if overlay {
            // Collapsed: full width, intrinsic height, no shrink — a scrolling
            // stack member rather than a fixed side column.
            Style {
                size: Size { width: Dimension::percent(1.0), height: Dimension::length(customizer::intrinsic_height()) },
                flex_grow: 0.0,
                flex_shrink: 0.0,
                ..taffy.style(customizer).cloned().unwrap_or_default()
            }
        } else {
            Style {
                size: Size { width: Dimension::length(CUSTOMIZER_W), height: Dimension::percent(1.0) },
                flex_shrink: 0.0,
                ..taffy.style(customizer).cloned().unwrap_or_default()
            }
        };
        let _ = taffy.set_style(customizer, cust_style);
        self.customizer_node = Some(customizer);

        // Docked: [gallery | preview | customizer] in a row. Collapsed
        // (overlay): [preview / customizer] stacked in a column that the app
        // scrolls vertically, with the gallery pulled out as an overlay flap.
        let content = if overlay {
            taffy
                .new_with_children(
                    Style {
                        display: taffy::Display::Flex,
                        flex_direction: taffy::FlexDirection::Column,
                        size: Size { width: Dimension::percent(1.0), height: Dimension::auto() },
                        flex_grow: 1.0,
                        flex_basis: length(0.0),
                        overflow: taffy::Point { x: taffy::Overflow::Hidden, y: taffy::Overflow::Hidden },
                        ..Default::default()
                    },
                    &[preview, customizer],
                )
                .unwrap()
        } else {
            taffy
                .new_with_children(
                    Style {
                        display: taffy::Display::Flex,
                        flex_direction: taffy::FlexDirection::Row,
                        size: Size { width: Dimension::percent(1.0), height: Dimension::auto() },
                        flex_grow: 1.0,
                        flex_basis: length(0.0),
                        ..Default::default()
                    },
                    &[gallery, preview, customizer],
                )
                .unwrap()
        };
        self.content_node = Some(content);

        let root_children: Vec<NodeId> = if overlay {
            // The overlay gallery is a root sibling so it isn't constrained by
            // the content row; it's a fixed-width full-height strip that
            // `apply_layout` slides on/off the left edge.
            let _ = taffy.set_style(
                gallery,
                Style {
                    size: Size { width: Dimension::length(GALLERY_W), height: Dimension::percent(1.0) },
                    position: taffy::Position::Absolute,
                    flex_shrink: 0.0,
                    ..taffy.style(gallery).cloned().unwrap_or_default()
                },
            );
            vec![topbar, content, gallery]
        } else {
            vec![topbar, content]
        };
        let root = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) },
                    ..Default::default()
                },
                &root_children,
            )
            .unwrap();
        node_map.insert(root, self as *mut dyn Widget);
        root
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        let o = Point::new(self.rect.x0, self.rect.y0);

        // Resolve this frame's layout class and reconcile the flap. A
        // breakpoint crossing drops a stale user toggle (back to the natural
        // default) and rebuilds the taffy tree next frame so the gallery moves
        // between the docked column and the overlay subtree.
        let layout = self.layout_class();
        self.gallery_flap.reconcile(layout);
        let new_build_width = self.rect.width();
        let class_changed =
            SplitLayout::from_width(self.build_width, BREAKPOINT) != layout && self.rect.width() > 0.0;
        if class_changed {
            self.structure_dirty = true;
        }
        if self.rect.width() > 0.0 {
            self.build_width = new_build_width;
        }
        self.last_layout = Some(layout);

        if let (Some(tb), Some(actions)) = (self.topbar_node, self.topbar_actions_node) {
            let tbl = taffy.layout(tb).unwrap();
            let tb_o = Point::new(o.x + tbl.location.x as f64, o.y + tbl.location.y as f64);
            let al = taffy.layout(actions).unwrap();
            let a_o = Point::new(tb_o.x + al.location.x as f64, tb_o.y + al.location.y as f64);
            if let Ok(children) = taffy.children(actions) {
                for (b, c) in self.topbar_buttons.iter_mut().zip(children.iter()) {
                    b.apply_layout(taffy, *c, a_o);
                }
            }
        }

        if let Some(content) = self.content_node {
            let cl = taffy.layout(content).unwrap();
            let c_o = Point::new(o.x + cl.location.x as f64, o.y + cl.location.y as f64);
            if layout == SplitLayout::Overlay {
                // Collapsed: preview stacked above customizer, both offset up
                // by the scroll so the taller-than-viewport content is fully
                // reachable. `content_rect` is the clip/viewport the wheel and
                // scrollbar work against.
                let vp = Rect::new(c_o.x, c_o.y, c_o.x + cl.size.width as f64, c_o.y + cl.size.height as f64);
                self.content_rect = vp;
                self.scroll_viewport_h = vp.height();
                let ph = self.preview_node.map(|n| taffy.layout(n).unwrap().size.height as f64).unwrap_or(0.0);
                let ch = self.customizer_node.map(|n| taffy.layout(n).unwrap().size.height as f64).unwrap_or(0.0);
                self.scroll_content_h = ph + ch;
                if self.shot_scroll {
                    self.scroll = 260.0;
                }
                let max_scroll = (self.scroll_content_h - self.scroll_viewport_h).max(0.0);
                self.scroll = self.scroll.clamp(0.0, max_scroll);
                let scrolled = Point::new(c_o.x, c_o.y - self.scroll);
                if let Some(n) = self.preview_node {
                    self.preview.apply_layout(taffy, n, scrolled);
                }
                if let Some(n) = self.customizer_node {
                    self.customizer.apply_layout(taffy, n, scrolled);
                }
            } else {
                if let Some(n) = self.gallery_node {
                    self.gallery.apply_layout(taffy, n, c_o);
                }
                if let Some(n) = self.preview_node {
                    self.preview.apply_layout(taffy, n, c_o);
                }
                if let Some(n) = self.customizer_node {
                    self.customizer.apply_layout(taffy, n, c_o);
                }
            }
        }

        // Overlay: place the gallery flap sliding in from the left edge by the
        // eased reveal (fully off-screen at reveal 0). Its origin is below the
        // top bar so the hamburger stays reachable.
        if layout == SplitLayout::Overlay {
            let slide = self.gallery_flap.slide();
            let gw = GALLERY_W as f64;
            let x0 = self.rect.x0 - gw * (1.0 - slide);
            let y0 = self.rect.y0 + TOPBAR_H as f64;
            self.gallery_overlay_rect = Rect::new(x0, y0, x0 + gw, self.rect.y1);
            if let Some(n) = self.gallery_node {
                self.gallery
                    .apply_layout(taffy, n, Point::new(self.gallery_overlay_rect.x0, self.gallery_overlay_rect.y0));
            }
        } else {
            self.gallery_overlay_rect = Rect::ZERO;
        }

        // `--shots` menu scenario: open the sidebar actions menu now that the
        // rows have real rects to anchor it to.
        if self.shot_menu {
            self.shot_menu = false;
            self.gallery.open_menu_for_shot();
        }
    }

    fn draw(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, p: &PresenceCtx) {
        // Advance the reveal animation (frame-rate-independent; `needs_redraw`
        // keeps frames flowing while it settles).
        let layout = self.layout_class();
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f64();
        self.last_tick = now;
        self.gallery_flap.tick(layout, dt);

        draw::fill_rect(scene, t, self.rect, theme.bg_app);

        // Content panes. In overlay mode the gallery floats above the scrim, so
        // it's drawn after the scrim below.
        if layout == SplitLayout::Overlay {
            // Clip the scrolling preview+customizer stack to its viewport so
            // scrolled-out content can't bleed over the top bar; then draw the
            // scroll indicator over the clipped content.
            scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &self.content_rect);
            self.preview.draw(scene, t, tr, theme, p);
            self.customizer.draw(scene, t, tr, theme, p);
            scene.pop_layer();
            self.draw_scroll_indicator(scene, t, theme);
        } else {
            self.preview.draw(scene, t, tr, theme, p);
            self.customizer.draw(scene, t, tr, theme, p);
            self.gallery.draw(scene, t, tr, theme, p);
        }

        // Overlay: a dimming scrim fades in with the reveal, then the gallery
        // flap slides over it. The scrim covers the content below the top bar.
        if layout == SplitLayout::Overlay {
            let reveal = self.gallery_flap.reveal();
            if reveal > 0.001 {
                let scrim_rect = Rect::new(
                    self.rect.x0,
                    self.rect.y0 + TOPBAR_H as f64,
                    self.rect.x1,
                    self.rect.y1,
                );
                let scrim = theme.tokens.colors.surfaces.overlay.with_alpha_factor((reveal * 0.6) as f32);
                scene.fill(Fill::NonZero, t, scrim, None, &scrim_rect);
                self.gallery.draw(scene, t, tr, theme, p);
            }
        }

        // Top bar over the panes.
        let tb = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, self.rect.y0 + TOPBAR_H as f64);
        draw::fill_rect(scene, t, tb, theme.bg_primary);
        draw::line(scene, t, Point::new(tb.x0, tb.y1), Point::new(tb.x1, tb.y1), theme.border_standard, 1.0);

        // Hamburger / close toggle (overlay only) sits at the far left. The OS
        // owns the window title/app-name/logo, so the app draws no wordmark.
        if self.show_toggle() {
            self.draw_toggle(scene, t, tr, theme, tb);
        }

        for b in &mut self.topbar_buttons {
            b.draw(scene, t, tr, theme, p);
        }

        // Transient status toast (centre of the top bar).
        if !self.status.is_empty() {
            let (w, _) = tr.measure(&self.status, 12.0, None);
            let pad = 14.0;
            let tw = w as f64 + pad * 2.0;
            let cx = tb.center().x - tw / 2.0;
            let pill = Rect::new(cx, tb.y0 + 14.0, cx + tw, tb.y0 + 40.0);
            draw::fill_rrect(scene, t, pill, 13.0, theme.tokens.colors.semantic.success);
            draw::text_center(tr, scene, t, &self.status, 12.0, vello::peniko::Color::WHITE, pill.x0, pill.x1, pill.y0, pill.y1, TextWeight::Semibold);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_event(&mut self, event: &WindowEvent, scale: f64, m: &winit::event::Modifiers, state: &mut AutoCommit, path: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, _node: NodeId, taffy: &TaffyTree<()>) {
        // Pointer-capture priority: a live picker / slider drag in the
        // customizer must keep receiving events wherever the cursor goes.
        if self.customizer.wants_pointer_capture() {
            if let Some(n) = self.customizer_node {
                self.customizer.handle_event(event, scale, m, state, path, handled, focus_target, n, taffy);
            }
            return;
        }

        // Track the cursor for the toggle + scrim hit tests.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Point::new(position.x / scale, position.y / scale);
        }

        let overlay = self.layout_class() == SplitLayout::Overlay;
        let gallery_open = overlay && self.gallery_flap.is_open(SplitLayout::Overlay);

        // Collapsed layout: the wheel scrolls the preview+customizer stack
        // (unless the overlay gallery is open, which owns input while up).
        if overlay && !gallery_open {
            if let WindowEvent::MouseWheel { delta, .. } = event {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -*y * 60.0,
                    MouseScrollDelta::PixelDelta(pp) => -pp.y as f32,
                };
                let max = (self.scroll_content_h - self.scroll_viewport_h).max(0.0);
                self.scroll = (self.scroll + dy as f64).clamp(0.0, max);
                *handled = true;
                return;
            }
        }

        // Esc dismisses an open overlay gallery before anything else sees it.
        if overlay && gallery_open {
            if let WindowEvent::KeyboardInput { event: key_event, .. } = event {
                if key_event.state == ElementState::Pressed
                    && key_event.logical_key == Key::Named(NamedKey::Escape)
                {
                    self.gallery_flap.close();
                    *handled = true;
                    return;
                }
            }
        }

        // The hamburger toggle (overlay only) summons / dismisses the gallery.
        if overlay {
            if let WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } = event {
                if self.toggle_rect.contains(self.last_cursor) {
                    self.gallery_flap.toggle(SplitLayout::Overlay);
                    *handled = true;
                    return;
                }
                // Tapping the scrim (content, outside the flap) dismisses it.
                if gallery_open && !self.gallery_overlay_rect.contains(self.last_cursor) {
                    self.gallery_flap.close();
                    *handled = true;
                    return;
                }
            }
        }

        for (b, c) in self.topbar_buttons.iter_mut().zip(
            self.topbar_actions_node
                .and_then(|n| taffy.children(n).ok())
                .unwrap_or_default()
                .iter(),
        ) {
            b.handle_event(event, scale, m, state, path, handled, focus_target, *c, taffy);
            if *handled {
                return;
            }
        }

        // The gallery only receives events when docked, or when its overlay
        // flap is open. While open, any pointer press landing inside the flap
        // is consumed so it can't fall through to the content beneath.
        if !overlay || gallery_open {
            if let Some(n) = self.gallery_node {
                self.gallery.handle_event(event, scale, m, state, path, handled, focus_target, n, taffy);
                if *handled {
                    // Selecting a theme from the open flap dismisses it so the
                    // narrow window returns to the preview + customizer (the
                    // master→detail return).
                    if overlay {
                        self.gallery_flap.close();
                    }
                    return;
                }
                if overlay {
                    if let WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } = event {
                        if self.gallery_overlay_rect.contains(self.last_cursor) {
                            *handled = true;
                            return;
                        }
                    }
                }
            }
        }
        if let Some(n) = self.customizer_node {
            self.customizer.handle_event(event, scale, m, state, path, handled, focus_target, n, taffy);
            if *handled {
                return;
            }
        }
        if let Some(n) = self.preview_node {
            self.preview.handle_event(event, scale, m, state, path, handled, focus_target, n, taffy);
        }
    }

    fn name(&self) -> &str { "ThemesApp" }
    fn get_rect(&self) -> Rect { self.rect }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.gallery, &self.preview, &self.customizer]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.gallery, &mut self.preview, &mut self.customizer]
    }

    fn needs_redraw(&self) -> bool { true }

    fn take_structure_dirty(&mut self) -> bool {
        let d = self.structure_dirty
            | self.gallery.take_structure_dirty()
            | self.customizer.take_structure_dirty();
        self.structure_dirty = false;
        d
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &ui::permissions::PermissionContext,
    ) {
        self.draw(scene, transform, text_renderer, theme, presence);
    }
}

/// Legible on-accent colour for the OS theme (used by the top-bar logo).
fn ui_on_accent(theme: &Theme) -> vello::peniko::Color {
    let a = [theme.accent_primary.r(), theme.accent_primary.g(), theme.accent_primary.b()];
    model::color(model::on_color(a))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Point the shared theme store at a throwaway config dir once for the
    /// whole test binary, so Apply never writes the developer's real
    /// `~/.config/qos/settings.json`. `Once` serialises the single env write
    /// before any concurrent test reads it.
    fn isolate_config() {
        use std::sync::Once;
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let dir = std::env::temp_dir().join(format!("qos-themes-test-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            // SAFETY: performed exactly once, guarded by `Once`, before the
            // seed path (the only env reader) runs in any test.
            unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };
        });
    }

    /// A freshly-seeded **live** app + its CRDT state (first-run seed done).
    /// Live = presets only, no fabricated custom theme.
    fn seeded() -> (ThemesApp, ui::State) {
        isolate_config();
        let mut app = ThemesApp::new();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        (app, st)
    }

    /// A freshly-seeded **demo** app (presets + the editable custom "sunset"),
    /// for the tests that exercise editing / deleting a custom theme.
    fn seeded_demo() -> (ThemesApp, ui::State) {
        isolate_config();
        let mut app = ThemesApp::new_demo();
        let mut st = ui::State::new();
        app.sync(&mut st.doc, &st.root);
        (app, st)
    }

    fn catalog(st: &ui::State) -> Vec<ThemeSpec> {
        doc::load_catalog(&st.doc, &st.root).unwrap()
    }
    fn selected(st: &ui::State) -> String {
        doc::get_str(&st.doc, &st.root, doc::KEY_SELECTED).unwrap_or_default()
    }
    fn run(app: &mut ThemesApp, st: &mut ui::State, cmd: &str) {
        doc::put_str(&mut st.doc, &st.root, doc::KEY_COMMAND, cmd);
        app.sync(&mut st.doc, &st.root);
    }

    #[test]
    fn duplicate_command_adds_editable_copy_and_selects_it() {
        let (mut app, mut st) = seeded();
        let before = catalog(&st).len();
        run(&mut app, &mut st, "duplicate:midnight");
        let cat = catalog(&st);
        assert_eq!(cat.len(), before + 1, "one theme added");
        let copy = cat.iter().find(|s| s.name == "Midnight copy").expect("named copy exists");
        assert!(!copy.builtin, "the copy is editable");
        assert_eq!(selected(&st), copy.id, "the copy becomes the selection");
    }

    #[test]
    fn live_seed_is_presets_only_and_selects_a_preset() {
        // Honest live default: only built-in presets, and the focused theme is
        // one of them — no fabricated "your custom theme".
        let (_app, st) = seeded();
        let cat = catalog(&st);
        assert!(cat.iter().all(|s| s.builtin), "live catalog is presets only");
        let sel = selected(&st);
        assert!(cat.iter().find(|s| s.id == sel).map(|s| s.builtin).unwrap_or(false));
        assert!(!cat.iter().any(|s| s.id == "sunset"), "no seeded custom theme on the live path");
    }

    #[test]
    fn delete_command_removes_a_custom_theme_and_reselects() {
        let (mut app, mut st) = seeded_demo();
        assert!(catalog(&st).iter().any(|s| s.id == "sunset"), "seeded custom present");
        run(&mut app, &mut st, "delete:sunset");
        assert!(!catalog(&st).iter().any(|s| s.id == "sunset"), "custom theme removed");
        assert!(catalog(&st).iter().any(|s| s.id == selected(&st)), "selection stays valid");
    }

    #[test]
    fn delete_command_refuses_builtin_presets() {
        let (mut app, mut st) = seeded();
        let before = catalog(&st).len();
        run(&mut app, &mut st, "delete:midnight");
        assert_eq!(catalog(&st).len(), before, "a builtin preset is protected");
        assert!(catalog(&st).iter().any(|s| s.id == "midnight"));
    }

    #[test]
    fn inline_name_edit_persists_into_the_selected_custom_theme() {
        let (mut app, mut st) = seeded_demo();
        // The demo's editable custom "sunset" is selected out of the box.
        assert_eq!(selected(&st), "sunset");
        // The sidebar's inline TextInput writes the edited name to KEY_ED_NAME;
        // the next sync reconciles it into the catalog (persisted in the CRDT).
        doc::put_str(&mut st.doc, &st.root, doc::KEY_ED_NAME, "My Theme");
        app.sync(&mut st.doc, &st.root);
        assert_eq!(catalog(&st).iter().find(|s| s.id == "sunset").unwrap().name, "My Theme");
    }

    #[test]
    fn rename_command_selects_its_target() {
        let (mut app, mut st) = seeded();
        run(&mut app, &mut st, "duplicate:nord");
        let copy_id = catalog(&st).iter().find(|s| s.name == "Nord copy").unwrap().id.clone();
        run(&mut app, &mut st, "select:midnight");
        run(&mut app, &mut st, &format!("rename:{copy_id}"));
        assert_eq!(selected(&st), copy_id, "rename selects the target so its name is editable");
    }

    #[test]
    fn apply_command_writes_the_active_theme_to_the_shared_store() {
        let (mut app, mut st) = seeded();
        run(&mut app, &mut st, "select:dracula");
        run(&mut app, &mut st, "apply");
        // The in-app "Active" badge id updates immediately…
        assert_eq!(doc::get_str(&st.doc, &st.root, doc::KEY_ACTIVE).as_deref(), Some("dracula"));
        // …and the shared settings store on disk now names it the OS theme.
        assert_eq!(store::load_active_id().as_deref(), Some("dracula"));
        let back = store::active_spec(
            &std::fs::read_to_string(store::store_path().unwrap()).unwrap(),
        )
        .expect("the full applied spec round-trips out of the store");
        assert_eq!(back.id, "dracula");
    }
}
