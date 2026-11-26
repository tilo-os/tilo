//! `RadioGroup`: a short list of choices of which exactly one is made, each a radio beside its
//! words, for a choice a person reads before making (how a report is sent, who may see a page).

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::widget::checkbox::{BOX_EDGE, LABEL_GAP};
use crate::widget::{CheckGlyph, CheckMotion, CheckPose, CheckboxFace};
use crate::{widget::Checkbox, widget::Interaction};
use tilo_ui_base::i18n::widget_text;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
use tilo_ui_base::{num::f32_of_f64, num::f64_of_usize};
use tilo_ui_base::{text_render::TextRenderer, text_render::TextStyle};
use tilo_ui_base::{widget::ModelPath, widget::TaffyTreeExt, widget::Widget};

#[path = "radio_group_component.rs"]
mod component;
pub use component::{RadioGroupProps, RadioProps};

/// One choice of a [`RadioGroup`]: what it says, a line under that, and whether it can be made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadioOption {
    pub label: String,
    pub detail: Option<String>,
    pub enabled: bool,
    /// Why it cannot be chosen, while it cannot.
    pub why: Option<String>,
}

impl RadioOption {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: None,
            enabled: true,
            why: None,
        }
    }

    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Why it cannot be chosen, while it cannot: its tooltip, and what assistive technology
    /// reads of it.
    #[must_use]
    pub fn with_why(mut self, why: impl Into<String>) -> Self {
        self.why = Some(why.into());
        self
    }
}

/// A choice's place in the group as last laid out: its whole row (what a pointer hits) and the
/// heights of its words.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct RowFit {
    height: f64,
    label: f64,
    detail: f64,
}

/// A column of radios, one of which is chosen: the group is one stop of the keyboard's order, the
/// arrow keys move the choice along it, and a press on a row's words chooses it as one on its
/// radio does.
#[allow(
    clippy::struct_excessive_bools,
    reason = "focus, enabled, how focus came and paint are independent states"
)]
pub struct RadioGroup {
    id: Option<String>,
    options: Vec<RadioOption>,
    selected: Option<usize>,
    /// The keyboard's row.
    cursor: usize,
    hover: Option<usize>,
    /// The row a pointer went down on: chosen only if it comes up there.
    pressed: Option<usize>,
    /// The focus came with a press, which never rings it.
    pressed_into_focus: bool,
    focused: bool,
    enabled: bool,
    /// Why none of it can be chosen, while none can.
    why: Option<String>,
    accessible_name: Option<String>,
    last_cursor: Point,
    on_change: Option<Box<dyn FnMut(usize)>>,
    motions: Vec<CheckMotion>,
    rect: Rect,
    rows: Vec<RowFit>,
    gap: f64,
    target: f64,
    label_style: TextStyle,
    detail_style: TextStyle,
    interaction: tilo_ui_base::interaction::InteractionEnvironment,
    dirty: bool,
}

impl RadioGroup {
    /// A group of `options`, `selected` chosen (`None`: none yet).
    pub fn new(options: Vec<RadioOption>, selected: Option<usize>) -> Self {
        let selected = selected.filter(|index| *index < options.len());
        let typography = tilo_ui_base::tokens::typography::stock();
        Self {
            id: None,
            cursor: selected.unwrap_or_default(),
            motions: options.iter().map(|_| CheckMotion::default()).collect(),
            options,
            selected,
            hover: None,
            pressed: None,
            pressed_into_focus: false,
            focused: false,
            enabled: true,
            why: None,
            accessible_name: None,
            last_cursor: Point::ZERO,
            on_change: None,
            rect: Rect::ZERO,
            rows: Vec::new(),
            gap: f64::from(tilo_ui_base::tokens::Spacing::BASE.s1),
            target: f64::from(
                tilo_ui_base::interaction::InteractionEnvironment::default().minimum_target(),
            ),
            label_style: typography.style(TextRole::Body),
            detail_style: typography.style(TextRole::Caption),
            interaction: tilo_ui_base::interaction::InteractionEnvironment::default(),
            dirty: true,
        }
    }

    #[must_use]
    pub fn into_widget(self) -> Box<dyn Widget> {
        Box::new(self)
    }

