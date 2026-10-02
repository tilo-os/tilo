//! The customizer panel.
//!
//! Composes real `ui` toolkit widgets — a [`ColorPicker`], two
//! [`Slider`]s and a [`Dropdown`] — with a custom token-chip grid. The
//! chip grid selects *which* colour token the picker edits; the picker,
//! sliders and dropdown retain their values in local typed state. The app
//! dispatches a granular action only when a changed value is committed.
//!
//! The picker has no public colour setter, so when the bound token (or
//! the selected theme) changes the app calls [`Customizer::load_picker`]
//! to swap in a fresh picker — the one place this app rebuilds a child
//! widget on demand.

use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::LocalState;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::tokens::{Metrics, Spacing, TypeScale};
use ui::widget::{Button, ColorPicker, Dropdown, Slider, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::draw;
use crate::model::{self, FONTS, ThemeSpec, TokenId};

/// Panel inset on every side — the toolkit's `s4` step.
const PAD: f32 = Spacing::BASE.s4;
/// The 1× line box of a control's `sm` label — the one text metric the
/// panel's vertical rhythm is built on, read from the toolkit's line-height
/// formula rather than carried as a copy of `size × 1.25`.
fn control_label_line_h() -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a 1× line box of a 12 pt label is a small logical-pixel value"
    )]
    let line = ui::text_metrics::line_height_px(TypeScale::default().sm, 1.0) as f32;
    line
}
/// Vertical rhythm between the panel's controls, stated as its relationship
/// rather than a number: each control's label rides in the gap above it, so
/// the gap is one `s1` from the previous control to the label's line box and
/// one `s1` from the label to its own control (HIG §7b: label ↔ control is
/// `s1`). Every control in the panel gets this same step.
fn gap() -> f32 {
    Spacing::BASE.s1 + control_label_line_h() + Spacing::BASE.s1
}
/// Child-control heights, named from the same metrics the `ui` widgets lay
/// themselves out with, so the panel's intrinsic height (used by the narrow
/// stacked/scrolling layout, where the panel can't claim `percent(1.0)`)
/// matches what the controls actually take. A stale mirror here under-
/// reserved the stack by 4 px and the column squeezed the font dropdown to
/// 38 px beside 40 px action buttons.
const PICKER_H: f32 = 220.0;
/// `Slider` lays out on `metrics.min_pointer_target`.
const SLIDER_H: f32 = Metrics::BASE.min_pointer_target;
/// `Dropdown` lays out on `metrics.bar_height`.
const DROPDOWN_H: f32 = Metrics::BASE.bar_height;
/// The action buttons share the dropdown's control height, so the panel's
/// last two rows never mix a 40 px chooser with shorter buttons.
const ACTIONS_H: f32 = DROPDOWN_H;

/// The colour-token chip grid: a header (panel title, theme name and the
/// "COLORS" label), the chips wrapped to the width the panel actually has,
/// and the selected-token caption directly under the last chip row.
const CHIP_SIZE: f64 = 28.0;
/// Gap between chips — the `s2` step.
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const, and an f32 -> f64 widening is exact"
)]
const CHIP_GAP: f64 = Spacing::BASE.s2 as f64;
/// Height of the header block above the first chip row.
const CHIP_HEADER_H: f64 = 58.0;
/// Air between the last chip row and the caption, and the caption's own line.
const CHIP_CAPTION_GAP: f64 = 15.0;
const CHIP_CAPTION_H: f64 = 15.0;

/// How many chips fit on one row at a content width of `avail` logical px.
/// The one wrap rule shared by the reserved grid height and the drawn /
/// hit-tested chip rects, so the two can never disagree.
fn chips_per_row(avail: f64) -> usize {
    let per_row = ((avail.max(0.0) + CHIP_GAP) / (CHIP_SIZE + CHIP_GAP)).floor();
    (per_row as usize).max(1)
}

/// How many rows the colour-token chips wrap into at a content width of
/// `avail` logical px (the panel width minus its two insets).
pub fn chip_rows_for_width(avail: f32) -> usize {
    TokenId::ALL.len().div_ceil(chips_per_row(f64::from(avail)))
}

