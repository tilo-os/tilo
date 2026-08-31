//! The customizer panel.
//!
//! Composes real `ui` toolkit widgets — a [`ColorPicker`], two
//! [`Slider`]s and a [`Dropdown`] — with a custom token-chip grid. The
//! chip grid selects *which* colour token the picker edits; the picker,
//! sliders and dropdown feed their values back to
//! [`crate::app::ThemesApp`] through dedicated CRDT scratch fields, which
//! the app reconciles into the selected [`ThemeSpec`].
//!
//! The picker has no public colour setter, so when the bound token (or
//! the selected theme) changes the app calls [`Customizer::load_picker`]
//! to swap in a fresh picker — the one place this app rebuilds a child
//! widget on demand.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ColorPicker, Dropdown, Slider, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::doc::{KEY_ED_FONT, KEY_ED_RADIUS, KEY_ED_SPACING};
use crate::draw;
use crate::model::{self, FONTS, ThemeSpec, TokenId};

const PAD: f32 = 16.0;
const GRID_H: f32 = 152.0;
const GAP: f32 = 22.0;
/// Child-control heights, mirrored from the `ui` widgets, so the panel's
/// intrinsic height can be computed for the narrow stacked/scrolling layout
/// (where the panel can't claim `percent(1.0)`).
const PICKER_H: f32 = 220.0;
const SLIDER_H: f32 = 30.0;
const DROPDOWN_H: f32 = 40.0;
const ACTIONS_H: f32 = 34.0;

/// Intrinsic content height of the customizer column (grid + picker + two
/// sliders + font dropdown + actions, plus padding and inter-row gaps).
/// Used to stack it under the preview inside the narrow vertical scroll.
pub fn intrinsic_height() -> f32 {
    PAD * 2.0 + GRID_H + GAP * 5.0 + PICKER_H + SLIDER_H * 2.0 + DROPDOWN_H + ACTIONS_H
}

/// Panic-free leaf/parent node construction for [`Widget::build_layout`].
/// `TaffyTree::new_leaf` / `new_with_children` are typed `-> TaffyResult<_>`
/// for API generality, but `new_leaf` always inserts and returns `Ok`, and
/// `new_with_children` only fails for a child `NodeId` belonging to a
/// *different* tree — never the case here, since every child passed in was
/// just built in this same `taffy` tree by the call immediately above each
/// call site. `Widget::build_layout` returns a bare `NodeId` (not a
/// `Result`) across every implementor, so surfacing a theoretical error here
/// would mean threading a `Result` through the whole toolkit for a branch
/// that can't be taken.
#[allow(
    clippy::unwrap_used,
    reason = "new_leaf/new_with_children cannot fail as called here; see doc comment"
)]
fn leaf_node(taffy: &mut TaffyTree<()>, style: Style) -> NodeId {
    taffy.new_leaf(style).unwrap()
}

#[allow(
    clippy::unwrap_used,
    reason = "new_leaf/new_with_children cannot fail as called here; see doc comment"
)]
fn parent_node(taffy: &mut TaffyTree<()>, style: Style, children: &[NodeId]) -> NodeId {
    taffy.new_with_children(style, children).unwrap()
}

pub struct Customizer {
    color_picker: ColorPicker,
    radius_slider: Slider,
    spacing_slider: Slider,
    font_dropdown: Dropdown,
    buttons: Vec<Button>,

    // pushed-in view state
    spec: Option<ThemeSpec>,
    edit_token: TokenId,
    editable: bool,

    rect: Rect,
    node: Option<NodeId>,
    grid_node: Option<NodeId>,
    picker_node: Option<NodeId>,
    radius_node: Option<NodeId>,
    spacing_node: Option<NodeId>,
    font_node: Option<NodeId>,
    actions_node: Option<NodeId>,
    chip_rects: Vec<(TokenId, Rect)>,
    grid_rect: Rect,
    structure_dirty: bool,
    /// Last known cursor position (logical px), tracked from `CursorMoved`
    /// so a press — which carries no position — can hit-test the chips.
    cursor: Option<Point>,
}