    /// Its stable identity, for tests, captures and agents; each choice is `<id>.<index>`.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self
    }

    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.dirty |= self.enabled != enabled;
        self.enabled = enabled;
    }

    /// Why none of it can be chosen, while none can: its tooltip, and what assistive technology
    /// reads of it.
    #[must_use]
    pub fn with_why(mut self, why: impl Into<String>) -> Self {
        self.why = Some(why.into());
        self
    }

    pub fn set_why(&mut self, why: Option<String>) {
        self.why = why;
    }

    /// Why the choice at `index` cannot be made, while it cannot: its own reason, or the
    /// group's.
    fn why_not(&self, index: usize) -> Option<&str> {
        if self.can_choose(index) {
            return None;
        }
        self.options
            .get(index)
            .and_then(|option| option.why.as_deref())
            .or(self.why.as_deref())
    }

    /// Run `f` with each index the person chooses.
    #[must_use]
    pub fn on_change(mut self, f: impl FnMut(usize) + 'static) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    #[must_use]
    pub fn options(&self) -> &[RadioOption] {
        &self.options
    }

    /// Show `index` as chosen, without telling anyone.
    pub fn set_selected(&mut self, index: Option<usize>) {
        let index = index.filter(|index| *index < self.options.len());
        if index != self.selected {
            self.dirty = true;
            self.selected = index;
        }
    }

    /// Take new words for the choices in place: their number stays.
    pub fn set_options(&mut self, options: Vec<RadioOption>) {
        if options != self.options {
            self.options = options;
            self.dirty = true;
        }
    }

    fn option_id(&self, index: usize) -> String {
        format!("{}.{index}", self.id.as_deref().unwrap_or("radio"))
    }

    fn can_choose(&self, index: usize) -> bool {
        self.enabled && self.options.get(index).is_some_and(|option| option.enabled)
    }

    /// Row `index` as last laid out: the whole of it, what a pointer hits.
    fn row_rect(&self, index: usize) -> Rect {
        let mut y = self.rect.y0;
        for fit in self.rows.iter().take(index) {
            y += fit.height + self.gap;
        }
        let height = self.rows.get(index).map_or(self.target, |fit| fit.height);
        Rect::new(self.rect.x0, y, self.rect.x1, y + height)
    }

    fn row_at(&self, point: Point) -> Option<usize> {
        (0..self.options.len()).find(|index| self.row_rect(*index).contains(point))
    }

    fn request(&mut self, index: usize) {
        if !self.can_choose(index) {
            return;
        }
        self.cursor = index;
        self.dirty = true;
        if self.selected == Some(index) {
            return;
        }
        self.selected = Some(index);
        if let Some(on_change) = self.on_change.as_mut() {
            on_change(index);
        }
    }

    /// The next choice that can be made from `from`, `step` along (wrapping), as the arrow keys
    /// walk a radio group.
    fn next_enabled(&self, from: usize, forward: bool) -> Option<usize> {
        let count = self.options.len();
        (1..=count)
            .map(|step| {
                if forward {
                    (from + step) % count
                } else {
                    (from + count * step - step) % count
                }
            })
            .find(|index| self.can_choose(*index))
    }

    fn handle_key(&mut self, key: &Key) -> bool {
        if self.options.is_empty() {
            return false;
        }
        match key {
            Key::Named(NamedKey::ArrowDown | NamedKey::ArrowRight) => {
                if let Some(next) = self.next_enabled(self.cursor, true) {
                    self.request(next);
                }
            }
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowLeft) => {
                if let Some(next) = self.next_enabled(self.cursor, false) {
                    self.request(next);
                }
            }
            Key::Named(NamedKey::Space) => self.request(self.cursor),
            _ => return false,
        }
        self.pressed_into_focus = false;
        true
    }

    /// How row `index` shows: hovered, focused with the keyboard's ring, or neither; a state the
    /// view asks it to show ([`super::shown`]) on top.
    fn interaction(&self, index: usize) -> Interaction {
        let enabled = self.can_choose(index);
        super::shown::interaction(Interaction {
            hovered: enabled && self.hover == Some(index) && self.interaction.supports_hover(),
            pressed: enabled && self.pressed == Some(index),
            focused: self.focused
                && self.cursor == index
                && !self.pressed_into_focus
                && tilo_ui_base::focus_visible::shown(),
            disabled: !enabled,
        })
    }

    /// Lays the rows out at `width`, or at the width their words want when it is `None`: the
    /// rows' height together and the widest row.
    fn measure_rows(&mut self, width: Option<f32>, text: &mut TextRenderer) -> (f64, f64) {
        let lead = f64::from(LABEL_GAP) + BOX_EDGE;
        let limit = width.map(|width| (width - f32_of_f64(lead)).max(0.0));
        let mut widest = 0.0_f64;
        let target = self.target;
        let rows: Vec<RowFit> = self
            .options
            .iter()
            .map(|option| {
                let (label_w, label) =
                    text.measure_style(&widget_text(&option.label), limit, &self.label_style);
                let (detail_w, detail) = option.detail.as_ref().map_or((0.0, 0.0), |detail| {
                    text.measure_style(&widget_text(detail), limit, &self.detail_style)
                });
                widest = widest.max(lead + f64::from(label_w.max(detail_w)));
                let label = f64::from(label);
                let detail = f64::from(detail);
                RowFit {
                    height: (label + detail).max(target),
                    label,
                    detail,
                }
            })
            .collect();
        let height = rows.iter().map(|fit| fit.height).sum::<f64>()
            + self.gap * f64_of_usize(rows.len().saturating_sub(1));
        self.rows = rows;
        (height, widest)
    }
}

