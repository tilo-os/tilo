//! `PictureChoice`: a choice among pictures, for a setting whose meaning is
//! visual (a text size, a pointer size, a colour filter). Each option is a
//! tile showing its [`Picture`] over its name; the chosen tile carries the
//! selected edge.
//!
//! The tiles fill each line evenly, as many on a line as fit at their
//! narrowest (`picture_tile_min_width`), rounded down to a count that fills
//! every line alike, so a narrow window shows two lines of two rather than
//! three and an orphan. Names are captions, which grow with the text scale.
//!
//! A choice binds like a segmented strip: [`PictureChoice::on_action`]
//! invokes an action with the chosen index (a pane binds it to its
//! setting's edit), and [`PictureChoice::from_state`] keeps the choice in
//! view state both ways. While an action is bound the chosen tile follows
//! the value the action wrote, never the press, so a refused choice is never
//! shown as made.
//!
//! Keyboard: one tab stop; the arrow keys move between tiles (up and down
//! by a line), Home and End to the ends, Enter or Space chooses. Assistive
//! technology sees a group of radio buttons, each with its name, whether it
//! is chosen, its position and where it is drawn, and chooses one with
//! `Activate`.

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::a11y::{AccessNode, AccessState, Role};
use crate::num::{f32_of_f64, f64_of_usize};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{
    ActionEmitter, AppModel, ControlBindingMetadata, ModelPath, TaffyTreeExt, Widget,
    card_column_width, card_columns,
};

pub use super::picture::{ColourFilter, Picture, TouchpadGesture};

/// One option of a [`PictureChoice`]: its picture and its name.
#[derive(Clone, Debug, PartialEq)]
pub struct PictureOption {
    /// What the tile says under its picture, and its accessible name.
    pub label: String,
    pub picture: Picture,
}

impl PictureOption {
    pub fn new(label: impl Into<String>, picture: Picture) -> Self {
        Self {
            label: label.into(),
            picture,
        }
    }
}

/// Two-way storage of the chosen option as the caller's own value.
struct Choice {
    /// The index of the value the state holds now, when it changed.
    pull: Box<dyn FnMut() -> Option<usize>>,
    /// Store the value of a chosen option.
    push: Box<dyn FnMut(usize)>,
}

/// A tile's height as a share of its width.
const TILE_ASPECT: f64 = 0.6;

#[allow(
    clippy::struct_excessive_bools,
    reason = "focus, enabled and how focus came are independent states"
)]
pub struct PictureChoice {
    id: Option<String>,
    options: Vec<PictureOption>,
    selected: Option<usize>,
    /// The keyboard's tile.
    cursor: usize,
    hover: Option<usize>,
    focused: bool,
    enabled: bool,
    accessible_name: Option<String>,
    last_cursor: Point,
    action: Option<ActionEmitter<usize>>,
    choice: Option<Choice>,
    on_change: Option<Box<dyn FnMut(usize)>>,
    rect: Rect,
    /// The theme's narrowest tile, the gap between tiles and the room for
    /// a name, from the last layout.
    narrowest: f64,
    /// The tallest a picture grows, however wide its tile: a wide window
    /// or large text widens the tiles without making the pictures loom.
    tallest: f64,
    gap: f64,
    label: f64,
}

impl PictureChoice {
    /// A choice among `options`, `selected` chosen (`None`: none yet).
    pub fn new(options: Vec<PictureOption>, selected: Option<usize>) -> Self {
        let selected = selected.filter(|index| *index < options.len());
        let metrics = crate::tokens::Metrics::BASE;
        let spacing = crate::tokens::Spacing::BASE;
        Self {
            id: None,
            cursor: selected.unwrap_or_default(),
            options,
            selected,
            hover: None,
            focused: false,
            enabled: true,
            accessible_name: None,
            last_cursor: Point::ZERO,
            action: None,
            choice: None,
            on_change: None,
            rect: Rect::ZERO,
            narrowest: f64::from(metrics.picture_tile_min_width),
            tallest: f64::from(metrics.picture_tile_width) * TILE_ASPECT,
            gap: f64::from(spacing.rhythm.sibling),
            label: f64::from(spacing.s6),
        }
    }