impl Default for Customizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Customizer {
    pub fn new() -> Self {
        Self {
            color_picker: ColorPicker::new(Color::from_rgb8(99, 102, 241)),
            radius_slider: Slider::new("radius_slider", KEY_ED_RADIUS),
            spacing_slider: Slider::new("spacing_slider", KEY_ED_SPACING),
            font_dropdown: Dropdown::new(
                FONTS.iter().map(std::string::ToString::to_string).collect(),
                KEY_ED_FONT,
            )
            .with_id("font_dropdown"),
            buttons: vec![
                Button::new(
                    "themes.duplicate",
                    "Duplicate",
                    Some(crate::doc::KEY_COMMAND),
                )
                .with_click_value("duplicate")
                .fit_content(ACTIONS_H),
                Button::new("themes.reset", "Reset", Some(crate::doc::KEY_COMMAND))
                    .with_click_value("reset")
                    .fit_content(ACTIONS_H),
                Button::new("themes.delete", "Delete", Some(crate::doc::KEY_COMMAND))
                    .with_click_value("delete")
                    .fit_content(ACTIONS_H)
                    .danger(),
            ],
            spec: None,
            edit_token: TokenId::Accent,
            editable: true,
            rect: Rect::ZERO,
            node: None,
            grid_node: None,
            picker_node: None,
            radius_node: None,
            spacing_node: None,
            font_node: None,
            actions_node: None,
            chip_rects: Vec::new(),
            grid_rect: Rect::ZERO,
            structure_dirty: false,
            cursor: None,
        }
    }

    pub fn set_view(&mut self, spec: ThemeSpec, edit_token: TokenId, editable: bool) {
        self.editable = editable;
        self.edit_token = edit_token;
        // Delete is only valid for a deletable custom theme.
        let can_delete = editable;
        if let Some(b) = self.buttons.get_mut(1) {
            b.set_enabled(editable);
        }
        if let Some(b) = self.buttons.get_mut(2) {
            b.set_enabled(can_delete);
        }
        self.spec = Some(spec);
    }

    /// Swap the colour picker so it shows `color`. Triggers a structural
    /// rebuild (new taffy node) on the next frame.
    pub fn load_picker(&mut self, color: model::Rgb) {
        self.color_picker = ColorPicker::new(model::color(color));
        self.structure_dirty = true;
    }

    pub fn picker_color(&self) -> model::Rgb {
        let c = self.color_picker.color();
        model::rgb_of(c)
    }
}