impl Widget for RadioGroup {
    fn is_layout_dirty(&self) -> bool {
        false
    }

    tilo_ui_base::described!();

    fn set_interaction_environment(
        &mut self,
        environment: tilo_ui_base::interaction::InteractionEnvironment,
    ) {
        if environment != self.interaction {
            self.interaction = environment;
            self.target = f64::from(environment.minimum_target());
            if !environment.supports_hover() {
                self.hover = None;
            }
            self.dirty = true;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.label_style = theme.tokens.typography.style(TextRole::Body);
        self.detail_style = theme.tokens.typography.style(TextRole::Caption);
        self.gap = f64::from(theme.tokens.spacing.s1);
        self.target = f64::from(self.interaction.minimum_target());
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            flex_shrink: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: Size<Option<f32>>,
        available: Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> Size<f32> {
        let given = known.width.or(match available.width {
            taffy::AvailableSpace::Definite(width) => Some(width),
            _ => None,
        });
        let (height, widest) = self.measure_rows(given, text);
        let width = given.unwrap_or(f32_of_f64(widest));
        Size {
            width,
            height: known.height.unwrap_or(f32_of_f64(height)),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.rect = rect;
            self.dirty = true;
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        let (scene, transform, text, theme, _) = ctx.parts();
        let now = tilo_ui_base::motion::frame_now();
        let motion = theme.motion();
        self.motions
            .resize_with(self.options.len(), CheckMotion::default);
        for index in 0..self.options.len() {
            let rect = self.row_rect(index);
            let fit = self.rows.get(index).copied().unwrap_or_default();
            let chosen = self.selected == Some(index);
            if let Some(kept) = self.motions.get_mut(index) {
                kept.follow_input(chosen, CheckGlyph::Check, motion, now);
            }
            let pose = self
                .motions
                .get(index)
                .map(|kept| kept.pose(now))
                .unwrap_or_else(|| CheckPose::at_rest(chosen, CheckGlyph::Check));
            let interact = self.interaction(index);
            // The radio stands at the middle of the words' first line, in a row as tall as a
            // target; the words fill the row's height around their own.
            let words = fit.label + fit.detail;
            let top = rect.y0 + ((rect.height() - words) / 2.0).max(0.0);
            let first_line = f64::from(self.label_style.line_height_px());
            let centre_y = if fit.detail > 0.0 {
                top + first_line.min(fit.label) / 2.0
            } else {
                rect.center().y
            };
            let face = CheckboxFace::new(
                Rect::from_center_size(
                    Point::new(rect.x0 + BOX_EDGE / 2.0, centre_y),
                    (BOX_EDGE, BOX_EDGE),
                ),
                chosen,
            )
            .radio()
            .pose(pose)
            .hovered(interact.hovered || interact.pressed)
            .focused(interact.focused)
            .disabled(interact.disabled);
            Checkbox::paint(scene, transform, theme, &face);
            let x = rect.x0 + BOX_EDGE + f64::from(LABEL_GAP);
            let limit = f32_of_f64((rect.x1 - x).max(0.0));
            let colors = &theme.tokens.colors;
            let (ink, dim) = if interact.disabled {
                (
                    colors
                        .text_primary
                        .multiply_alpha(tilo_ui_base::widget::DISABLED_ALPHA),
                    colors
                        .text_dim
                        .multiply_alpha(tilo_ui_base::widget::DISABLED_ALPHA),
                )
            } else {
                (colors.text_primary, colors.text_dim)
            };
            let option = &self.options[index];
            text.draw_style(
                scene,
                &widget_text(&option.label),
                ink,
                transform * Affine::translate((x, top)),
                Some(limit),
                &self.label_style,
            );
            if let Some(detail) = &option.detail {
                text.draw_style(
                    scene,
                    &widget_text(detail),
                    dim,
                    transform * Affine::translate((x, top + fit.label)),
                    Some(limit),
                    &self.detail_style,
                );
            }
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = ctx.logical(position);
                let hover = self.row_at(self.last_cursor);
                if hover != self.hover {
                    self.hover = hover;
                    self.dirty = true;
                }
            }
            WindowEvent::CursorLeft { .. } if self.hover.is_some() || self.pressed.is_some() => {
                self.hover = None;
                self.pressed = None;
                self.dirty = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.enabled => {
                if let Some(index) = self.row_at(self.last_cursor)
                    && self.can_choose(index)
                {
                    self.pressed = Some(index);
                    self.pressed_into_focus = true;
                    self.dirty = true;
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(pressed) = self.pressed.take() {
                    self.dirty = true;
                    if self.row_at(self.last_cursor) == Some(pressed) {
                        self.request(pressed);
                    }
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.focused
                    && self.enabled
                    && event.state == ElementState::Pressed
                    && self.handle_key(&event.logical_key) =>
            {
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn needs_animation_frame(&self) -> bool {
        let now = tilo_ui_base::motion::frame_now();
        self.motions.iter().any(|kept| kept.is_moving(now))
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.needs_animation_frame()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn name(&self) -> &'static str {
        "RadioGroup"
    }

    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.row_at(point)
            .filter(|index| self.can_choose(*index))
            .map(|_| winit::window::CursorIcon::Pointer)
    }

    fn is_focusable(&self) -> bool {
        (0..self.options.len()).any(|index| self.can_choose(index))
    }

    fn set_focus(&mut self, focused: bool) {
        self.dirty = true;
        self.focused = focused;
        if focused {
            // The keyboard lands on the choice made, or the first that can be made.
            self.cursor = self
                .selected
                .filter(|index| self.can_choose(*index))
                .or_else(|| (0..self.options.len()).find(|index| self.can_choose(*index)))
                .unwrap_or_default();
        } else {
            self.pressed_into_focus = false;
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn accessibility_hidden(&self) -> bool {
        false
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let count = self.options.len();
        let children = self
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                let mut node = AccessNode::new(Role::RadioButton)
                    .with_action_target(self.option_id(index))
                    .with_name(option.label.clone())
                    .with_bounds(self.row_rect(index))
                    .with_state(
                        AccessState::new()
                            .checked(self.selected == Some(index))
                            .focused(self.focused && self.cursor == index)
                            .disabled(!self.can_choose(index)),
                    )
                    .with_position(index.saturating_add(1), count);
                // A choice that cannot be made says why; one that can, what it means.
                if let Some(said) = self.why_not(index).or(option.detail.as_deref()) {
                    node = node.with_description(said.to_owned());
                }
                node
            })
            .collect();
        Some(
            AccessNode::new(Role::Group)
                .with_semantic_key(self.id.as_deref().unwrap_or("radio"))
                .with_optional_name(self.accessible_name.clone())
                .with_optional_value(
                    self.selected
                        .and_then(|index| self.options.get(index))
                        .map(|option| option.label.clone()),
                )
                .with_state(
                    AccessState::new()
                        .focusable(self.is_focusable())
                        .focused(self.focused)
                        .disabled(!self.enabled),
                )
                .with_optional_description(self.why.clone().filter(|_| !self.enabled))
                .with_children(children),
        )
    }

    /// Over a choice that cannot be made, why.
    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        let index = self.row_at(point)?;
        self.why_not(index)
            .map(|why| (self.row_rect(index), why.to_owned()))
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index] = path else {
            return None;
        };
        (*index < self.options.len()).then(|| self.row_rect(*index))
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &tilo_ui_base::a11y::Action,
        _state: &mut tilo_ui_base::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        let prefix = format!("{}.", self.id.as_deref().unwrap_or("radio"));
        let Some(index) = target
            .strip_prefix(&prefix)
            .and_then(|index| index.parse::<usize>().ok())
            .filter(|index| *index < self.options.len())
        else {
            return false;
        };
        match action {
            tilo_ui_base::a11y::Action::Focus => {
                self.focused = true;
                self.cursor = index;
                self.dirty = true;
                true
            }
            tilo_ui_base::a11y::Action::Activate | tilo_ui_base::a11y::Action::ToggleCheck
                if self.can_choose(index) =>
            {
                self.request(index);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
#[path = "radio_group_tests.rs"]
mod tests;