/// Height of the chip-grid region (header, wrapped chip rows, caption) at a
/// content width of `avail`, at 1×. Measured from the row count instead of a
/// fixed band, so a wide panel that fits every chip on one row does not leave
/// a dead strip between the chips and their caption.
pub fn chip_grid_height(avail: f32) -> f32 {
    chip_grid_height_scaled(avail, 1.0)
}

/// [`chip_grid_height`] at the accessibility text scale: the header block
/// and the caption seat glyphs the renderer draws at that scale, so they
/// grow with it; the chips themselves do not.
pub fn chip_grid_height_scaled(avail: f32, text_scale: f32) -> f32 {
    let scale = f64::from(text_scale.max(1.0));
    let rows = chip_rows_for_width(avail) as f64;
    let chips = rows * (CHIP_SIZE + CHIP_GAP) - CHIP_GAP;
    ((CHIP_HEADER_H + CHIP_CAPTION_GAP + CHIP_CAPTION_H) * scale + chips) as f32
}

/// Intrinsic content height of the customizer column (grid + picker + two
/// sliders + font dropdown + actions, plus padding and inter-row gaps) when
/// the panel is `panel_width` wide, at 1×. Used to stack it under the
/// preview inside the narrow vertical scroll, where the panel can't claim
/// `percent(1.0)`.
pub fn intrinsic_height_for_width(panel_width: f32) -> f32 {
    intrinsic_height_for_width_scaled(panel_width, 1.0)
}

/// [`intrinsic_height_for_width`] at the accessibility text scale. The
/// inter-control gap carries each control's label (drawn one label offset
/// above the control), so it grows with the type as the grid's header does;
/// the toolkit controls keep their own heights.
pub fn intrinsic_height_for_width_scaled(panel_width: f32, text_scale: f32) -> f32 {
    let scale = text_scale.max(1.0);
    PAD * 2.0
        + chip_grid_height_scaled(panel_width - PAD * 2.0, scale)
        + gap() * scale * 5.0
        + PICKER_H
        + SLIDER_H * 2.0
        + DROPDOWN_H
        + ACTIONS_H
}

/// Vertical offset (logical px, at 1×) of a control's label above its
/// control — the label's line box plus its `s1` caption gap, the same two
/// terms [`gap`] is built from so the label always lands inside the gap.
fn control_label_offset() -> f64 {
    f64::from(control_label_line_h() + Spacing::BASE.s1)
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
    motion_slider: Slider,
    material_slider: Slider,
    icon_slider: Slider,
    font_dropdown: Dropdown,
    buttons: Vec<Button>,
    color_state: LocalState<Color>,
    radius_state: LocalState<f64>,
    spacing_state: LocalState<f64>,
    motion_state: LocalState<f64>,
    material_state: LocalState<f64>,
    icon_state: LocalState<f64>,
    font_state: LocalState<String>,
    action_theme: Option<String>,
    pending_edit_token: Option<TokenId>,

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
    motion_node: Option<NodeId>,
    material_node: Option<NodeId>,
    icon_node: Option<NodeId>,
    font_node: Option<NodeId>,
    actions_node: Option<NodeId>,
    chip_rects: Vec<(TokenId, Rect)>,
    grid_rect: Rect,
    structure_dirty: bool,
    /// Last known cursor position (logical px), tracked from `CursorMoved`
    /// so a press — which carries no position — can hit-test the chips.
    cursor: Option<Point>,
    /// Panel width (logical px) the next `build_layout` sizes the chip grid
    /// for. The owning app sets it from the layout class it is building
    /// (docked column or full narrow width) before every `build_layout`.
    layout_width: f32,
    /// The OS theme's text scale as `build_layout` last saw it. The panel's
    /// hand-laid text (header, caption, control labels) is drawn at that
    /// scale by the renderer, so the offsets that seat it follow the same
    /// factor.
    text_scale: f32,
}