impl Widget for Customizer {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.color_picker.sync(state, path);
        self.radius_slider.sync(state, path);
        self.spacing_slider.sync(state, path);
        self.font_dropdown.sync(state, path);
        for b in &mut self.buttons {
            b.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let grid = leaf_node(
            taffy,
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(GRID_H),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
        );
        self.grid_node = Some(grid);

        let picker = self.color_picker.build_layout(taffy, theme, node_map);
        self.picker_node = Some(picker);
        let radius = self.radius_slider.build_layout(taffy, theme, node_map);
        self.radius_node = Some(radius);
        let spacing = self.spacing_slider.build_layout(taffy, theme, node_map);
        self.spacing_node = Some(spacing);
        let font = self.font_dropdown.build_layout(taffy, theme, node_map);
        self.font_node = Some(font);

        let btn_nodes: Vec<NodeId> = self
            .buttons
            .iter_mut()
            .map(|b| b.build_layout(taffy, theme, node_map))
            .collect();
        let actions = parent_node(
            taffy,
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Row,
                gap: Size {
                    width: length(8.0_f32),
                    height: length(0.0_f32),
                },
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(34.0),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
            &btn_nodes,
        );
        self.actions_node = Some(actions);

        let node = parent_node(
            taffy,
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                gap: Size {
                    width: length(0.0_f32),
                    height: length(GAP),
                },
                padding: taffy::Rect {
                    left: length(PAD),
                    right: length(PAD),
                    top: length(PAD),
                    bottom: length(PAD),
                },
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &[grid, picker, radius, spacing, font, actions],
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else { return };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        let o = Point::new(self.rect.x0, self.rect.y0);

        if let Some(n) = self.grid_node
            && let Ok(gl) = taffy.layout(n)
        {
            self.grid_rect = Rect::new(
                o.x + f64::from(gl.location.x),
                o.y + f64::from(gl.location.y),
                o.x + f64::from(gl.location.x) + f64::from(gl.size.width),
                o.y + f64::from(gl.location.y) + f64::from(gl.size.height),
            );
            self.layout_chips();
        }
        if let Some(n) = self.picker_node {
            self.color_picker.apply_layout(taffy, n, o);
        }
        if let Some(n) = self.radius_node {
            self.radius_slider.apply_layout(taffy, n, o);
        }
        if let Some(n) = self.spacing_node {
            self.spacing_slider.apply_layout(taffy, n, o);
        }
        if let Some(n) = self.font_node {
            self.font_dropdown.apply_layout(taffy, n, o);
        }
        if let Some(n) = self.actions_node
            && let Ok(al) = taffy.layout(n)
        {
            let ao = Point::new(
                o.x + f64::from(al.location.x),
                o.y + f64::from(al.location.y),
            );
            if let Ok(children) = taffy.children(n) {
                for (b, c) in self.buttons.iter_mut().zip(children.iter()) {
                    b.apply_layout(taffy, *c, ao);
                }
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        p: &PresenceCtx,
    ) {
        // Panel background + left divider.
        draw::fill_rect(scene, t, self.rect, theme.bg_app);
        draw::line(
            scene,
            t,
            Point::new(self.rect.x0, self.rect.y0),
            Point::new(self.rect.x0, self.rect.y1),
            theme.border_standard,
            1.0,
        );

        self.draw_header(scene, t, tr, theme);
        self.draw_chip_grid(scene, t, tr, theme);
        let font_r = self.draw_controls(scene, t, tr, theme, p);

        if !self.editable {
            self.draw_readonly_veil(scene, t, tr, theme, font_r);
        }
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        p: &PresenceCtx,
    ) {
        // The font dropdown's open list paints here so it floats above
        // the sliders / actions.
        self.font_dropdown.draw_overlay(scene, t, tr, theme, p);
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        m: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        _node: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Some(Point::new(position.x / scale, position.y / scale));
        }

        if self.editable
            && self.handle_pointer_capture(
                event,
                scale,
                m,
                state,
                path,
                handled,
                focus_target,
                taffy,
            )
        {
            return;
        }

        if self.handle_chip_selection(event, state, path, handled) {
            return;
        }

        if self.editable
            && self.handle_control_events(
                event,
                scale,
                m,
                state,
                path,
                handled,
                focus_target,
                taffy,
            )
        {
            return;
        }

        self.handle_button_events(event, scale, m, state, path, handled, focus_target, taffy);
    }

    fn name(&self) -> &'static str {
        "Customizer"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![
            &self.color_picker,
            &self.radius_slider,
            &self.spacing_slider,
            &self.font_dropdown,
        ]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut v: Vec<&mut (dyn Widget + '_)> = vec![
            &mut self.color_picker,
            &mut self.radius_slider,
            &mut self.spacing_slider,
            &mut self.font_dropdown,
        ];
        for b in &mut self.buttons {
            v.push(b as &mut (dyn Widget + '_));
        }
        v
    }

    fn take_structure_dirty(&mut self) -> bool {
        let d = self.structure_dirty;
        self.structure_dirty = false;
        d
    }

    fn wants_pointer_capture(&self) -> bool {
        self.editable
            && (self.color_picker.wants_pointer_capture()
                || self.radius_slider.wants_pointer_capture()
                || self.spacing_slider.wants_pointer_capture())
    }
}

impl Customizer {
    fn layout_chips(&mut self) {
        self.chip_rects.clear();
        let x0 = self.grid_rect.x0;
        let y0 = self.grid_rect.y0 + 58.0;
        let size = 28.0;
        let gap = 8.0;
        let avail = self.grid_rect.width();
        let per_row = (((avail + gap) / (size + gap)).floor() as usize).max(1);
        for (i, tok) in TokenId::ALL.iter().enumerate() {
            let col = (i % per_row) as f64;
            let row = (i / per_row) as f64;
            let x = x0 + col * (size + gap);
            let y = y0 + row * (size + gap);
            self.chip_rects
                .push((*tok, Rect::new(x, y, x + size, y + size)));
        }
    }

    /// Panel title and selected theme name, plus the "COLORS" section label
    /// (part of [`Widget::draw`]).
    fn draw_header(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let name = self
            .spec
            .as_ref()
            .map(|s| s.name.clone())
            .unwrap_or_default();
        draw::text_w(
            tr,
            scene,
            t,
            "Customize",
            16.0,
            theme.text_primary,
            self.grid_rect.x0,
            self.grid_rect.y0 + 2.0,
            TextWeight::Bold,
        );
        draw::text(
            tr,
            scene,
            t,
            &name,
            11.0,
            theme.text_dim,
            self.grid_rect.x0,
            self.grid_rect.y0 + 22.0,
        );

        draw::text_w(
            tr,
            scene,
            t,
            "COLORS",
            11.0,
            theme.text_dim,
            self.grid_rect.x0,
            self.grid_rect.y0 + 40.0,
            TextWeight::Semibold,
        );
    }

    /// The colour-token chip grid and its selected-token/hex caption (part
    /// of [`Widget::draw`]).
    fn draw_chip_grid(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        for (tok, r) in &self.chip_rects {
            let col = self
                .spec
                .as_ref()
                .map_or([128, 128, 128], |s| s.token(*tok));
            draw::fill_rrect(scene, t, *r, 6.0, model::color(col));
            let sel = *tok == self.edit_token;
            let ring = if sel {
                theme.accent_primary
            } else {
                theme.border_standard
            };
            draw::stroke_rrect(scene, t, *r, 6.0, ring, if sel { 2.5 } else { 1.0 });
        }
        // Caption: selected token + hex.
        if let Some(spec) = &self.spec {
            let hex = model::hex(spec.token(self.edit_token));
            let cap = format!("{} · {}", self.edit_token.label(), hex);
            draw::text_w(
                tr,
                scene,
                t,
                &cap,
                12.0,
                theme.text_primary,
                self.grid_rect.x0,
                self.grid_rect.y1 - 15.0,
                TextWeight::Medium,
            );
        }
    }

    /// The real picker / sliders / dropdown / action buttons and their
    /// labels (part of [`Widget::draw`]). Returns the font dropdown's rect
    /// so [`Self::draw_readonly_veil`] can size the veil against it.
    fn draw_controls(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        p: &PresenceCtx,
    ) -> Rect {
        self.color_picker.draw(scene, t, tr, theme, p);

        // Labels above sliders / dropdown.
        let radius_r = self.radius_slider.get_rect();
        let spacing_r = self.spacing_slider.get_rect();
        let font_r = self.font_dropdown.get_rect();
        if let Some(spec) = &self.spec {
            draw::text(
                tr,
                scene,
                t,
                &format!("Corner radius — {:.0}px", spec.radius_px()),
                12.0,
                theme.text_dim,
                radius_r.x0,
                radius_r.y0 - 18.0,
            );
            draw::text(
                tr,
                scene,
                t,
                &format!("Spacing — {:.0}px", spec.spacing_px()),
                12.0,
                theme.text_dim,
                spacing_r.x0,
                spacing_r.y0 - 18.0,
            );
        }
        draw::text(
            tr,
            scene,
            t,
            "Font family",
            12.0,
            theme.text_dim,
            font_r.x0,
            font_r.y0 - 18.0,
        );

        self.radius_slider.draw(scene, t, tr, theme, p);
        self.spacing_slider.draw(scene, t, tr, theme, p);
        self.font_dropdown.draw(scene, t, tr, theme, p);
        for b in &mut self.buttons {
            b.draw(scene, t, tr, theme, p);
        }
        font_r
    }

    /// The read-only veil painted over the controls when the selected theme
    /// is a built-in preset (part of [`Widget::draw`]). `font_r` is the font
    /// dropdown's rect from [`Self::draw_controls`].
    fn draw_readonly_veil(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        font_r: Rect,
    ) {
        let veil = Rect::new(
            self.rect.x0,
            self.color_picker.get_rect().y0 - 8.0,
            self.rect.x1,
            font_r.y1 + 6.0,
        );
        draw::fill_rect(scene, t, veil, theme.bg_app.multiply_alpha(0.55));
        draw::text_center(
            tr,
            scene,
            t,
            "Preset — Duplicate to edit",
            12.0,
            theme.text_dim,
            veil.x0,
            veil.x1,
            veil.center().y - 8.0,
            veil.center().y + 8.0,
            TextWeight::Semibold,
        );
    }

    /// Pointer-capture priority for a live picker / slider drag (part of
    /// [`Widget::handle_event`], editable only). Returns `true` if a
    /// captured control owns the event, in which case the caller must stop
    /// dispatching regardless of `*handled` (a capture still claims the
    /// event even while the pointer strays outside the control's rect).
    #[allow(
        clippy::too_many_arguments,
        reason = "forwards the same independently-meaningful parameters Widget::handle_event receives; a context struct would just be destructured back apart at every child dispatch"
    )]
    fn handle_pointer_capture(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        m: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        taffy: &TaffyTree<()>,
    ) -> bool {
        if self.color_picker.wants_pointer_capture() {
            if let Some(n) = self.picker_node {
                self.color_picker.handle_event(
                    event,
                    scale,
                    m,
                    state,
                    path,
                    handled,
                    focus_target,
                    n,
                    taffy,
                );
            }
            return true;
        }
        if self.radius_slider.wants_pointer_capture() {
            if let Some(n) = self.radius_node {
                self.radius_slider.handle_event(
                    event,
                    scale,
                    m,
                    state,
                    path,
                    handled,
                    focus_target,
                    n,
                    taffy,
                );
            }
            return true;
        }
        if self.spacing_slider.wants_pointer_capture() {
            if let Some(n) = self.spacing_node {
                self.spacing_slider.handle_event(
                    event,
                    scale,
                    m,
                    state,
                    path,
                    handled,
                    focus_target,
                    n,
                    taffy,
                );
            }
            return true;
        }
        false
    }

    /// Colour-token chip selection (part of [`Widget::handle_event`],
    /// editable only). Returns `true` if a chip was clicked.
    fn handle_chip_selection(
        &mut self,
        event: &WindowEvent,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) -> bool {
        if !self.editable {
            return false;
        }
        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        else {
            return false;
        };
        let Some(pt) = self.cursor else {
            return false;
        };
        for (tok, r) in &self.chip_rects {
            if r.contains(pt) {
                crate::doc::put_str(
                    state,
                    path,
                    crate::doc::KEY_COMMAND,
                    &format!("edittoken:{}", tok.key()),
                );
                *handled = true;
                return true;
            }
        }
        false
    }

    /// Forward the event into the picker / sliders / dropdown, in order
    /// (part of [`Widget::handle_event`], editable only). Returns `true`
    /// once one of them sets `*handled`.
    #[allow(
        clippy::too_many_arguments,
        reason = "forwards the same independently-meaningful parameters Widget::handle_event receives; a context struct would just be destructured back apart at every child dispatch"
    )]
    fn handle_control_events(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        m: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        taffy: &TaffyTree<()>,
    ) -> bool {
        if let Some(n) = self.picker_node {
            self.color_picker.handle_event(
                event,
                scale,
                m,
                state,
                path,
                handled,
                focus_target,
                n,
                taffy,
            );
            if *handled {
                return true;
            }
        }
        if let Some(n) = self.radius_node {
            self.radius_slider.handle_event(
                event,
                scale,
                m,
                state,
                path,
                handled,
                focus_target,
                n,
                taffy,
            );
            if *handled {
                return true;
            }
        }
        if let Some(n) = self.spacing_node {
            self.spacing_slider.handle_event(
                event,
                scale,
                m,
                state,
                path,
                handled,
                focus_target,
                n,
                taffy,
            );
            if *handled {
                return true;
            }
        }
        if let Some(n) = self.font_node {
            self.font_dropdown.handle_event(
                event,
                scale,
                m,
                state,
                path,
                handled,
                focus_target,
                n,
                taffy,
            );
            if *handled {
                return true;
            }
        }
        false
    }

    /// Forward the event into the action buttons, always active so
    /// Duplicate works on a read-only preset too (part of
    /// [`Widget::handle_event`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "forwards the same independently-meaningful parameters Widget::handle_event receives; a context struct would just be destructured back apart at every child dispatch"
    )]
    fn handle_button_events(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        m: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        taffy: &TaffyTree<()>,
    ) {
        let Some(an) = self.actions_node else {
            return;
        };
        let Ok(bchildren) = taffy.children(an) else {
            return;
        };
        for (b, c) in self.buttons.iter_mut().zip(bchildren.iter()) {
            b.handle_event(
                event,
                scale,
                m,
                state,
                path,
                handled,
                focus_target,
                *c,
                taffy,
            );
            if *handled {
                break;
            }
        }
    }
}