    /// A choice among `choices`, kept in `state`: choosing a tile stores its
    /// value, and a value set anywhere else chooses its tile.
    pub fn from_state<T: Clone + PartialEq + 'static>(
        choices: Vec<(T, PictureOption)>,
        state: &crate::LocalState<T>,
    ) -> Self {
        let (values, options): (Vec<T>, Vec<PictureOption>) = choices.into_iter().unzip();
        let values = std::rc::Rc::new(values);
        let (current, binding) = crate::local_state::LocalBinding::new(state);
        let binding = std::rc::Rc::new(std::cell::RefCell::new(binding));
        let selected = values.iter().position(|value| *value == current);
        let mut picker = Self::new(options, selected);
        let (pull_values, pull_binding) =
            (std::rc::Rc::clone(&values), std::rc::Rc::clone(&binding));
        picker.choice = Some(Choice {
            pull: Box::new(move || {
                let value = pull_binding.borrow_mut().poll()?;
                pull_values.iter().position(|candidate| *candidate == value)
            }),
            push: Box::new(move |index| {
                if let Some(value) = values.get(index) {
                    binding.borrow_mut().publish(value);
                }
            }),
        });
        picker
    }

    /// Box this widget in the SDK so thin apps reuse its shared
    /// implementation.
    #[must_use]
    pub fn into_widget(self) -> Box<dyn Widget> {
        Box::new(self)
    }

    /// Its stable identity, for tests, captures and agents; each tile is
    /// `<id>.<index>`.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// What the choice is called, in the person's words ("Text size").
    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self
    }

    /// Whether the choice takes input; a disabled one is dimmed.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Run `f` with each index the person chooses (unbound choices only).
    #[must_use]
    pub fn on_change(mut self, f: impl FnMut(usize) + 'static) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    /// Invoke `binding` with the chosen index. The chosen tile then follows
    /// the value the action writes, not the press.
    #[must_use]
    pub fn on_action(mut self, binding: crate::tilo_actions::ActionBinding<usize>) -> Self {
        self.action = Some(ActionEmitter::new(binding));
        self
    }

    /// The chosen option, if any.
    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Show `index` as chosen, without telling anyone.
    pub fn set_selected(&mut self, index: Option<usize>) {
        self.selected = index.filter(|index| *index < self.options.len());
    }

    fn option_id(&self, index: usize) -> String {
        format!("{}.{index}", self.id.as_deref().unwrap_or("picture"))
    }

    /// `(columns, tile width, picture height)` across the laid-out width.
    fn geometry(&self) -> (usize, f64, f64) {
        geometry(
            self.rect.width(),
            self.options.len(),
            (self.gap, self.narrowest, self.tallest),
        )
    }

    /// Tile `index`'s picture, without its name.
    fn tile_rect(&self, index: usize) -> Rect {
        let (columns, width, height) = self.geometry();
        let column = index.checked_rem(columns).unwrap_or(0);
        let line = index.checked_div(columns).unwrap_or(0);
        let x = self.rect.x0 + f64_of_usize(column) * (width + self.gap);
        let y = self.rect.y0 + f64_of_usize(line) * (height + self.label + self.gap);
        Rect::new(x, y, x + width, y + height)
    }

    /// Tile `index` with its name: what a pointer or a finger hits.
    fn target_rect(&self, index: usize) -> Rect {
        let picture = self.tile_rect(index);
        Rect::new(picture.x0, picture.y0, picture.x1, picture.y1 + self.label)
    }

    fn tile_at(&self, point: Point) -> Option<usize> {
        (0..self.options.len()).find(|index| self.target_rect(*index).contains(point))
    }

    fn request(&mut self, index: usize) {
        if !self.enabled || index >= self.options.len() {
            return;
        }
        self.cursor = index;
        if self.selected == Some(index) {
            return;
        }
        if let Some(action) = &mut self.action {
            if let Err(error) = action.emit(index) {
                tracing::warn!(%error, "picture choice refused its index");
            }
            return;
        }
        self.selected = Some(index);
        if let Some(choice) = self.choice.as_mut() {
            (choice.push)(index);
        }
        if let Some(on_change) = self.on_change.as_mut() {
            on_change(index);
        }
    }

    fn paint_tile(
        &self,
        scene: &mut Scene,
        transform: Affine,
        index: usize,
        (text, theme): (&mut TextRenderer, &Theme),
    ) {
        let Some(option) = self.options.get(index) else {
            return;
        };
        let colors = &theme.tokens.colors;
        let area = self.tile_rect(index);
        let radius = f64::from(theme.tokens.radii.control);
        let tile = RoundedRect::from_rect(area, radius);
        let ground = colors.surfaces.raised;
        scene.fill(Fill::NonZero, transform, ground, None, &tile);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &tile,
        );
        option.picture.paint(scene, transform, area, theme, text);
        scene.pop_layer();
        let chosen = self.selected == Some(index);
        if chosen {
            crate::surface::selected_edge(scene, transform, area, radius, theme);
        } else {
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                crate::surface::edge_on(ground),
                None,
                &crate::surface::edge_outline(area, radius),
            );
            if self.hover == Some(index) && self.enabled {
                scene.fill(Fill::NonZero, transform, colors.overlay_subtle, None, &tile);
            }
        }
        if self.focused && self.cursor == index && self.enabled {
            crate::widget::activation::paint_focus_ring(scene, transform, area, radius, theme);
        }
        let caption = theme.text(TextRole::Caption);
        let style = if chosen { caption.strong() } else { caption };
        let room = f32_of_f64(area.width());
        let name = text.elide_to_width_style(&option.label, room, &style);
        let (width, _) = text.measure_style(&name, None, &style);
        text.draw_style(
            scene,
            &name,
            if chosen {
                theme.text_primary
            } else {
                theme.text_dim
            },
            transform
                * Affine::translate((
                    area.center().x - f64::from(width) / 2.0,
                    area.y1 + f64::from(theme.tokens.spacing.s2),
                )),
            None,
            &style,
        );
    }

    fn handle_key(&mut self, key: &Key) -> bool {
        let last = self.options.len().saturating_sub(1);
        let columns = self.geometry().0;
        match key {
            Key::Named(NamedKey::ArrowLeft) => self.cursor = self.cursor.saturating_sub(1),
            Key::Named(NamedKey::ArrowRight) => {
                self.cursor = self.cursor.saturating_add(1).min(last);
            }
            Key::Named(NamedKey::ArrowUp) => self.cursor = self.cursor.saturating_sub(columns),
            Key::Named(NamedKey::ArrowDown) => {
                self.cursor = self.cursor.saturating_add(columns).min(last);
            }
            Key::Named(NamedKey::Home) => self.cursor = 0,
            Key::Named(NamedKey::End) => self.cursor = last,
            Key::Named(NamedKey::Enter | NamedKey::Space) => self.request(self.cursor),
            _ => return false,
        }
        true
    }
}