impl Default for Customizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Customizer {
    pub fn new() -> Self {
        // Before a spec is loaded the picker shows the default theme's
        // accent. `load_picker` replaces it with the token being edited.
        let color_state = LocalState::new(Theme::default().accent_primary);
        let radius_state = LocalState::new(0.5);
        let spacing_state = LocalState::new(0.5);
        let motion_state = LocalState::new(f64::from(model::MOTION_NORM_AT_STOCK));
        let material_state = LocalState::new(f64::from(model::MATERIAL_NORM_AT_STOCK));
        let icon_state = LocalState::new(f64::from(model::ICON_NORM_AT_STOCK));
        let font_state = LocalState::new(String::from("Inter"));
        Self {
            color_picker: ColorPicker::from_state(&color_state),
            radius_slider: Slider::from_state(&radius_state)
                .with_id("radius_slider")
                .with_accessible_name("Corner radius"),
            spacing_slider: Slider::from_state(&spacing_state)
                .with_id("spacing_slider")
                .with_accessible_name("Interface spacing"),
            motion_slider: Slider::from_state(&motion_state)
                .with_id("motion_slider")
                .with_accessible_name("Motion"),
            material_slider: Slider::from_state(&material_state)
                .with_id("material_slider")
                .with_accessible_name("Materials"),
            icon_slider: Slider::from_state(&icon_state)
                .with_id("icon_slider")
                .with_accessible_name("Icons"),
            font_dropdown: Dropdown::from_state(
                FONTS.iter().map(std::string::ToString::to_string).collect(),
                &font_state,
            )
            .with_id("font_dropdown"),
            buttons: vec![
                Button::new("themes.duplicate", "Duplicate", None).fit_content(ACTIONS_H),
                Button::new("themes.reset", "Reset", None).fit_content(ACTIONS_H),
                Button::new("themes.delete", "Delete", None)
                    .fit_content(ACTIONS_H)
                    .danger(),
            ],
            color_state,
            radius_state,
            spacing_state,
            motion_state,
            material_state,
            icon_state,
            font_state,
            action_theme: None,
            pending_edit_token: None,
            spec: None,
            edit_token: TokenId::Accent,
            editable: true,
            rect: Rect::ZERO,
            node: None,
            grid_node: None,
            picker_node: None,
            radius_node: None,
            spacing_node: None,
            motion_node: None,
            material_node: None,
            icon_node: None,
            font_node: None,
            actions_node: None,
            chip_rects: Vec::new(),
            grid_rect: Rect::ZERO,
            structure_dirty: false,
            cursor: None,
            layout_width: 0.0,
            text_scale: 1.0,
        }
    }

    /// Tell the next `build_layout` how wide the panel will be, so the chip
    /// grid reserves exactly the rows its chips wrap into at that width.
    pub fn set_layout_width(&mut self, panel_width: f32) {
        self.layout_width = panel_width;
    }

    /// Intrinsic content height of the panel at the width it was last told
    /// about (see [`Self::set_layout_width`]); the narrow stacked layout uses
    /// it to size the panel as a scrolling stack member.
    pub fn intrinsic_height(&self, text_scale: f32) -> f32 {
        intrinsic_height_for_width_scaled(self.layout_width, text_scale)
    }

    /// The text scale the panel's hand-laid text geometry follows, as
    /// `build_layout` last saw it on the OS theme.
    fn scale(&self) -> f64 {
        f64::from(self.text_scale.max(1.0))
    }

    /// Renderer-relative size for a theme-resolved type size, so the theme's
    /// text scale is applied exactly once.
    fn renderer_size(tr: &TextRenderer, size: f32) -> f32 {
        tr.renderer_input_style(&ui::text_render::TextStyle::new(size))
            .size
    }

    /// Content width the chip grid is sized for: the declared panel width
    /// minus the panel's two insets.
    fn grid_content_width(&self) -> f32 {
        (self.layout_width - PAD * 2.0).max(0.0)
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
        self.color_state.set(model::color(color));
    }

