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

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{FlapState, SplitLayout, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::customizer::Customizer;
use crate::doc;
use crate::draw;
use crate::gallery::Gallery;
use crate::model::{self, default_catalog, presets, ThemeSpec, TokenId, FONTS};
use crate::preview::PreviewPane;
use crate::widgets::{Btn, BtnVariant};

const TOPBAR_H: f32 = 56.0;
const GALLERY_W: f32 = 272.0;
const CUSTOMIZER_W: f32 = 344.0;

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
}

impl ThemesApp {
    pub fn new() -> Self {
        Self {
            catalog: default_catalog(),
            topbar_buttons: vec![
                Btn::new("new", "New", BtnVariant::Ghost),
                Btn::new("export", "Export JSON", BtnVariant::Ghost),
                Btn::new("save", "Save", BtnVariant::Primary),
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
        }
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
        self.build_width = width;
        let layout = SplitLayout::from_width(width, BREAKPOINT);
        self.last_layout = Some(layout);
        // Snap the gallery open at its endpoint: docked shows it beside the
        // content (the natural wide default); overlay floats it over the scrim
        // so the narrow shot captures the revealed flap.
        self.gallery_flap.snap(true);
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
            "duplicate" => {
                if let Some(src) = catalog.iter().find(|s| s.id == selected).cloned() {
                    let mut s = src.clone();
                    s.id = Self::new_id();
                    s.name = format!("{} copy", src.name);
                    s.builtin = false;
                    s.order = Self::max_order(catalog) + 1;
                    let id = s.id.clone();
                    catalog.push(s);
                    doc::put_str(state, path, doc::KEY_SELECTED, &id);
                    self.status = format!("Duplicated “{}”", src.name);
                    self.structure_dirty = true;
                }
            }
            "save" => {
                self.status = "All changes saved".to_string();
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
            "delete" => {
                if let Some(idx) = catalog.iter().position(|s| s.id == selected) {
                    if !catalog[idx].builtin && catalog.len() > 1 {
                        catalog.remove(idx);
                        let next = catalog.get(idx.min(catalog.len() - 1)).map(|s| s.id.clone()).unwrap_or_default();
                        doc::put_str(state, path, doc::KEY_SELECTED, &next);
                        self.status = "Deleted theme".to_string();
                        self.structure_dirty = true;
                    }
                }
            }
            _ => {}
        }
    }
}

impl Widget for ThemesApp {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        // 1. Seed the document on first run.
        if doc::load_catalog(state, path).is_none() {
            let cat = default_catalog();
            doc::save_catalog(state, path, &cat);
            let sel = cat.last().map(|s| s.id.clone()).unwrap_or_default();
            doc::put_str(state, path, doc::KEY_SELECTED, &sel);
            doc::put_str(state, path, doc::KEY_EDIT_TOKEN, "accent");
            if let Some(s) = cat.iter().find(|s| s.id == sel) {
                doc::put_f64(state, path, doc::KEY_ED_RADIUS, s.radius_norm as f64);
                doc::put_f64(state, path, doc::KEY_ED_SPACING, s.spacing_norm as f64);
                doc::put_str(state, path, doc::KEY_ED_FONT, &s.font);
            }
            doc::put_str(state, path, doc::KEY_COMMAND, "");
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
            let pc = self.customizer.picker_color();
            catalog[idx].set_token(edit_token, pc);
        } else {
            // Built-in preset: pin the controls to the read-only values.
            let spec = catalog[idx].clone();
            doc::put_f64(state, path, doc::KEY_ED_RADIUS, spec.radius_norm as f64);
            doc::put_f64(state, path, doc::KEY_ED_SPACING, spec.spacing_norm as f64);
            doc::put_str(state, path, doc::KEY_ED_FONT, &spec.font);
        }

        // 6. Persist + cache.
        doc::save_catalog(state, path, &catalog);
        self.catalog = catalog;

        // 7. Push view state into the panels.
        let sel_spec = self.catalog[idx].clone();
        self.gallery.set_catalog(&self.catalog, &selected);
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
        self.preview_node = Some(preview);

        let customizer = self.customizer.build_layout(taffy, theme, node_map);
        let _ = taffy.set_style(
            customizer,
            Style {
                size: Size { width: Dimension::length(CUSTOMIZER_W), height: Dimension::percent(1.0) },
                flex_shrink: 0.0,
                ..taffy.style(customizer).cloned().unwrap_or_default()
            },
        );
        self.customizer_node = Some(customizer);

        // Docked: [gallery | preview | customizer]. Overlay: [preview |
        // customizer], with the gallery laid out as a sibling that floats over
        // the content (positioned in `apply_layout` from the eased reveal).
        let content_children: Vec<NodeId> = if overlay {
            vec![preview, customizer]
        } else {
            vec![gallery, preview, customizer]
        };
        let content = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Row,
                    size: Size { width: Dimension::percent(1.0), height: Dimension::auto() },
                    flex_grow: 1.0,
                    flex_basis: length(0.0),
                    ..Default::default()
                },
                &content_children,
            )
            .unwrap();
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
            if layout == SplitLayout::Docked {
                if let Some(n) = self.gallery_node {
                    self.gallery.apply_layout(taffy, n, c_o);
                }
            }
            if let Some(n) = self.preview_node {
                self.preview.apply_layout(taffy, n, c_o);
            }
            if let Some(n) = self.customizer_node {
                self.customizer.apply_layout(taffy, n, c_o);
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
        self.preview.draw(scene, t, tr, theme, p);
        self.customizer.draw(scene, t, tr, theme, p);
        if layout == SplitLayout::Docked {
            self.gallery.draw(scene, t, tr, theme, p);
        }

        // Overlay: a dimming scrim fades in with the reveal, then the gallery
        // flap slides over it. The scrim covers the content below the top bar.
        if layout == SplitLayout::Overlay {
            let reveal = self.gallery_flap.reveal();
            if reveal > 0.001 {
                use ui::color_compat::ColorCompat;
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

        // Hamburger / close toggle (overlay only) sits at the far left and
        // shifts the logo right to make room.
        let logo_x = if self.show_toggle() {
            self.draw_toggle(scene, t, tr, theme, tb);
            tb.x0 + 16.0 + TOGGLE_SIZE + 12.0
        } else {
            tb.x0 + 16.0
        };
        draw::circle(scene, t, Point::new(logo_x + 10.0, tb.center().y), 9.0, theme.accent_primary);
        draw::circle(scene, t, Point::new(logo_x + 10.0, tb.center().y), 4.0, ui_on_accent(theme));
        draw::text_w(tr, scene, t, "Themes", 18.0, theme.text_primary, logo_x + 30.0, tb.y0 + 11.0, TextWeight::Bold);
        draw::text(tr, scene, t, "Theme studio", 11.0, theme.text_dim, logo_x + 30.0, tb.y0 + 33.0);

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
    use ui::color_compat::ColorCompat;
    let a = [theme.accent_primary.r(), theme.accent_primary.g(), theme.accent_primary.b()];
    model::color(model::on_color(a))
}
