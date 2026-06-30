//! Root widget for the Themes app. Owns the theme catalog, the three
//! panels (gallery / preview / customizer) and the top bar, and runs the
//! per-frame reconciliation that keeps the CRDT, the editor controls and
//! the selected [`ThemeSpec`] in sync.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

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
            bound_key: None,
            status: String::new(),
            structure_dirty: false,
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

        // Panes.
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
                &[gallery, preview, customizer],
            )
            .unwrap();
        self.content_node = Some(content);

        let root = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) },
                    ..Default::default()
                },
                &[topbar, content],
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

    fn draw(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, p: &PresenceCtx) {
        draw::fill_rect(scene, t, self.rect, theme.bg_app);

        // Panes.
        self.gallery.draw(scene, t, tr, theme, p);
        self.preview.draw(scene, t, tr, theme, p);
        self.customizer.draw(scene, t, tr, theme, p);

        // Top bar over the panes.
        let tb = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, self.rect.y0 + TOPBAR_H as f64);
        draw::fill_rect(scene, t, tb, theme.bg_primary);
        draw::line(scene, t, Point::new(tb.x0, tb.y1), Point::new(tb.x1, tb.y1), theme.border_standard, 1.0);
        draw::circle(scene, t, Point::new(tb.x0 + 26.0, tb.center().y), 9.0, theme.accent_primary);
        draw::circle(scene, t, Point::new(tb.x0 + 26.0, tb.center().y), 4.0, ui_on_accent(theme));
        draw::text_w(tr, scene, t, "Themes", 18.0, theme.text_primary, tb.x0 + 46.0, tb.y0 + 11.0, TextWeight::Bold);
        draw::text(tr, scene, t, "Theme studio", 11.0, theme.text_dim, tb.x0 + 46.0, tb.y0 + 33.0);

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
        if let Some(n) = self.gallery_node {
            self.gallery.handle_event(event, scale, m, state, path, handled, focus_target, n, taffy);
            if *handled {
                return;
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