    pub fn picker_color(&self) -> model::Rgb {
        let c = self.color_state.get();
        model::rgb_of(c)
    }

    pub fn radius(&self) -> f64 {
        self.radius_state.get()
    }
    pub fn spacing(&self) -> f64 {
        self.spacing_state.get()
    }
    pub fn motion(&self) -> f64 {
        self.motion_state.get()
    }
    pub fn material(&self) -> f64 {
        self.material_state.get()
    }
    pub fn icon(&self) -> f64 {
        self.icon_state.get()
    }
    pub fn font(&self) -> String {
        self.font_state.get()
    }
    pub fn load_controls(&self, spec: &ThemeSpec) {
        self.radius_state.set(f64::from(spec.radius_norm));
        self.spacing_state.set(f64::from(spec.spacing_norm));
        self.motion_state
            .set(f64::from(model::motion_norm(spec.motion_scale)));
        self.material_state
            .set(f64::from(model::material_norm(spec.material_scale)));
        self.icon_state
            .set(f64::from(model::icon_norm(spec.icon_scale)));
        self.font_state.set(spec.font.clone());
    }
    pub fn bind_actions(
        &mut self,
        theme_id: &str,
        duplicate: tilo_actions::ActionBinding<()>,
        reset: Option<tilo_actions::ActionBinding<()>>,
        delete: Option<tilo_actions::ActionBinding<()>>,
    ) {
        if self.action_theme.as_deref() == Some(theme_id) {
            return;
        }
        let mut buttons = vec![
            Button::new("themes.duplicate", "Duplicate", None)
                .on_action(duplicate)
                .fit_content(ACTIONS_H),
        ];
        buttons.push(match reset {
            Some(binding) => Button::new("themes.reset", "Reset", None)
                .on_action(binding)
                .fit_content(ACTIONS_H),
            None => Button::new("themes.reset", "Reset", None)
                .fit_content(ACTIONS_H)
                .with_enabled(false),
        });
        buttons.push(match delete {
            Some(binding) => Button::new("themes.delete", "Delete", None)
                .on_action(binding)
                .fit_content(ACTIONS_H)
                .danger(),
            None => Button::new("themes.delete", "Delete", None)
                .fit_content(ACTIONS_H)
                .danger()
                .with_enabled(false),
        });
        self.buttons = buttons;
        self.action_theme = Some(theme_id.to_string());
        self.structure_dirty = true;
    }

    pub fn take_edit_token(&mut self) -> Option<TokenId> {
        self.pending_edit_token.take()
    }
}