/// How `count` tiles lay out across `width`: as many on a line as fit at
/// their narrowest, rounded down to a count that fills every line alike,
/// sharing the line, each picture no taller than `tallest`.
/// `(columns, tile width, picture height)`.
fn geometry(
    width: f64,
    count: usize,
    (gap, narrowest, tallest): (f64, f64, f64),
) -> (usize, f64, f64) {
    let count = count.max(1);
    let fit = card_columns(width, narrowest, gap).min(count);
    let columns = (1..=fit)
        .rev()
        .find(|columns| count.is_multiple_of(*columns))
        .unwrap_or(fit);
    let tile = card_column_width(width, columns, gap);
    (columns, tile, (tile * TILE_ASPECT).min(tallest).round())
}

impl Widget for PictureChoice {
    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn sync(&mut self, _state: &AppModel, _root: &ModelPath) {
        if let Some(index) = self.choice.as_mut().and_then(|choice| (choice.pull)()) {
            self.set_selected(Some(index));
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // A tile's name is a caption: the tile grows with the text so its
        // name still fits.
        self.narrowest =
            f64::from(theme.tokens.metrics.picture_tile_min_width * theme.text_growth());
        self.tallest = f64::from(theme.tokens.metrics.picture_tile_width) * TILE_ASPECT;
        self.gap = f64::from(theme.tokens.spacing.rhythm.sibling);
        self.label = f64::from(theme.text(TextRole::Caption).line_height_px())
            + f64::from(theme.tokens.spacing.s2);
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            min_size: Size {
                width: Dimension::length(f32_of_f64(self.narrowest)),
                height: Dimension::auto(),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: Size<Option<f32>>,
        available: Size<taffy::AvailableSpace>,
        _text: &mut TextRenderer,
    ) -> Size<f32> {
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(width) => width,
            _ => f32_of_f64((self.narrowest + self.gap) * f64_of_usize(self.options.len())),
        });
        let (columns, _, height) = geometry(
            f64::from(width),
            self.options.len(),
            (self.gap, self.narrowest, self.tallest),
        );
        let lines = self.options.len().div_ceil(columns.max(1));
        Size {
            width,
            height: f32_of_f64(
                (f64_of_usize(lines) * (height + self.label + self.gap) - self.gap).max(0.0),
            ),
        }
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if !self.enabled {
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                transform,
                &self.rect,
            );
        }
        for index in 0..self.options.len() {
            self.paint_tile(scene, transform, index, (text, theme));
        }
        if !self.enabled {
            scene.pop_layer();
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = ctx.logical(position);
                let hover = self.tile_at(self.last_cursor);
                if hover != self.hover {
                    self.hover = hover;
                    *ctx.handled = true;
                }
            }
            WindowEvent::CursorLeft { .. } if self.hover.is_some() => {
                self.hover = None;
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.enabled => {
                if let Some(index) = self.tile_at(self.last_cursor) {
                    self.request(index);
                    *ctx.focus_target = Some(ctx.node_id);
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

    fn name(&self) -> &'static str {
        "PictureChoice"
    }

    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        (self.enabled && self.tile_at(point).is_some())
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn tooltip_target_at(&self, _point: Point) -> Option<(Rect, String)> {
        None
    }

    fn is_focusable(&self) -> bool {
        self.enabled && !self.options.is_empty()
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if focused {
            self.cursor = self
                .selected
                .unwrap_or(self.cursor)
                .min(self.options.len().saturating_sub(1));
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let count = self.options.len();
        let children = self
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                AccessNode::new(Role::RadioButton)
                    .with_action_target(self.option_id(index))
                    .with_name(option.label.clone())
                    .with_bounds(self.target_rect(index))
                    .with_state(
                        AccessState::new()
                            .checked(self.selected == Some(index))
                            .focused(self.focused && self.cursor == index)
                            .disabled(!self.enabled),
                    )
                    .with_position(index.saturating_add(1), count)
            })
            .collect();
        Some(
            AccessNode::new(Role::Group)
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
                .with_children(children),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index] = path else {
            return None;
        };
        (*index < self.options.len()).then(|| self.target_rect(*index))
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        let prefix = format!("{}.", self.id.as_deref().unwrap_or("picture"));
        let Some(index) = target
            .strip_prefix(&prefix)
            .and_then(|index| index.parse::<usize>().ok())
            .filter(|index| *index < self.options.len())
        else {
            return false;
        };
        match action {
            crate::a11y::Action::Focus => {
                self.focused = true;
                self.cursor = index;
                true
            }
            crate::a11y::Action::Activate | crate::a11y::Action::ToggleCheck if self.enabled => {
                self.request(index);
                true
            }
            _ => false,
        }
    }
}

impl crate::widget::controls::ActionControls for PictureChoice {
    crate::widget::controls::optional_emitter_controls!();
    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        match &self.action {
            Some(action) => (0..self.options.len())
                .map(|index| action.metadata(self.option_id(index)))
                .collect(),
            // A choice kept in view state is made through the tiles' own
            // semantic actions.
            None if self.choice.is_some() => (0..self.options.len())
                .map(|index| ControlBindingMetadata {
                    control_id: self.option_id(index),
                    kind: crate::widget::ControlBindingKind::SemanticAction(self.option_id(index)),
                })
                .collect(),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
#[path = "picture_choice_tests.rs"]
mod tests;