impl Widget for Customizer {
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.color_picker.sync(state, path);
        self.radius_slider.sync(state, path);
        self.spacing_slider.sync(state, path);
        self.motion_slider.sync(state, path);
        self.material_slider.sync(state, path);
        self.icon_slider.sync(state, path);
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
        self.text_scale = theme.text_scale;
        let grid = leaf_node(
            taffy,
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(chip_grid_height_scaled(
                        self.grid_content_width(),
                        theme.text_scale,
                    )),
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
        let motion = self.motion_slider.build_layout(taffy, theme, node_map);
        self.motion_node = Some(motion);
        let material = self.material_slider.build_layout(taffy, theme, node_map);
        self.material_node = Some(material);
        let icon = self.icon_slider.build_layout(taffy, theme, node_map);
        self.icon_node = Some(icon);
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
                    width: length(theme.tokens.spacing.s2),
                    height: length(0.0_f32),
                },
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(ACTIONS_H),
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
                // Each control's label rides in the gap above it, so the
                // gap grows with the type.
                gap: Size {
                    width: length(0.0_f32),
                    height: length(gap() * theme.text_scale.max(1.0)),
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
            &[
                grid, picker, radius, spacing, motion, material, icon, font, actions,
            ],
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
        if let Some(n) = self.motion_node {
            self.motion_slider.apply_layout(taffy, n, o);
        }
        if let Some(n) = self.material_node {
            self.material_slider.apply_layout(taffy, n, o);
        }
        if let Some(n) = self.icon_node {
            self.icon_slider.apply_layout(taffy, n, o);
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
        // Panel background + left divider. The divider is a hairline derived
        // from the panel's own fill (HIG §9.5), so it separates the customizer
        // from the preview on the dark preset where `border_standard` did not.
        draw::fill_rect(scene, t, self.rect, theme.bg_app);
        draw::line(
            scene,
            t,
            Point::new(self.rect.x0 + 0.5, self.rect.y0),
            Point::new(self.rect.x0 + 0.5, self.rect.y1),
            ui::surface::edge_on(theme.bg_app),
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Some(Point::new(
                position.x / ctx.scale_factor,
                position.y / ctx.scale_factor,
            ));
        }

        if self.editable && self.handle_pointer_capture(event, ctx) {
            return;
        }

        if self.handle_chip_selection(event, ctx.state, ctx.path, ctx.handled) {
            return;
        }

        if self.editable && self.handle_control_events(event, ctx) {
            return;
        }

        self.handle_button_events(event, ctx);
    }

    fn name(&self) -> &'static str {
        "Customizer"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn children(&self) -> Vec<&dyn Widget> {
        // Mirror `children_mut`: the action buttons are part of the read-only
        // tree too, so the accessibility tree, inspection and named targets
        // see the same controls the event path drives.
        let mut v: Vec<&dyn Widget> = vec![
            &self.color_picker,
            &self.radius_slider,
            &self.spacing_slider,
            &self.motion_slider,
            &self.material_slider,
            &self.icon_slider,
            &self.font_dropdown,
        ];
        for b in &self.buttons {
            v.push(b as &dyn Widget);
        }
        v
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut v: Vec<&mut (dyn Widget + '_)> = vec![
            &mut self.color_picker,
            &mut self.radius_slider,
            &mut self.spacing_slider,
            &mut self.motion_slider,
            &mut self.material_slider,
            &mut self.icon_slider,
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
                || self.spacing_slider.wants_pointer_capture()
                || self.motion_slider.wants_pointer_capture()
                || self.material_slider.wants_pointer_capture()
                || self.icon_slider.wants_pointer_capture())
    }
}

impl Customizer {
    fn layout_chips(&mut self) {
        self.chip_rects.clear();
        let x0 = self.grid_rect.x0;
        let y0 = self.grid_rect.y0 + CHIP_HEADER_H * self.scale();
        let per_row = chips_per_row(self.grid_rect.width());
        for (i, tok) in TokenId::ALL.iter().enumerate() {
            let col = (i % per_row) as f64;
            let row = (i / per_row) as f64;
            let x = x0 + col * (CHIP_SIZE + CHIP_GAP);
            let y = y0 + row * (CHIP_SIZE + CHIP_GAP);
            self.chip_rects
                .push((*tok, Rect::new(x, y, x + CHIP_SIZE, y + CHIP_SIZE)));
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
        // Panel title in the theme's `lg` step, the theme name and section
        // label in `sm`; the header's 1× offsets scale with the type.
        let scale = self.scale();
        let title_size = Self::renderer_size(tr, theme.tokens.typography.scale.lg);
        let small_size = Self::renderer_size(tr, theme.tokens.typography.scale.sm);
        PaintCtx::new(scene, t, tr, theme).text(
            "Customize",
            &TextStyle::new(title_size).with_weight(TextWeight::Bold),
            theme.text_primary,
            Point::new(self.grid_rect.x0, self.grid_rect.y0 + 2.0 * scale),
            None,
        );
        draw::text(
            tr,
            scene,
            t,
            &name,
            small_size,
            theme.text_dim,
            self.grid_rect.x0,
            self.grid_rect.y0 + 22.0 * scale,
        );

        PaintCtx::new(scene, t, tr, theme).text(
            "COLORS",
            &TextStyle::new(small_size).with_weight(TextWeight::Semibold),
            theme.text_dim,
            Point::new(self.grid_rect.x0, self.grid_rect.y0 + 40.0 * scale),
            None,
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
            draw_token_chip(scene, t, theme, *r, col, *tok == self.edit_token);
        }
        // Caption: selected token + hex.
        if let Some(spec) = &self.spec {
            let hex = model::hex(spec.token(self.edit_token));
            let cap = format!("{} · {}", self.edit_token.label(), hex);
            let style = TextStyle::new(Self::renderer_size(tr, theme.tokens.typography.scale.sm))
                .with_weight(TextWeight::Medium);
            PaintCtx::new(scene, t, tr, theme).text(
                &cap,
                &style,
                theme.text_primary,
                Point::new(
                    self.grid_rect.x0,
                    self.grid_rect.y1 - CHIP_CAPTION_H * self.scale(),
                ),
                None,
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
        // Labels are the theme's `sm` step, one scaled label offset above
        // their control (the inter-control gap grows by the same factor).
        let label_size = Self::renderer_size(tr, theme.tokens.typography.scale.sm);
        let label_offset = control_label_offset() * self.scale();
        if let Some(spec) = &self.spec {
            draw::text(
                tr,
                scene,
                t,
                &format!("Corner radius — {:.0}px", spec.radius_px()),
                label_size,
                theme.text_dim,
                radius_r.x0,
                radius_r.y0 - label_offset,
            );
            draw::text(
                tr,
                scene,
                t,
                &format!("Spacing — {:.0}px", spec.spacing_px()),
                label_size,
                theme.text_dim,
                spacing_r.x0,
                spacing_r.y0 - label_offset,
            );
            let motion_r = self.motion_slider.get_rect();
            let material_r = self.material_slider.get_rect();
            let icon_r = self.icon_slider.get_rect();
            draw::text(
                tr,
                scene,
                t,
                &format!("Motion — {:.0}%", spec.motion_scale * 100.0),
                label_size,
                theme.text_dim,
                motion_r.x0,
                motion_r.y0 - label_offset,
            );
            draw::text(
                tr,
                scene,
                t,
                &format!("Materials — {:.0}%", spec.material_scale * 100.0),
                label_size,
                theme.text_dim,
                material_r.x0,
                material_r.y0 - label_offset,
            );
            draw::text(
                tr,
                scene,
                t,
                &format!("Icons — {:.0}%", spec.icon_scale * 100.0),
                label_size,
                theme.text_dim,
                icon_r.x0,
                icon_r.y0 - label_offset,
            );
        }
        draw::text(
            tr,
            scene,
            t,
            "Font family",
            label_size,
            theme.text_dim,
            font_r.x0,
            font_r.y0 - label_offset,
        );

        self.radius_slider.draw(scene, t, tr, theme, p);
        self.spacing_slider.draw(scene, t, tr, theme, p);
        self.motion_slider.draw(scene, t, tr, theme, p);
        self.material_slider.draw(scene, t, tr, theme, p);
        self.icon_slider.draw(scene, t, tr, theme, p);
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
            &mut PaintCtx::new(scene, t, tr, theme),
            "Preset — Duplicate to edit",
            &TextStyle::new(12.0).with_weight(TextWeight::Semibold),
            theme.text_dim,
            Rect::new(
                veil.x0,
                veil.center().y - 8.0,
                veil.x1,
                veil.center().y + 8.0,
            ),
        );
    }

    /// Pointer-capture priority for a live picker / slider drag (part of
    /// [`Widget::handle_event`], editable only). Returns `true` if a
    /// captured control owns the event, in which case the caller must stop
    /// dispatching regardless of `*handled` (a capture still claims the
    /// event even while the pointer strays outside the control's rect).
    fn handle_pointer_capture(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if self.color_picker.wants_pointer_capture() {
            if let Some(n) = self.picker_node {
                self.color_picker
                    .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            }
            return true;
        }
        if self.radius_slider.wants_pointer_capture() {
            if let Some(n) = self.radius_node {
                self.radius_slider
                    .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            }
            return true;
        }
        if self.spacing_slider.wants_pointer_capture() {
            if let Some(n) = self.spacing_node {
                self.spacing_slider
                    .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            }
            return true;
        }
        if self.motion_slider.wants_pointer_capture() {
            if let Some(n) = self.motion_node {
                self.motion_slider
                    .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            }
            return true;
        }
        if self.material_slider.wants_pointer_capture() {
            if let Some(n) = self.material_node {
                self.material_slider
                    .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            }
            return true;
        }
        if self.icon_slider.wants_pointer_capture() {
            if let Some(n) = self.icon_node {
                self.icon_slider
                    .handle_event(event, &mut ctx.reborrow(ctx.path, n));
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
        _state: &mut AutoCommit,
        _path: &ObjId,
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
                self.pending_edit_token = Some(*tok);
                *handled = true;
                return true;
            }
        }
        false
    }

    /// Forward the event into the picker / sliders / dropdown, in order
    /// (part of [`Widget::handle_event`], editable only). Returns `true`
    /// once one of them sets `*handled`.
    fn handle_control_events(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if let Some(n) = self.picker_node {
            self.color_picker
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            if *ctx.handled {
                return true;
            }
        }
        if let Some(n) = self.radius_node {
            self.radius_slider
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            if *ctx.handled {
                return true;
            }
        }
        if let Some(n) = self.spacing_node {
            self.spacing_slider
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            if *ctx.handled {
                return true;
            }
        }
        if let Some(n) = self.motion_node {
            self.motion_slider
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            if *ctx.handled {
                return true;
            }
        }
        if let Some(n) = self.material_node {
            self.material_slider
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            if *ctx.handled {
                return true;
            }
        }
        if let Some(n) = self.icon_node {
            self.icon_slider
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            if *ctx.handled {
                return true;
            }
        }
        if let Some(n) = self.font_node {
            self.font_dropdown
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            if *ctx.handled {
                return true;
            }
        }
        false
    }

    /// Forward the event into the action buttons, always active so
    /// Duplicate works on a read-only preset too (part of
    /// [`Widget::handle_event`]).
    fn handle_button_events(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let Some(an) = self.actions_node else {
            return;
        };
        let Ok(bchildren) = ctx.taffy.children(an) else {
            return;
        };
        for (b, c) in self.buttons.iter_mut().zip(bchildren.iter()) {
            b.handle_event(event, &mut ctx.reborrow(ctx.path, *c));
            if *ctx.handled {
                break;
            }
        }
    }
}

/// The same color swatch paint used by live customization and frozen library views.
fn draw_token_chip(
    scene: &mut Scene,
    t: Affine,
    theme: &Theme,
    rect: Rect,
    rgb: [u8; 3],
    selected: bool,
) {
    // The rest ring derives from the chip's own colour, so a chip that
    // matches the panel ground (a dark theme's `base` on the dark app) still
    // has a rim; the selected chip carries the accent ring instead.
    let fill = model::color(rgb);
    if selected {
        draw::fill_rrect(scene, t, rect, 6.0, fill);
        draw::stroke_rrect(scene, t, rect, 6.0, theme.accent_primary, 2.5);
    } else {
        ui::surface::card(scene, t, rect, 6.0, fill);
    }
}

pub(crate) fn draw_snapshot_tokens(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    spec: &ThemeSpec,
    rect: Rect,
) {
    draw::text(
        tr,
        scene,
        t,
        &format!(
            "{} · radius {:.2} · spacing {:.2}",
            spec.font, spec.radius_norm, spec.spacing_norm
        ),
        13.,
        theme.text_primary,
        rect.x0,
        rect.y0,
    );
    for (i, token) in TokenId::ALL.iter().enumerate() {
        let y = rect.y0 + 34. + i as f64 * 38.;
        draw_token_chip(
            scene,
            t,
            theme,
            Rect::new(rect.x0, y, rect.x0 + 28., y + 28.),
            spec.token(*token),
            false,
        );
        draw::text(
            tr,
            scene,
            t,
            &format!("{} · {}", token.label(), model::hex(spec.token(*token))),
            12.,
            theme.text_primary,
            rect.x0 + 40.,
            y + 5.,
        );
    }
}
