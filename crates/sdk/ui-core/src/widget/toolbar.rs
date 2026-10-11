//! The toolbar: a measured strip of commands with a shared `ContextMenu`
//! for overflow, so every command stays reachable however narrow the window.
//! The extent cache is the sole source for intrinsic sizing, fitting and placement.
//!
//! Commands are `Label`-role plain buttons; a toggle that is on is a lift;
//! groups are split by hairline separators that never stand at the start or
//! end of what is shown. A command can be disabled (dimmed, announced as
//! disabled, inert), carry a dot (something new behind it), open a menu of
//! its own, or rank above its neighbours for staying out of the More menu
//! ([`Toolbar::with_priority`]). Hosted controls (a segmented strip, a
//! number field, a colour well) stand in the row with
//! [`Toolbar::push_element`]. A painted widget hosts one with
//! [`Toolbar::lay_out_in`] (or [`Toolbar::lay_out_at`] once it has been
//! measured). The band behind a window's toolbar row is [`paint_band`]
//! with its controls inset by [`band_padding`].
use crate::num::{f32_of_f64, f64_of_usize};
use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::context_menu::{ContextMenu, MenuItem};
use crate::a11y::{AccessNode, AccessState, Action, Role};
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{TaffyTreeExt, Widget};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Orientation {
    Horizontal,
    Vertical,
}

/// One command of a toolbar and what it carries.
pub struct ToolbarCommand {
    /// The command's visible label, or the accessible name and tooltip of
    /// an icon-only command.
    pub label: String,
    /// Shown instead of the label when present.
    pub icon: Option<crate::icons::Icon>,
    /// A colour well shown instead of a label or icon (the colour the next
    /// mark takes); the label is its accessible name and tooltip.
    pub swatch: Option<vello::peniko::Color>,
    /// `Some` for a toggle; `Some(true)` paints the lift.
    pub active: Option<bool>,
    /// What assistive technology and the More menu call it, when that
    /// differs from the label (a toggle labelled "Aa" is "Match case").
    pub accessible_name: Option<String>,
    /// A disabled command is dimmed, announced as disabled and inert.
    pub enabled: bool,
    /// A dot at the command's corner: something new behind it.
    pub dot: bool,
    /// Higher stays visible longer when the row is too narrow; equal
    /// priorities give way from the end.
    pub priority: i32,
    /// `Some` opens a menu anchored to the command instead of running it.
    pub menu: Option<Vec<MenuItem>>,
    /// Goes with the command before it: the two (zoom out, zoom in) give way
    /// to the More menu together, never one without the other.
    pub joined: bool,
    pub on_click: Box<dyn FnMut()>,
    action: Option<super::ActionEmitter<()>>,
    native: Option<(String, super::activation::NativeControl)>,
}

impl ToolbarCommand {
    fn new(label: &str, on_click: Box<dyn FnMut()>) -> Self {
        Self {
            label: label.to_string(),
            icon: None,
            swatch: None,
            active: None,
            accessible_name: None,
            enabled: true,
            dot: false,
            priority: 0,
            menu: None,
            joined: false,
            on_click,
            action: None,
            native: None,
        }
    }

    fn is_enabled(&self) -> bool {
        self.enabled
            && self
                .action
                .as_ref()
                .is_none_or(super::ActionEmitter::is_available)
    }

    /// The name it is announced and listed by.
    fn name(&self) -> &str {
        self.accessible_name.as_deref().unwrap_or(&self.label)
    }
}

pub enum ToolbarItem {
    Button(ToolbarCommand),
    Separator,
    Spacer,
    /// A control hosted in the row: it lays itself out, paints and takes
    /// focus like anywhere else. It never goes to the More menu.
    Element(Box<dyn Widget>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Item(usize),
    Overflow,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent focus, capture and invalidation flags"
)]
pub struct Toolbar {
    items: Vec<ToolbarItem>,
    orientation: Orientation,
    rect: Rect,
    item_rects: Vec<Rect>,
    /// One past the last shown item.
    visible_count: usize,
    /// Commands that went to the More menu.
    hidden: Vec<bool>,
    extents: Vec<Size<f32>>,
    chip_extent: Size<f32>,
    intrinsic: Size<f32>,
    font: TextStyle,
    padding: f32,
    minimum: f32,
    separator: f32,
    /// Main-axis gap between siblings (`spacing.s3`, HIG §7b).
    gap: f32,
    overflow_rect: Rect,
    overflow_native: super::activation::NativeControl,
    overflow_id: String,
    overflow_action: super::ActionEmitter<()>,
    overflow_requested: std::rc::Rc<std::cell::Cell<bool>>,
    overflow_keyboard: bool,
    hovered: Option<Target>,
    pointer: Option<Point>,
    pressed: Option<Target>,
    focused: bool,
    focus: Option<Target>,
    menu: ContextMenu,
    menu_indices: Vec<Option<usize>>,
    /// A command's own menu, open, and the command it belongs to.
    item_menu: Option<(usize, ContextMenu)>,
    viewport: Option<Rect>,
    swallow_release: bool,
    /// The last command run, drained by [`Toolbar::take_activated`].
    activated: Option<usize>,
    /// A label, an item or the theme changed since the last placement.
    layout_dirty: bool,
    /// The text scale the extents were measured at.
    measured_scale: Option<f32>,
    /// Element nodes laid out apart from a host (a painted host).
    detached: Vec<Option<super::floating_surface::DetachedContent>>,
    theme: Theme,
}

impl Toolbar {
    pub fn new(orientation: Orientation) -> Self {
        let overflow_requested = std::rc::Rc::new(std::cell::Cell::new(false));
        let request = std::rc::Rc::clone(&overflow_requested);
        let overflow_id = format!("toolbar.overflow:{}", uuid::Uuid::new_v4());
        let (overflow_native, binding) = super::activation::NativeControl::new(
            &overflow_id,
            "More",
            tilo_actions::OperationKind::View,
            Box::new(move || request.set(true)),
        );
        Self {
            items: Vec::new(),
            orientation,
            rect: Rect::ZERO,
            item_rects: Vec::new(),
            visible_count: 0,
            hidden: Vec::new(),
            extents: Vec::new(),
            chip_extent: Size::ZERO,
            intrinsic: Size::ZERO,
            font: crate::tokens::typography::stock().style(TextRole::Label),
            padding: 8.0,
            // Replaced by `build_layout` from `theme.tokens.metrics`; the
            // unscaled reference keeps a renderer-less caller's estimate
            // consistent with the real one.
            minimum: crate::tokens::Metrics::BASE.min_pointer_target,
            // Replaced by `build_layout`; the default-token value keeps a
            // renderer-less caller's estimate consistent with the real one.
            separator: SEPARATOR_HAIRLINE,
            gap: crate::tokens::Spacing::default().s3,
            overflow_rect: Rect::ZERO,
            overflow_native,
            overflow_id,
            overflow_action: super::ActionEmitter::new(binding),
            overflow_requested,
            overflow_keyboard: true,
            hovered: None,
            pointer: None,
            pressed: None,
            focused: false,
            focus: None,
            menu: ContextMenu::new(),
            menu_indices: Vec::new(),
            item_menu: None,
            viewport: None,
            swallow_release: false,
            activated: None,
            layout_dirty: true,
            measured_scale: None,
            detached: Vec::new(),
            theme: Theme::dark(),
        }
    }

    fn push_command(mut self, command: ToolbarCommand) -> Self {
        self.items.push(ToolbarItem::Button(command));
        self.layout_dirty = true;
        self
    }

    #[must_use]
    pub fn push_button(self, label: &str, on_click: impl FnMut() + 'static) -> Self {
        self.push_command(ToolbarCommand::new(label, Box::new(on_click)))
    }
    /// An icon-only command; `label` is its accessible name, its tooltip and
    /// its row in the overflow menu.
    #[must_use]
    pub fn push_icon(
        self,
        icon: crate::icons::Icon,
        label: &str,
        on_click: impl FnMut() + 'static,
    ) -> Self {
        let mut command = ToolbarCommand::new(label, Box::new(on_click));
        command.icon = Some(icon);
        self.push_command(command)
    }
    /// A command that is on or off (bold, a panel's visibility): a lift
    /// while on, a checked row in the overflow menu.
    #[must_use]
    pub fn push_toggle(self, label: &str, active: bool, on_click: impl FnMut() + 'static) -> Self {
        let mut command = ToolbarCommand::new(label, Box::new(on_click));
        command.active = Some(active);
        self.push_command(command)
    }
    /// An icon-only toggle (a panel's visibility, a view mode); `name` is
    /// its accessible name, tooltip and overflow row.
    #[must_use]
    pub fn push_icon_toggle(
        self,
        icon: crate::icons::Icon,
        name: &str,
        active: bool,
        on_click: impl FnMut() + 'static,
    ) -> Self {
        let mut command = ToolbarCommand::new(name, Box::new(on_click));
        command.icon = Some(icon);
        command.active = Some(active);
        self.push_command(command)
    }
    /// A colour well (a marker or pen colour): the colour shows in the row,
    /// pressing it runs `on_click` (open a picker); `name` is its accessible
    /// name, tooltip and overflow row.
    #[must_use]
    pub fn push_swatch(
        self,
        color: vello::peniko::Color,
        name: &str,
        on_click: impl FnMut() + 'static,
    ) -> Self {
        let mut command = ToolbarCommand::new(name, Box::new(on_click));
        command.swatch = Some(color);
        self.push_command(command)
    }
    /// A command that opens `items` as a menu anchored to it (Sort, Insert,
    /// View). In the More menu it is a row with the same nested menu.
    #[must_use]
    pub fn push_menu(self, label: &str, items: Vec<MenuItem>) -> Self {
        let mut command = ToolbarCommand::new(label, Box::new(|| {}));
        command.menu = Some(items);
        self.push_command(command)
    }
    /// A control hosted in the row (a segmented strip, a number field, a
    /// colour well). It keeps its own size and never goes to the More menu.
    #[must_use]
    pub fn push_element(mut self, element: Box<dyn Widget>) -> Self {
        self.items.push(ToolbarItem::Element(element));
        self.layout_dirty = true;
        self
    }

    /// Bind the last pushed command to the same admitted action in the row
    /// and overflow menu. The callback is not run for a bound command.
    #[must_use]
    pub fn with_action(mut self, binding: tilo_actions::ActionBinding<()>) -> Self {
        if let Some(command) = self.last_command() {
            command.action = Some(super::ActionEmitter::new(binding));
            command.native = None;
        }
        self
    }

    /// Publish the last command's callback as an admitted transient view action.
    #[must_use]
    pub fn with_view(self, id: &str) -> Self {
        self.with_native(id, tilo_actions::OperationKind::View)
    }

    /// Publish the last command's callback as an admitted local effect.
    #[must_use]
    pub fn with_effect(self, id: &str) -> Self {
        self.with_native(id, tilo_actions::OperationKind::Effect)
    }

    fn with_native(mut self, id: &str, kind: tilo_actions::OperationKind) -> Self {
        if let Some(command) = self.last_command() {
            let callback = std::mem::replace(&mut command.on_click, Box::new(|| {}));
            let (native, binding) =
                super::activation::NativeControl::new(id, command.name(), kind, callback);
            command.native = Some((id.to_owned(), native));
            command.action = Some(super::ActionEmitter::new(binding));
        }
        self
    }

    fn last_command(&mut self) -> Option<&mut ToolbarCommand> {
        match self.items.last_mut() {
            Some(ToolbarItem::Button(command)) => Some(command),
            _ => None,
        }
    }

    fn command(&self, index: usize) -> Option<&ToolbarCommand> {
        match self.items.get(index) {
            Some(ToolbarItem::Button(command)) => Some(command),
            _ => None,
        }
    }

    fn command_mut(&mut self, index: usize) -> Option<&mut ToolbarCommand> {
        match self.items.get_mut(index) {
            Some(ToolbarItem::Button(command)) => Some(command),
            _ => None,
        }
    }

    /// The last pushed command is announced and listed as `name` (a toggle
    /// labelled "Aa" named "Match case").
    #[must_use]
    pub fn with_accessible_name(mut self, name: &str) -> Self {
        if let Some(command) = self.last_command() {
            command.accessible_name = Some(name.to_string());
        }
        self
    }

    /// The last pushed command stays visible ahead of lower priorities when
    /// the row is too narrow (default 0; equal priorities give way from the
    /// end).
    #[must_use]
    pub fn with_priority(mut self, priority: i32) -> Self {
        if let Some(command) = self.last_command() {
            command.priority = priority;
        }
        self
    }

    /// The last pushed command goes with the command before it: they give
    /// way to the More menu together (zoom out and zoom in), so overflow
    /// never leaves one of the pair alone.
    #[must_use]
    pub fn joined(mut self) -> Self {
        if let Some(command) = self.last_command() {
            command.joined = true;
        }
        self
    }

    /// The last pushed command starts disabled.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        if let Some(command) = self.last_command() {
            command.enabled = enabled;
        }
        self
    }

    /// Turn the toggle at `index` on or off (the host syncing from its model).
    pub fn set_active(&mut self, index: usize, on: bool) {
        if let Some(command) = self.command_mut(index)
            && command.active.is_some()
        {
            command.active = Some(on);
            self.menu_indices.clear();
            self.reconcile_menu();
        }
    }

    /// Enable or disable the command at `index`: a disabled command is
    /// dimmed, announced as disabled, and neither the pointer nor the
    /// keyboard runs it.
    pub fn set_enabled(&mut self, index: usize, enabled: bool) {
        if let Some(command) = self.command_mut(index)
            && command.enabled != enabled
        {
            command.enabled = enabled;
            self.menu_indices.clear();
            self.reconcile_menu();
        }
    }

    /// Replace the label of the command at `index` (a zoom level, a count).
    pub fn set_label(&mut self, index: usize, label: &str) {
        if let Some(command) = self.command_mut(index)
            && command.label != label
        {
            label.clone_into(&mut command.label);
            self.layout_dirty = true;
            self.measured_scale = None;
            self.menu_indices.clear();
            self.reconcile_menu();
        }
    }

    /// Show `color` in the colour well at `index`, named `name`.
    pub fn set_swatch(&mut self, index: usize, color: vello::peniko::Color, name: &str) {
        if let Some(command) = self.command_mut(index)
            && command.swatch.is_some()
        {
            command.swatch = Some(color);
            if command.label != name {
                name.clone_into(&mut command.label);
                self.menu_indices.clear();
                self.reconcile_menu();
            }
        }
    }

    /// Replace the accessible name of the command at `index`.
    pub fn set_accessible_name(&mut self, index: usize, name: &str) {
        if let Some(command) = self.command_mut(index) {
            command.accessible_name = Some(name.to_string());
            self.menu_indices.clear();
            self.reconcile_menu();
        }
    }

    /// Show or hide the dot at the corner of the command at `index`.
    pub fn set_dot(&mut self, index: usize, dot: bool) {
        if let Some(command) = self.command_mut(index) {
            command.dot = dot;
        }
    }

    /// Replace the rows of the menu the command at `index` opens.
    pub fn set_menu(&mut self, index: usize, items: Vec<MenuItem>) {
        if let Some(command) = self.command_mut(index) {
            command.menu = Some(items.clone());
        }
        if let Some((open, menu)) = self.item_menu.as_mut()
            && *open == index
        {
            menu.set_items(items);
        }
    }

    /// The command run last (by pointer, keyboard, the More menu or an
    /// assistive action), once. Poll it after routing an event when the
    /// host keeps its state in `&mut self` rather than in the callbacks.
    pub fn take_activated(&mut self) -> Option<usize> {
        self.activated.take()
    }

    /// Open the More menu (a stage hook that shows what overflowed).
    pub fn open_overflow(&mut self) {
        self.open_menu(false);
    }

    /// Open the menu of the command at `index`, anchored to it (a stage
    /// hook that shows a command's menu, like [`Self::open_overflow`]).
    pub fn open_command_menu(&mut self, index: usize) {
        if self.is_shown(index) {
            self.open_item_menu(index, false);
        }
    }

    /// The menu the toolbar has open, if any: a command's own menu with the
    /// command's index, or the More menu with `None`.
    pub fn shown_menu(&self) -> Option<(Option<usize>, &ContextMenu)> {
        if let Some((index, menu)) = &self.item_menu {
            return Some((Some(*index), menu));
        }
        self.menu.is_open().then_some((None, &self.menu))
    }

    /// Close whatever menu the toolbar has open: another surface (a
    /// palette, a sheet) takes over the window.
    pub fn close_menus(&mut self) {
        self.menu.close();
        self.item_menu = None;
        self.pressed = None;
    }

    /// Where the item at `index` stands, while it is shown in the row.
    pub fn item_rect(&self, index: usize) -> Option<Rect> {
        self.item_rects
            .get(index)
            .copied()
            .filter(|rect| !rect.is_zero_area())
    }

    /// The size the whole row wants: every item, at the theme's metrics.
    pub fn intrinsic_size(&mut self, text: &mut TextRenderer, theme: &Theme) -> vello::kurbo::Size {
        self.resolve_theme(theme);
        self.update_measurements(text);
        vello::kurbo::Size::new(
            f64::from(self.intrinsic.width),
            f64::from(self.intrinsic.height),
        )
    }

    /// Lay the toolbar out in `rect` without a taffy pass, for a painted
    /// widget that hosts one in its own chrome. Measures with `text` in
    /// `theme`, then fits and places the commands; draw and dispatch as
    /// usual afterwards.
    pub fn lay_out_in(&mut self, rect: Rect, text: &mut TextRenderer, theme: &Theme) {
        self.resolve_theme(theme);
        self.measure_detached_elements(text, theme);
        self.update_measurements(text);
        self.measured_scale = Some(theme.text_scale);
        self.rect = rect;
        self.place();
        self.place_detached_elements();
    }

    /// [`Self::lay_out_in`] from a layout pass that has no renderer: the
    /// cached measurements place the commands, and they are measured only
    /// when a label or the text scale changed since.
    pub fn lay_out_at(&mut self, rect: Rect, theme: &Theme) {
        if self.measured_scale != Some(theme.text_scale) || self.extents.len() != self.items.len() {
            crate::text_render::with_layout_renderer(theme.text_scale, |text| {
                self.lay_out_in(rect, text, theme);
            });
            return;
        }
        self.resolve_theme(theme);
        self.rect = rect;
        self.place();
        self.place_detached_elements();
    }

    /// Whether the separator at `index` is shown: only between two shown
    /// commands, never first, last, or beside another separator.
    fn separator_shown(&self, index: usize) -> bool {
        self.separator_shown_in(index, self.visible_count, &self.hidden)
    }

    fn separator_shown_in(&self, index: usize, visible: usize, hidden: &[bool]) -> bool {
        let shown = |i: usize| i < visible && !hidden.get(i).copied().unwrap_or(false);
        let is_control = |i: usize, item: &ToolbarItem| {
            shown(i) && matches!(item, ToolbarItem::Button(_) | ToolbarItem::Element(_))
        };
        let before = self
            .items
            .iter()
            .enumerate()
            .take(index)
            .rev()
            .find(|(i, item)| {
                !matches!(item, ToolbarItem::Spacer)
                    && (shown(*i) || !matches!(item, ToolbarItem::Button(_)))
            })
            .is_some_and(|(i, item)| is_control(i, item));
        let after = self
            .items
            .iter()
            .enumerate()
            .skip(index.saturating_add(1))
            .any(|(i, item)| is_control(i, item));
        index < visible && before && after
    }
    #[must_use]
    pub fn push_separator(mut self) -> Self {
        self.items.push(ToolbarItem::Separator);
        self.layout_dirty = true;
        self
    }
    #[must_use]
    pub fn push_spacer(mut self) -> Self {
        self.items.push(ToolbarItem::Spacer);
        self.layout_dirty = true;
        self
    }

    /// Whether the command at `index` is in the row (not in the More menu).
    fn is_shown(&self, index: usize) -> bool {
        index < self.visible_count && !self.hidden.get(index).copied().unwrap_or(false)
    }

    pub fn has_overflow(&self) -> bool {
        self.items
            .iter()
            .enumerate()
            .any(|(index, item)| matches!(item, ToolbarItem::Button(_)) && !self.is_shown(index))
    }
    pub fn visible_count(&self) -> usize {
        self.visible_count
    }
    pub fn separator_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| matches!(item, ToolbarItem::Separator))
            .count()
    }
    pub fn is_overflow_open(&self) -> bool {
        self.menu.is_open()
    }

    /// Compatibility estimate for callers without a renderer. Real lifecycle layout
    /// always replaces this decision with the measured extent cache.
    pub fn recompute_visible(&mut self, axis_extent: f64, theme: &Theme) {
        let mut used = 0.0;
        let axes: Vec<f64> = self
            .items
            .iter()
            .map(|item| match item {
                ToolbarItem::Spacer => 0.0,
                ToolbarItem::Separator => f64::from(separator_extent(theme)),
                ToolbarItem::Element(element) => {
                    let rect = element.get_rect();
                    match self.orientation {
                        Orientation::Horizontal => rect.width(),
                        Orientation::Vertical => rect.height(),
                    }
                }
                ToolbarItem::Button(command) => {
                    let text = match self.orientation {
                        Orientation::Horizontal => {
                            f64_of_usize(command.label.chars().count()) * 7.0
                        }
                        Orientation::Vertical => {
                            f64::from(theme.tokens.typography.size(TextRole::Caption))
                        }
                    };
                    (text + 2.0 * f64::from(theme.tokens.spacing.s3))
                        .max(f64::from(theme.tokens.metrics.min_pointer_target))
                }
            })
            .collect();
        let gap = f64::from(theme.tokens.spacing.s3);
        let total = axes.iter().sum::<f64>() + gap * f64_of_usize(axes.len().saturating_sub(1));
        let budget = if total <= axis_extent {
            axis_extent
        } else {
            (axis_extent - 24.0 - gap).max(0.0)
        };
        self.visible_count = axes
            .iter()
            .enumerate()
            .take_while(|(index, axis)| {
                used += **axis + if *index > 0 { gap } else { 0.0 };
                used <= budget
            })
            .count();
        self.hidden = vec![false; self.items.len()];
    }

    fn axis(&self, size: Size<f32>) -> f64 {
        f64::from(match self.orientation {
            Orientation::Horizontal => size.width,
            Orientation::Vertical => size.height,
        })
    }

    fn update_measurements(&mut self, text: &mut TextRenderer) -> bool {
        let style = self.font.clone();
        let button = |label: &str, text: &mut TextRenderer| {
            let (width, height) = text.measure_style(label, None, &style);
            Size {
                width: (width + self.padding * 2.0).ceil().max(self.minimum),
                height: (height + self.padding * 2.0).max(self.minimum),
            }
        };
        let square = Size {
            width: self.minimum,
            height: self.minimum,
        };
        let chip = square;
        let previous = std::mem::take(&mut self.extents);
        let extents: Vec<_> = self
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| match item {
                ToolbarItem::Button(command)
                    if command.icon.is_some() || command.swatch.is_some() =>
                {
                    square
                }
                ToolbarItem::Button(command) => button(&command.label, text),
                ToolbarItem::Spacer => Size::ZERO,
                // A hosted control's size comes from its own layout.
                ToolbarItem::Element(_) => previous.get(index).copied().unwrap_or(square),
                ToolbarItem::Separator => match self.orientation {
                    Orientation::Horizontal => Size {
                        width: self.separator,
                        height: self.minimum,
                    },
                    Orientation::Vertical => Size {
                        width: self.minimum,
                        height: self.separator,
                    },
                },
            })
            .collect();
        let changed = extents != previous || chip != self.chip_extent;
        self.extents = extents;
        self.chip_extent = chip;
        self.recompute_intrinsic();
        changed
    }

    fn recompute_intrinsic(&mut self) {
        self.intrinsic =
            self.extents
                .iter()
                .fold(Size::ZERO, |size, item| match self.orientation {
                    Orientation::Horizontal => Size {
                        width: size.width + item.width,
                        height: size.height.max(item.height),
                    },
                    Orientation::Vertical => Size {
                        width: size.width.max(item.width),
                        height: size.height + item.height,
                    },
                });
        // Siblings sit `gap` apart (HIG §7b): one gap per boundary.
        let gaps = self.gap * count_to_f32(self.items.len().saturating_sub(1));
        match self.orientation {
            Orientation::Horizontal => self.intrinsic.width += gaps,
            Orientation::Vertical => self.intrinsic.height += gaps,
        }
    }

    /// The main-axis extent the row takes with `hidden` commands in the
    /// More menu (spacers at zero).
    fn used(&self, hidden: &[bool]) -> f64 {
        let gap = f64::from(self.gap);
        let visible = self.items.len();
        let shown_control = |index: usize, item: &ToolbarItem| match item {
            ToolbarItem::Button(_) => !hidden.get(index).copied().unwrap_or(false),
            ToolbarItem::Element(_) => true,
            ToolbarItem::Separator | ToolbarItem::Spacer => false,
        };
        // Nothing after the last shown control takes room.
        let end = self
            .items
            .iter()
            .enumerate()
            .rev()
            .find(|(index, item)| shown_control(*index, item))
            .map_or(0, |(index, _)| index + 1);
        let mut count = 0usize;
        let mut used = 0.0;
        for (index, item) in self.items.iter().enumerate().take(end) {
            let present = match item {
                ToolbarItem::Button(_) | ToolbarItem::Element(_) => shown_control(index, item),
                ToolbarItem::Separator => self.separator_shown_in(index, visible, hidden),
                ToolbarItem::Spacer => true,
            };
            if present {
                used += self.extents.get(index).map_or(0.0, |size| self.axis(*size));
                count += 1;
            }
        }
        used + gap * f64_of_usize(count.saturating_sub(1))
    }

    /// Which commands give way to the More menu in `axis_total`: none when
    /// the row fits; otherwise the lowest priority first, from the end
    /// among equals, until the rest and the More button fit.
    fn fit(&self, axis_total: f64) -> Vec<bool> {
        let mut hidden = vec![false; self.items.len()];
        if self.used(&hidden) <= axis_total {
            return hidden;
        }
        let budget = (axis_total - self.axis(self.chip_extent) - f64::from(self.gap)).max(0.0);
        // Each pass hides at least one more command, so this ends.
        loop {
            if self.used(&hidden) <= budget {
                break;
            }
            let candidate = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| match item {
                    ToolbarItem::Button(command) if !hidden.get(index).copied().unwrap_or(true) => {
                        Some((command.priority, index))
                    }
                    _ => None,
                })
                .min_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
            let Some((_, index)) = candidate else { break };
            for member in self.joined_group(index) {
                if let Some(slot) = hidden.get_mut(member) {
                    *slot = true;
                }
            }
        }
        hidden
    }

    /// The command at `index` and every command joined to it, through the
    /// commands before and after it ([`Self::joined`]).
    fn joined_group(&self, index: usize) -> Vec<usize> {
        let commands: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| matches!(item, ToolbarItem::Button(_)))
            .map(|(i, _)| i)
            .collect();
        let Some(at) = commands.iter().position(|&i| i == index) else {
            return vec![index];
        };
        let joined = |i: usize| self.command(i).is_some_and(|c| c.joined);
        let mut start = at;
        while start > 0 && commands.get(start).is_some_and(|&i| joined(i)) {
            start -= 1;
        }
        let mut end = at;
        while commands.get(end + 1).is_some_and(|&i| joined(i)) {
            end += 1;
        }
        commands
            .get(start..=end)
            .map(<[usize]>::to_vec)
            .unwrap_or_default()
    }

    fn place(&mut self) {
        let axis_total = match self.orientation {
            Orientation::Horizontal => self.rect.width(),
            Orientation::Vertical => self.rect.height(),
        }
        .max(0.0);
        let gap = f64::from(self.gap);
        self.hidden = self.fit(axis_total);
        // One past the last item that stands in the row.
        self.visible_count = self
            .items
            .iter()
            .enumerate()
            .rev()
            .find(|(index, item)| match item {
                ToolbarItem::Button(_) => !self.hidden.get(*index).copied().unwrap_or(false),
                ToolbarItem::Element(_) => true,
                ToolbarItem::Separator | ToolbarItem::Spacer => false,
            })
            .map_or(0, |(index, _)| index + 1);
        let overflow = self.has_overflow();

        let chip = if overflow {
            self.axis(self.chip_extent).min(axis_total)
        } else {
            0.0
        };
        let present: Vec<bool> = (0..self.items.len())
            .map(|index| match self.items.get(index) {
                Some(ToolbarItem::Button(_)) => self.is_shown(index),
                Some(ToolbarItem::Separator) => self.separator_shown(index),
                Some(ToolbarItem::Spacer | ToolbarItem::Element(_)) => index < self.visible_count,
                None => false,
            })
            .collect();
        let count = present.iter().filter(|p| **p).count();
        let used: f64 = self
            .extents
            .iter()
            .zip(&present)
            .filter(|(_, p)| **p)
            .map(|(size, _)| self.axis(*size))
            .sum::<f64>()
            + gap * f64_of_usize(count.saturating_sub(1));
        let spacers = self
            .items
            .iter()
            .zip(&present)
            .filter(|(item, p)| **p && matches!(item, ToolbarItem::Spacer))
            .count();
        let chip_gap = if overflow { gap } else { 0.0 };
        let spacer = if spacers == 0 {
            0.0
        } else {
            (axis_total - used - chip - chip_gap).max(0.0) / f64_of_usize(spacers)
        };
        self.item_rects = vec![Rect::ZERO; self.items.len()];
        let mut cursor = match self.orientation {
            Orientation::Horizontal => self.rect.x0,
            Orientation::Vertical => self.rect.y0,
        };
        for (index, item) in self.items.iter().enumerate() {
            if !present.get(index).copied().unwrap_or(false) {
                continue;
            }
            let extent = if matches!(item, ToolbarItem::Spacer) {
                spacer
            } else {
                self.extents.get(index).map_or(0.0, |size| self.axis(*size))
            };
            let rect = match self.orientation {
                Orientation::Horizontal => {
                    Rect::new(cursor, self.rect.y0, cursor + extent, self.rect.y1)
                }
                Orientation::Vertical => {
                    Rect::new(self.rect.x0, cursor, self.rect.x1, cursor + extent)
                }
            };
            // A hosted control keeps its own cross size, centred in the row.
            let rect = match (item, self.extents.get(index)) {
                (ToolbarItem::Element(_), Some(size)) => match self.orientation {
                    Orientation::Horizontal => {
                        let h = f64::from(size.height).min(rect.height());
                        let y = rect.center().y - h / 2.0;
                        Rect::new(rect.x0, y, rect.x1, y + h)
                    }
                    Orientation::Vertical => {
                        let w = f64::from(size.width).min(rect.width());
                        let x = rect.center().x - w / 2.0;
                        Rect::new(x, rect.y0, x + w, rect.y1)
                    }
                },
                _ => rect,
            };
            if let Some(slot) = self.item_rects.get_mut(index) {
                *slot = rect;
            }
            cursor += extent + gap;
        }
        self.overflow_rect = if overflow {
            match self.orientation {
                Orientation::Horizontal => Rect::new(
                    self.rect.x1 - chip,
                    self.rect.y0,
                    self.rect.x1,
                    self.rect.y1,
                ),
                Orientation::Vertical => Rect::new(
                    self.rect.x0,
                    self.rect.y1 - chip,
                    self.rect.x1,
                    self.rect.y1,
                ),
            }
        } else {
            Rect::ZERO
        };
        self.reconcile_menu();
        let targets = self.targets();
        if self.focus.is_some_and(|target| !targets.contains(&target)) {
            self.focus = targets.last().copied();
        }
        self.refresh_hover();
        self.pressed = self.pressed.filter(|target| targets.contains(target));
        self.layout_dirty = false;
    }

    /// Lay hosted controls out in their own trees, for a painted host.
    fn measure_detached_elements(&mut self, text: &mut TextRenderer, theme: &Theme) {
        let available = Size {
            width: taffy::AvailableSpace::MaxContent,
            height: taffy::AvailableSpace::MaxContent,
        };
        self.detached.resize_with(self.items.len(), || None);
        for (index, item) in self.items.iter_mut().enumerate() {
            let ToolbarItem::Element(element) = item else {
                continue;
            };
            let content = super::floating_surface::DetachedContent::lay_out(
                element.as_mut(),
                theme,
                available,
                text,
            );
            let (w, h) = content.size();
            if let Some(slot) = self.extents.get_mut(index) {
                *slot = Size {
                    width: f32_of_f64(w),
                    height: f32_of_f64(h),
                };
            } else {
                self.extents.resize(
                    index,
                    Size {
                        width: self.minimum,
                        height: self.minimum,
                    },
                );
                self.extents.push(Size {
                    width: f32_of_f64(w),
                    height: f32_of_f64(h),
                });
            }
            if let Some(slot) = self.detached.get_mut(index) {
                *slot = Some(content);
            }
        }
    }

    /// Place the hosted controls of a painted host at their rects.
    fn place_detached_elements(&mut self) {
        for (index, item) in self.items.iter_mut().enumerate() {
            let (ToolbarItem::Element(element), Some(Some(content)), Some(rect)) =
                (item, self.detached.get(index), self.item_rects.get(index))
            else {
                continue;
            };
            element.apply_layout(&content.taffy, content.node, rect.origin());
        }
    }

    fn reconcile_menu(&mut self) {
        let mut indices = Vec::new();
        let mut separator = false;
        for (index, item) in self.items.iter().enumerate() {
            match item {
                ToolbarItem::Button(_) if !self.is_shown(index) => {
                    if separator && !indices.is_empty() {
                        indices.push(None);
                    }
                    indices.push(Some(index));
                    separator = false;
                }
                ToolbarItem::Separator => separator = true,
                ToolbarItem::Button(_) | ToolbarItem::Spacer | ToolbarItem::Element(_) => {}
            }
        }
        if indices != self.menu_indices {
            let open = self.menu.is_open();
            let focused_index = self
                .menu
                .hovered()
                .and_then(|row| self.menu_indices.get(row).copied().flatten());
            self.menu = ContextMenu::new();
            for index in &indices {
                self.menu
                    .push(match index.and_then(|index| self.command(index)) {
                        Some(command) => {
                            let mut row = match &command.menu {
                                Some(items) => MenuItem::submenu(command.name(), items.clone()),
                                None => MenuItem::action(command.name(), || {}),
                            };
                            if let Some(icon) = command.icon {
                                row = row.with_icon(icon);
                            } else if command.swatch.is_some() {
                                row = row.with_icon(crate::icons::Icon::Palette);
                            }
                            // A toggle's row shows its state with the check.
                            if let Some(active) = command.active {
                                row = row.with_checked(active);
                            }
                            row.with_enabled(command.is_enabled())
                        }
                        None => MenuItem::separator(),
                    });
            }
            self.menu_indices = indices;
            if open && self.has_overflow() {
                self.open_menu(true);
                if let Some(row) = self
                    .menu_indices
                    .iter()
                    .position(|index| *index == focused_index)
                {
                    self.menu.focus_item(row);
                }
            }
        }
        if !self.has_overflow() {
            self.menu.close();
        }
        if let Some(viewport) = self.viewport {
            self.menu.reanchor(self.overflow_rect, viewport);
        }
    }

    fn open_menu(&mut self, keyboard: bool) {
        if !self.has_overflow() {
            return;
        }
        self.item_menu = None;
        self.menu
            .open_anchored(self.overflow_rect, self.viewport.unwrap_or(self.rect));
        self.focus = Some(Target::Overflow);
        self.hovered = None;
        self.pressed = None;
        if keyboard {
            self.menu.focus_first_action();
        }
    }

    /// Open the menu of the command at `index`, anchored to it.
    fn open_item_menu(&mut self, index: usize, keyboard: bool) {
        let Some(items) = self.command(index).and_then(|c| c.menu.clone()) else {
            return;
        };
        let anchor = self.item_rects.get(index).copied().unwrap_or(self.rect);
        let mut menu = ContextMenu::with_items(items);
        menu.open_anchored(anchor, self.viewport.unwrap_or(self.rect));
        if keyboard {
            menu.focus_first_action();
        }
        self.menu.close();
        self.item_menu = Some((index, menu));
        self.focus = Some(Target::Item(index));
        self.hovered = None;
        self.pressed = None;
    }

    fn invoke(&mut self, index: usize) {
        let opens_menu = match self.items.get_mut(index) {
            Some(ToolbarItem::Button(command)) if command.is_enabled() => {
                if command.menu.is_some() {
                    true
                } else {
                    if let Some(action) = &mut command.action {
                        if let Err(error) = action.emit(()) {
                            tracing::warn!(%error, "toolbar command refused its arguments");
                        }
                    } else {
                        (command.on_click)();
                    }
                    self.activated = Some(index);
                    false
                }
            }
            _ => false,
        };
        if opens_menu {
            if self
                .item_menu
                .as_ref()
                .is_some_and(|(open, _)| *open == index)
            {
                self.item_menu = None;
            } else {
                self.open_item_menu(index, false);
            }
        }
    }
    fn drain_menu(&mut self) {
        if let Some(index) = self
            .menu
            .take_selection()
            .and_then(|row| self.menu_indices.get(row).copied().flatten())
        {
            self.invoke(index);
        }
        if self.menu.take_submenu_selection().is_some() {
            // A pick in a command's nested menu ran its own action.
            self.menu.close();
        }
        if !self.menu.is_open() {
            self.focus = Some(Target::Overflow);
            self.pressed = None;
            self.refresh_hover();
        }
    }

    /// Popup routing still updates `pointer`, so closing or relayout cannot
    /// arm a command using a hover target from before the popup opened.
    fn refresh_hover(&mut self) -> bool {
        let hovered = if self.menu.is_open() || self.item_menu.is_some() {
            None
        } else {
            self.pointer.and_then(|point| {
                self.targets().into_iter().find(|target| {
                    self.target_rect(*target)
                        .is_some_and(|rect| rect.contains(point))
                })
            })
        };
        let changed = hovered != self.hovered;
        self.hovered = hovered;
        changed
    }

    fn targets(&self) -> Vec<Target> {
        let mut targets: Vec<_> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                (matches!(item, ToolbarItem::Button(_)) && self.is_shown(index))
                    .then_some(Target::Item(index))
            })
            .collect();
        if self.has_overflow() {
            targets.push(Target::Overflow);
        }
        targets
    }
    fn target_rect(&self, target: Target) -> Option<Rect> {
        match target {
            Target::Item(index) => self.item_rects.get(index).copied(),
            Target::Overflow => Some(self.overflow_rect),
        }
    }
    fn target_enabled(&self, target: Target) -> bool {
        match target {
            Target::Item(index) => self.command(index).is_some_and(|c| c.is_enabled()),
            Target::Overflow => true,
        }
    }
    fn activate(&mut self, target: Target, keyboard: bool) {
        match target {
            Target::Item(index) if self.command(index).is_some_and(|c| c.menu.is_some()) => {
                if self
                    .item_menu
                    .as_ref()
                    .is_some_and(|(open, _)| *open == index)
                {
                    self.item_menu = None;
                } else if self.target_enabled(target) {
                    self.open_item_menu(index, keyboard);
                }
            }
            Target::Item(index) => self.invoke(index),
            Target::Overflow => {
                self.overflow_keyboard = keyboard;
                if let Err(error) = self.overflow_action.emit(()) {
                    tracing::warn!(%error, "toolbar overflow view action refused");
                }
            }
        }
    }
    fn handle_key(&mut self, key: &Key, handled: &mut bool) {
        match key {
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                if let Some(target) = self.focus {
                    self.activate(target, true);
                }
                *handled = true;
            }
            Key::Named(NamedKey::ArrowRight) if self.orientation == Orientation::Horizontal => {
                self.move_focus(true);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowLeft) if self.orientation == Orientation::Horizontal => {
                self.move_focus(false);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowDown) if self.orientation == Orientation::Vertical => {
                self.move_focus(true);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) if self.orientation == Orientation::Vertical => {
                self.move_focus(false);
                *handled = true;
            }
            _ => {}
        }
    }

    fn move_focus(&mut self, forward: bool) {
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        let index = self
            .focus
            .and_then(|focus| targets.iter().position(|target| *target == focus));
        let next = match index {
            None => 0,
            Some(index) if forward => index
                .saturating_add(1)
                .checked_rem(targets.len())
                .unwrap_or_default(),
            Some(index) => index
                .checked_sub(1)
                .unwrap_or_else(|| targets.len().saturating_sub(1)),
        };
        self.focus = targets.get(next).copied();
    }
    fn semantic_target(&self, path: &[usize]) -> Option<Target> {
        let targets = self.targets();
        match path {
            [child] => targets.get(*child).copied(),
            [menu, child] if self.menu.is_open() && *menu == targets.len() => self
                .menu_indices
                .iter()
                .flatten()
                .nth(*child)
                .map(|index| Target::Item(*index)),
            _ => None,
        }
    }
    fn focus_target(&mut self, target: Target) {
        self.focused = true;
        match target {
            Target::Item(index) if !self.is_shown(index) => {
                self.open_menu(true);
                if let Some(row) = self
                    .menu_indices
                    .iter()
                    .position(|value| *value == Some(index))
                {
                    self.menu.focus_item(row);
                }
            }
            _ => self.focus = Some(target),
        }
    }

    /// The hosted controls' nodes under the toolbar's node, in item order.
    fn element_nodes(&self, taffy: &TaffyTree<()>, node: NodeId) -> Vec<(usize, NodeId)> {
        // A painted host lays the toolbar out without a node of its own;
        // only hosted controls have nodes to look up.
        if !self.has_elements() {
            return Vec::new();
        }
        let children = taffy.children(node).unwrap_or_default();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| matches!(item, ToolbarItem::Element(_)))
            .map(|(index, _)| index)
            .zip(children)
            .collect()
    }

    fn has_elements(&self) -> bool {
        self.items
            .iter()
            .any(|item| matches!(item, ToolbarItem::Element(_)))
    }
}

/// The separator's painted rule is a hairline, not a spacing step.
const SEPARATOR_HAIRLINE: f32 = 1.0;

/// Main-axis extent a separator occupies: the hairline alone. The sibling
/// gap on either side is its breathing room, so a group boundary reads as
/// `gap + rule + gap` and never doubles up with margins of its own. One
/// definition for the renderer-less estimate and the measured layout so
/// the two cannot disagree about where items fall.
fn separator_extent(_theme: &Theme) -> f32 {
    SEPARATOR_HAIRLINE
}

fn count_to_f32(n: usize) -> f32 {
    f32::from(u16::try_from(n).unwrap_or(u16::MAX))
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "bounded logical layout dimensions"
)]
impl crate::widget::controls::ActionControls for Toolbar {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        let _ = self.overflow_action.install_custody(domain, attachment);
        for item in &mut self.items {
            match item {
                ToolbarItem::Button(command) => {
                    if let Some(action) = &mut command.action {
                        let _ = action.install_custody(domain, attachment);
                    }
                }
                ToolbarItem::Element(element) => crate::widget::controls::install_control_custody(
                    element.as_mut(),
                    domain,
                    attachment,
                ),
                _ => {}
            }
        }
    }
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        if let Some(invocation) = self.overflow_action.take() {
            return Some(invocation);
        }
        self.items.iter_mut().find_map(|item| match item {
            ToolbarItem::Button(command) => {
                command.action.as_mut().and_then(super::ActionEmitter::take)
            }
            ToolbarItem::Element(element) => {
                crate::widget::controls::take_control_invocation(element.as_mut())
            }
            _ => None,
        })
    }
    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        let mut bindings = Vec::new();
        if self.has_overflow() {
            bindings.push(self.overflow_action.metadata("toolbar.overflow"));
        }
        for (index, item) in self.items.iter().enumerate() {
            match item {
                ToolbarItem::Button(command) => {
                    if (self.is_shown(index)
                        || (self.menu.is_open() && self.menu_indices.contains(&Some(index))))
                        && let Some(action) = &command.action
                    {
                        bindings.push(action.metadata(format!("toolbar.item.{index}")));
                    }
                }
                ToolbarItem::Element(element) => bindings.extend(
                    crate::widget::controls::control_action_bindings(element.as_ref()),
                ),
                _ => {}
            }
        }
        bindings
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        self.overflow_action.refresh_availability(allowed);
        let mut availability_changed = false;
        for item in &mut self.items {
            match item {
                ToolbarItem::Button(command) => {
                    if let Some(action) = &mut command.action {
                        let before = action.is_available();
                        action.refresh_availability(allowed);
                        if before != action.is_available() {
                            self.layout_dirty = true;
                            availability_changed = true;
                        }
                    }
                }
                ToolbarItem::Element(element) => {
                    crate::widget::controls::refresh_control_action_availability(
                        element.as_mut(),
                        allowed,
                    )
                }
                _ => {}
            }
        }
        if availability_changed {
            self.menu_indices.clear();
        }
    }
}

impl crate::resources::PublishesResources for Toolbar {
    fn resources(&self, _doc: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
        let mut resources: Vec<_> = self
            .items
            .iter()
            .filter_map(|item| match item {
                ToolbarItem::Button(command) => {
                    command.native.as_ref().map(|(_, native)| native.resource())
                }
                _ => None,
            })
            .collect();
        if self.has_overflow() {
            resources.push(self.overflow_native.resource());
        }
        resources
    }
}

impl Widget for Toolbar {
    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::resources::PublishesResources
    );

    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        let mut handlers: std::collections::HashSet<_> =
            std::iter::once(super::activation::NATIVE_INVOKE.to_owned()).collect();
        for child in self.children() {
            handlers.extend(child.resource_operation_handlers());
        }
        handlers
    }

    fn start_operation(
        &mut self,
        op: &super::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> Result<Option<super::ResourceOperationStart>, String> {
        if self.has_overflow()
            && let Some(started) = self.overflow_native.start(&self.overflow_id, op)?
        {
            return Ok(Some(started));
        }
        for item in &mut self.items {
            if let ToolbarItem::Button(command) = item
                && let Some((id, native)) = &mut command.native
                && let Some(started) = native.start(id, op)?
            {
                return Ok(Some(started));
            }
        }
        crate::widget::start_in_children(self, op, doc, path)
    }
    fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
        Widget::set_platform_conventions(&mut self.menu, conventions);
        for item in &mut self.items {
            if let ToolbarItem::Element(element) = item {
                element.set_platform_conventions(conventions);
            }
        }
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if self.overflow_requested.replace(false) {
            self.open_menu(self.overflow_keyboard);
            self.overflow_keyboard = true;
        }
        for item in &mut self.items {
            if let ToolbarItem::Element(element) = item {
                element.sync(state, path);
            }
        }
    }

    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.resolve_theme(theme);
        self.layout_dirty = true;
        self.detached.clear();
        // Bootstrap only for legacy callers that skip renderer settlement.
        self.extents = self
            .items
            .iter()
            .map(|item| match item {
                ToolbarItem::Button(_) | ToolbarItem::Element(_) => Size {
                    width: self.minimum,
                    height: self.minimum,
                },
                ToolbarItem::Separator => match self.orientation {
                    Orientation::Horizontal => Size {
                        width: self.separator,
                        height: self.minimum,
                    },
                    Orientation::Vertical => Size {
                        width: self.minimum,
                        height: self.separator,
                    },
                },
                ToolbarItem::Spacer => Size::ZERO,
            })
            .collect();
        self.chip_extent = Size {
            width: self.minimum,
            height: self.minimum,
        };
        let style = Style {
            flex_shrink: 1.0,
            min_size: Size {
                width: Dimension::length(0.0),
                height: Dimension::length(0.0),
            },
            ..Default::default()
        };
        let node = if self.has_elements() {
            // Hosted controls are laid out by the host, out of flow; the
            // toolbar sizes itself from its measured row in
            // `adjust_layout_style`.
            let children: Vec<NodeId> = self
                .items
                .iter_mut()
                .filter_map(|item| match item {
                    ToolbarItem::Element(element) => Some(element.build_layout(tree, theme, nodes)),
                    _ => None,
                })
                .collect();
            for &child in &children {
                if let Ok(mut child_style) = tree.style(child).cloned() {
                    child_style.position = taffy::Position::Absolute;
                    let _ = tree.set_style(child, child_style);
                }
            }
            tree.parent_node(style, &children)
        } else {
            tree.leaf_node(style)
        };
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn measure(
        &mut self,
        known: Size<Option<f32>>,
        _available: Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> Size<f32> {
        self.update_measurements(text);
        Size {
            width: known.width.unwrap_or(self.intrinsic.width),
            height: known.height.unwrap_or(self.intrinsic.height),
        }
    }
    fn adjust_layout_style(
        &mut self,
        tree: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        self.measured_scale = Some(self.theme.text_scale);
        let mut changed = self.update_measurements(text);
        if !self.has_elements() {
            if changed {
                let _ = tree.mark_dirty(node);
            }
            // The row gives way along its own axis (into the More menu), never
            // across it: in a column a toolbar keeps its full height.
            let Ok(mut style) = tree.style(node).cloned() else {
                return changed;
            };
            // A row a manual layout places (a header's actions) takes the
            // rect it is given, a hidden one none: its minimum is the
            // placement's, and holding a minimum of its own would fight it.
            if style.position == taffy::Position::Absolute {
                return changed;
            }
            let cross = match self.orientation {
                Orientation::Horizontal => &mut style.min_size.height,
                Orientation::Vertical => &mut style.min_size.width,
            };
            let want = Dimension::length(match self.orientation {
                Orientation::Horizontal => self.intrinsic.height.ceil(),
                Orientation::Vertical => self.intrinsic.width.ceil(),
            });
            if *cross == want {
                return changed;
            }
            *cross = want;
            return tree.set_style(node, style).is_ok() || changed;
        }
        // Hosted controls: their extents are what the host measured, and
        // the toolbar's own basis is the whole row.
        for (index, child) in self.element_nodes(tree, node) {
            let size = tree.layout(child).map(|l| l.size).unwrap_or(Size::ZERO);
            if let Some(slot) = self.extents.get_mut(index)
                && *slot != size
            {
                *slot = size;
                changed = true;
            }
        }
        self.recompute_intrinsic();
        let Ok(mut style) = tree.style(node).cloned() else {
            return changed;
        };
        let (main, cross) = match self.orientation {
            Orientation::Horizontal => (self.intrinsic.width, self.intrinsic.height),
            Orientation::Vertical => (self.intrinsic.height, self.intrinsic.width),
        };
        let basis = Dimension::length(main.ceil());
        let cross = Dimension::length(cross.ceil());
        let (want_basis, want_min) = (basis, cross);
        let current_min = match self.orientation {
            Orientation::Horizontal => style.min_size.height,
            Orientation::Vertical => style.min_size.width,
        };
        // A manually placed row keeps the placement's minimum (see above).
        let placed = style.position == taffy::Position::Absolute;
        if style.flex_basis == want_basis && (placed || current_min == want_min) {
            return changed;
        }
        style.flex_basis = want_basis;
        if !placed {
            match self.orientation {
                Orientation::Horizontal => style.min_size.height = want_min,
                Orientation::Vertical => style.min_size.width = want_min,
            }
        }
        tree.set_style(node, style).is_ok() || changed
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(tree, node, origin);
        self.place();
        let elements = self.element_nodes(tree, node);
        for (index, child) in elements {
            let rect = self.item_rects.get(index).copied().unwrap_or(Rect::ZERO);
            let l = tree.layout(child).copied().unwrap_or_default();
            if let Some(ToolbarItem::Element(element)) = self.items.get_mut(index) {
                element.apply_layout(
                    tree,
                    child,
                    Point::new(
                        rect.x0 - f64::from(l.location.x),
                        rect.y0 - f64::from(l.location.y),
                    ),
                );
            }
        }
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.viewport = presence.viewport.get().or(self.viewport);
        if let Some(viewport) = self.viewport {
            self.menu.reanchor(self.overflow_rect, viewport);
        }
        self.menu.layout_within(text, theme);
        // Explicit tiny allocations remain authoritative, including for glyph paint.
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.rect,
        );
        for index in 0..self.items.len() {
            let Some(rect) = self
                .item_rects
                .get(index)
                .copied()
                .filter(|rect| !rect.is_zero_area())
            else {
                continue;
            };
            match self.items.get(index) {
                Some(ToolbarItem::Button(command)) => {
                    let (label, icon, swatch, active, enabled, dot, name) = (
                        command.label.clone(),
                        command.icon,
                        command.swatch,
                        command.active.unwrap_or(false),
                        command.is_enabled(),
                        command.dot,
                        command.accessible_name.clone(),
                    );
                    let open = self.item_menu.as_ref().is_some_and(|(i, _)| *i == index);
                    // A colour well is a bare face (its lift on hover and
                    // press) with the colour standing in for the label.
                    let (shown, name) = match swatch {
                        Some(_) => ("", name.as_deref().or(Some(label.as_str()))),
                        None => (label.as_str(), name.as_deref()),
                    };
                    self.draw_button(
                        &mut PaintCtx::new(scene, transform, text, theme),
                        Target::Item(index),
                        shown,
                        name,
                        icon,
                        active || open,
                        enabled,
                    );
                    if let Some(color) = swatch {
                        let side = f64::from(theme.tokens.metrics.icon_md);
                        let well = Rect::from_center_size(rect.center(), (side, side));
                        crate::widget::ColorSwatchFace::new(well, color, &label)
                            .target_rect(rect)
                            .paint(scene, transform, text, theme);
                    }
                    if dot {
                        paint_dot(scene, transform, rect, theme);
                    }
                }
                Some(ToolbarItem::Separator) if !self.separator_shown(index) => {}
                Some(ToolbarItem::Separator) => {
                    let margin = f64::from(theme.tokens.spacing.s2);
                    let line = match self.orientation {
                        Orientation::Horizontal => Rect::new(
                            rect.center().x - 0.5,
                            rect.y0 + margin,
                            rect.center().x + 0.5,
                            (rect.y1 - margin).max(rect.y0 + margin),
                        ),
                        Orientation::Vertical => Rect::new(
                            rect.x0 + margin,
                            rect.center().y - 0.5,
                            (rect.x1 - margin).max(rect.x0 + margin),
                            rect.center().y + 0.5,
                        ),
                    };
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        crate::chrome::chrome_hairline(theme),
                        None,
                        &line,
                    );
                }
                Some(ToolbarItem::Element(_)) => {
                    if let Some(ToolbarItem::Element(element)) = self.items.get_mut(index) {
                        element.draw(scene, transform, text, theme, presence);
                    }
                }
                Some(ToolbarItem::Spacer) | None => {}
            }
        }
        if self.has_overflow() {
            self.draw_button(
                &mut PaintCtx::new(scene, transform, text, theme),
                Target::Overflow,
                "More commands",
                None,
                Some(crate::icons::Icon::MoreHorizontal),
                false,
                true,
            );
        }
        scene.pop_layer();
    }
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for item in &mut self.items {
            if let ToolbarItem::Element(element) = item {
                element.draw_overlay(scene, transform, text, theme, presence);
            }
        }
        if let Some((index, menu)) = self.item_menu.as_mut() {
            if let (Some(viewport), Some(anchor)) = (self.viewport, self.item_rects.get(*index)) {
                menu.reanchor(*anchor, viewport);
            }
            menu.draw_overlay(scene, transform, text, theme, presence);
        }
        self.menu
            .draw_overlay(scene, transform, text, theme, presence);
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if (self.focused || self.traps_focus())
            && matches!(event, WindowEvent::KeyboardInput { event, .. }
                if event.repeat && event.state == ElementState::Pressed
                    && matches!(event.logical_key, Key::Named(NamedKey::Enter | NamedKey::Space)))
        {
            *ctx.handled = true;
            return;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Some(Point::new(
                position.x / ctx.scale_factor,
                position.y / ctx.scale_factor,
            ));
        }
        if self.menu.is_open() {
            self.menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id));
            if matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                }
            ) {
                self.swallow_release = true;
            }
            self.drain_menu();
            return;
        }
        if let Some((_, menu)) = self.item_menu.as_mut() {
            menu.handle_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id));
            if matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                }
            ) {
                self.swallow_release = true;
            }
            if !menu.is_open() {
                self.item_menu = None;
                self.refresh_hover();
            }
            return;
        }
        // Hosted controls first: each answers for its own rect and focus. A
        // painted host lays the toolbar out itself (`lay_out_in`), so its
        // controls live in their own trees and the host's node is not ours.
        let painted = !self.detached.is_empty();
        if painted {
            for (item, content) in self.items.iter_mut().zip(&self.detached) {
                let (ToolbarItem::Element(element), Some(content)) = (item, content) else {
                    continue;
                };
                element.handle_event(
                    event,
                    &mut crate::widget::EventCtx {
                        scale_factor: ctx.scale_factor,
                        modifiers: ctx.modifiers,
                        state: ctx.state.reborrow(),
                        path: ctx.path,
                        handled: &mut *ctx.handled,
                        focus_target: &mut *ctx.focus_target,
                        node_id: content.node,
                        taffy: &content.taffy,
                    },
                );
                if *ctx.handled {
                    return;
                }
            }
        }
        let elements = if painted || !self.has_elements() {
            Vec::new()
        } else {
            self.element_nodes(ctx.taffy, ctx.node_id)
        };
        for (index, child) in elements {
            if let Some(ToolbarItem::Element(element)) = self.items.get_mut(index) {
                element.handle_event(event, &mut ctx.reborrow(ctx.path, child));
                if *ctx.handled {
                    return;
                }
            }
        }
        match event {
            WindowEvent::CursorMoved { .. } => {
                *ctx.handled |= self.refresh_hover();
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.refresh_hover();
                self.pressed = self.hovered.filter(|t| self.target_enabled(*t));
                if let Some(target) = self.pressed {
                    self.focus_target(target);
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                } else if self.hovered.is_some() {
                    // A disabled command swallows its press.
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.refresh_hover();
                if self.swallow_release {
                    self.swallow_release = false;
                    self.pressed = None;
                    *ctx.handled = true;
                    return;
                }
                if let Some(target) = self
                    .pressed
                    .take()
                    .filter(|target| Some(*target) == self.hovered)
                {
                    self.activate(target, false);
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.focused && event.state == ElementState::Pressed =>
            {
                self.handle_key(&event.logical_key, ctx.handled);
            }
            _ => {}
        }
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.items
            .iter()
            .filter_map(|item| match item {
                ToolbarItem::Element(element) => Some(element.as_ref()),
                _ => None,
            })
            .collect()
    }
    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for item in &self.items {
            if let ToolbarItem::Element(element) = item {
                visitor(element.as_ref());
            }
        }
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.items
            .iter_mut()
            .filter_map(|item| match item {
                ToolbarItem::Element(element) => Some(element.as_mut() as &mut (dyn Widget + '_)),
                _ => None,
            })
            .collect()
    }
    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty
            || self.items.iter().any(|item| match item {
                ToolbarItem::Element(element) => element.is_layout_dirty(),
                _ => false,
            })
    }
    fn layout_needs_measure(&self) -> bool {
        self.is_layout_dirty()
    }
    fn needs_redraw(&self) -> bool {
        true
    }
    fn is_focusable(&self) -> bool {
        self.items
            .iter()
            .any(|item| matches!(item, ToolbarItem::Button(_)))
    }
    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if focused && self.focus.is_none() {
            self.focus = self.targets().first().copied();
        }
        if !focused {
            self.menu.close();
            self.item_menu = None;
            self.pressed = None;
            self.refresh_hover();
        }
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn traps_focus(&self) -> bool {
        self.menu.is_open() || self.item_menu.is_some()
    }
    fn wants_pointer_capture(&self) -> bool {
        self.menu.is_open()
            || self.item_menu.is_some()
            || self.pressed.is_some()
            || self.swallow_release
            || self.items.iter().any(|item| match item {
                ToolbarItem::Element(element) => element.wants_pointer_capture(),
                _ => false,
            })
    }
    fn modal_capture(&self) -> Option<Rect> {
        self.menu
            .modal_capture()
            .or_else(|| self.item_menu.as_ref().and_then(|(_, m)| m.modal_capture()))
    }
    fn name(&self) -> &'static str {
        "Toolbar"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn accessibility(&self) -> Option<AccessNode> {
        let mut children: Vec<_> = self
            .targets()
            .iter()
            .map(|target| {
                let (name, route, expanded, checked, enabled) = match target {
                    Target::Item(index) => {
                        let command = self.command(*index);
                        let open = self.item_menu.as_ref().is_some_and(|(i, _)| i == index);
                        (
                            command.map(|c| c.name().to_owned()).unwrap_or_default(),
                            format!("toolbar.item.{index}"),
                            command.and_then(|c| c.menu.as_ref()).map(|_| open),
                            command.and_then(|c| c.active),
                            command.is_none_or(|c| c.is_enabled()),
                        )
                    }
                    Target::Overflow => (
                        "More commands".to_owned(),
                        "toolbar.overflow".to_owned(),
                        Some(self.menu.is_open()),
                        None,
                        true,
                    ),
                };
                let role = if checked.is_some() {
                    Role::ToggleButton
                } else {
                    Role::Button
                };
                AccessNode::new(role)
                    .with_name(name)
                    .with_action_target(route)
                    .with_bounds(self.target_rect(*target).unwrap_or(Rect::ZERO))
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.focused
                            && !self.menu.is_open()
                            && self.focus == Some(*target),
                        expanded,
                        checked,
                        disabled: !enabled,
                        ..Default::default()
                    })
            })
            .collect();
        if self.menu.is_open() {
            let rows = self
                .menu_indices
                .iter()
                .enumerate()
                .filter_map(|(row, index)| {
                    let index = (*index)?;
                    let command = self.command(index)?;
                    Some(
                        AccessNode::new(Role::MenuItem)
                            .with_name(command.name().to_owned())
                            .with_action_target(format!("toolbar.item.{index}"))
                            // Keep scrolled-out actions focusable so semantic focus
                            // can reveal them, but export no fabricated painted area.
                            .with_bounds(self.menu.item_rect(row).unwrap_or(Rect::ZERO))
                            .with_state(AccessState {
                                focusable: true,
                                focused: self.focused && self.menu.hovered() == Some(row),
                                checked: command.active,
                                disabled: !command.is_enabled(),
                                ..Default::default()
                            }),
                    )
                })
                .collect();
            children.push(
                AccessNode::new(Role::Menu)
                    .with_bounds(self.menu.get_rect())
                    .with_children(rows),
            );
        }
        if let Some((_, menu)) = &self.item_menu
            && let Some(node) = Widget::accessibility(menu)
        {
            children.push(node);
        }
        Some(
            AccessNode::new(Role::Toolbar)
                .with_state(AccessState {
                    focusable: self.is_focusable(),
                    focused: self.focused,
                    ..Default::default()
                })
                .with_children(children),
        )
    }
    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        match self.semantic_target(path)? {
            Target::Item(index) if !self.is_shown(index) => self
                .menu_indices
                .iter()
                .position(|value| *value == Some(index))
                .and_then(|row| self.menu.item_rect(row)),
            target => self.target_rect(target),
        }
    }
    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let Some(target) = self.semantic_target(path) else {
            return false;
        };
        self.focus_target(target);
        true
    }
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if let Some((_, menu)) = self.item_menu.as_mut()
            && menu.perform_semantic_action(target, action, state, path)
        {
            if !menu.is_open() {
                self.item_menu = None;
            }
            return true;
        }
        let target = if target == "toolbar.overflow" && self.has_overflow() {
            Target::Overflow
        } else if let Some(index) = target
            .strip_prefix("toolbar.item.")
            .and_then(|index| index.parse::<usize>().ok())
        {
            if self.command(index).is_none() || (!self.is_shown(index) && !self.menu.is_open()) {
                return false;
            }
            Target::Item(index)
        } else {
            return false;
        };
        match action {
            Action::Focus => self.focus_target(target),
            Action::Activate => {
                if !self.target_enabled(target) {
                    return false;
                }
                if let Target::Item(index) = target
                    && !self.is_shown(index)
                {
                    if let Some(row) = self
                        .menu_indices
                        .iter()
                        .position(|value| *value == Some(index))
                    {
                        self.menu.click_item(row);
                        self.drain_menu();
                    }
                } else {
                    self.activate(target, true);
                }
            }
            _ => return false,
        }
        true
    }
}

/// The dot on a command: your colour, at the corner of its rect, the
/// mark an unread count has without the count.
fn paint_dot(scene: &mut Scene, transform: Affine, rect: Rect, theme: &Theme) {
    let d = f64::from(theme.tokens.spacing.s2);
    let inset = f64::from(theme.tokens.spacing.s1);
    let centre = Point::new(rect.x1 - inset - d / 2.0, rect.y0 + inset + d / 2.0);
    scene.fill(
        vello::peniko::Fill::NonZero,
        transform,
        theme.tokens.colors.intent.you,
        None,
        &vello::kurbo::Circle::new(centre, d / 2.0),
    );
}

impl Toolbar {
    /// The theme's metrics for a toolbar: `Label` text, `s3` insets and
    /// gaps, and the pointer target as every command's floor.
    fn resolve_theme(&mut self, theme: &Theme) {
        if (self.theme.text_scale - theme.text_scale).abs() > f32::EPSILON {
            self.layout_dirty = true;
        }
        self.theme = theme.clone();
        self.font = theme.text(TextRole::Label);
        self.padding = theme.tokens.spacing.s3;
        // A hit target is a metric, not a spacing step: a custom theme's
        // dense `spacing_px` rescales `s8` but must not shrink the pointer
        // target beneath the 32 px floor (HIG §7b).
        self.minimum = theme.tokens.metrics.min_pointer_target;
        self.separator = separator_extent(theme);
        self.gap = theme.tokens.spacing.s3;
    }

    /// One command, painted as a plain button: bare at rest, a lift of the
    /// ground under the pointer, and a full lift while a toggle is on.
    fn draw_button(
        &self,
        ctx: &mut PaintCtx<'_>,
        target: Target,
        label: &str,
        name: Option<&str>,
        icon: Option<crate::icons::Icon>,
        active: bool,
        enabled: bool,
    ) {
        let Some(rect) = self.target_rect(target).filter(|rect| !rect.is_zero_area()) else {
            return;
        };
        let theme = ctx.theme;
        let face = match icon {
            Some(icon) => crate::widget::ButtonFace::icon_only(rect, icon, label),
            None => crate::widget::ButtonFace::new(rect, label),
        };
        let face = match name {
            Some(name) => face.accessible_name(name),
            None => face,
        }
        .role(crate::widget::ButtonRole::Plain)
        .toggled(active)
        .disabled(!enabled)
        .hovered(enabled && self.hovered == Some(target))
        .pressed(enabled && self.pressed == Some(target))
        .focused(self.focused && self.focus == Some(target) && !self.menu.is_open());
        crate::widget::Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
    }
}

/// The band behind a window's toolbar row: no fill of its own (the toolbar
/// stands on its region's ground) and one derived hairline along its bottom
/// edge, at `metrics.chrome_rule_px`.
pub fn paint_band(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
    let rule = f64::from(theme.tokens.metrics.chrome_rule_px);
    if rule > 0.0 {
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            crate::chrome::chrome_hairline(theme),
            None,
            &Rect::new(rect.x0, rect.y1 - rule, rect.x1, rect.y1),
        );
    }
}

/// The inset of a toolbar band's controls from its edges: siblings' step
/// across, the tightest step above and below so the band stays one control
/// tall (HIG §7b).
pub fn band_padding(theme: &Theme) -> taffy::Rect<taffy::LengthPercentage> {
    let spacing = &theme.tokens.spacing;
    let across = taffy::LengthPercentage::length(spacing.s3);
    let along = taffy::LengthPercentage::length(spacing.s1);
    taffy::Rect {
        left: across,
        right: across,
        top: along,
        bottom: along,
    }
}
// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::PresenceCtx;
    use vello_encoding::DrawTag;

    #[test]
    fn two_toolbar_overflow_actions_open_only_their_own_menu() {
        use crate::headless::Framed;
        let first = Toolbar::new(Orientation::Horizontal)
            .push_button("First toolbar long command", || {})
            .with_view("test.first-command");
        let second = Toolbar::new(Orientation::Horizontal)
            .push_button("Second toolbar long command", || {})
            .with_view("test.second-command");
        let root = crate::widget::Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .push(Box::new(first))
            .push(Box::new(second));
        let mut frame = Framed::new(crate::State::new(), root, (100, 300), Theme::dark());
        let resources = Framed::harness(&frame).resources();
        let overflow_ids: Vec<_> = resources
            .iter()
            .filter(|resource| resource.kind == "tilo.control" && resource.label == "More")
            .map(|resource| resource.id.as_str())
            .collect();
        assert_eq!(overflow_ids.len(), 2);
        assert_ne!(overflow_ids[0], overflow_ids[1]);
        let mut buttons: Vec<_> = Framed::harness(&frame)
            .controls()
            .into_iter()
            .filter(|control| control.name.as_deref() == Some("More commands"))
            .map(|control| control.bounds.expect("actual overflow button bounds"))
            .collect();
        buttons.sort_by(|left, right| left.y0.total_cmp(&right.y0));
        assert_eq!(buttons.len(), 2);
        assert!(
            buttons.iter().all(|rect| rect.area() > 0.0),
            "both actual exported overflow buttons have physical bounds"
        );
        for (index, expected, absent) in [
            (
                1,
                "Second toolbar long command",
                "First toolbar long command",
            ),
            (
                0,
                "First toolbar long command",
                "Second toolbar long command",
            ),
        ] {
            let point = buttons[index].center();
            Framed::harness_mut(&mut frame).click_at(point.x, point.y);
            Framed::harness_mut(&mut frame).poll_frame().unwrap();
            let controls = Framed::harness(&frame).controls();
            assert!(
                controls
                    .iter()
                    .any(|control| control.name.as_deref() == Some(expected))
            );
            assert!(
                !controls
                    .iter()
                    .any(|control| control.name.as_deref() == Some(absent))
            );
            Framed::harness_mut(&mut frame).key("Escape");
            Framed::harness_mut(&mut frame).poll_frame().unwrap();
        }
    }

    #[test]
    fn nested_native_controls_keep_their_operation_route() {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let count = calls.clone();
        let toolbar = Toolbar::new(Orientation::Horizontal).push_element(Box::new(
            crate::widget::Button::new("toolbar.filter", "Filter").on_view(move || {
                count.set(count.get() + 1);
            }),
        ));
        let mut frame = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            toolbar,
            (400, 80),
            Theme::dark(),
        );
        let audit = frame.native_app_audit();
        assert!(audit.is_certified(), "{audit:?}");
        frame
            .perform_named("toolbar.filter", &Action::Activate)
            .unwrap();
        assert_eq!(
            calls.get(),
            1,
            "the toolbar forwards its child's admitted operation"
        );
    }

    #[test]
    fn held_activation_keys_do_not_repeat_admitted_toolbar_commands() {
        use crate::headless::Framed;
        use std::cell::Cell;
        use std::rc::Rc;
        let press = || WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event: crate::synthetic_input::key_event(
                Key::Named(NamedKey::Enter),
                None,
                ElementState::Pressed,
            ),
            is_synthetic: true,
        };
        for width in [800, 100] {
            let calls = Rc::new(Cell::new(0));
            let count = Rc::clone(&calls);
            let toolbar = Toolbar::new(Orientation::Horizontal)
                .push_button("Open workspace settings", move || {
                    count.set(count.get() + 1)
                })
                .with_view("test.workspace-settings");
            let mut frame = Framed::new(crate::State::new(), toolbar, (width, 480), Theme::dark());
            assert!(Framed::harness(&frame).native_app_audit().is_certified());
            if width == 100 {
                Framed::harness_mut(&mut frame)
                    .perform_named("toolbar.overflow", &Action::Activate)
                    .unwrap();
                Framed::harness_mut(&mut frame).poll_frame().unwrap();
            }
            Framed::harness_mut(&mut frame)
                .perform_named("toolbar.item.0", &Action::Focus)
                .unwrap();
            let mut held = press();
            if let WindowEvent::KeyboardInput { event, .. } = &mut held {
                event.repeat = true;
            }
            Framed::harness_mut(&mut frame).send_host_event(&held);
            Framed::harness_mut(&mut frame).poll_frame().unwrap();
            assert_eq!(
                calls.get(),
                0,
                "{width}: a held key cannot start a new command"
            );
            Framed::harness_mut(&mut frame).send_host_event(&press());
            Framed::harness_mut(&mut frame).poll_frame().unwrap();
            assert_eq!(
                calls.get(),
                1,
                "{width}: one deliberate key press invokes once"
            );
            for _ in 0..3 {
                Framed::harness_mut(&mut frame).send_host_event(&held);
                Framed::harness_mut(&mut frame).poll_frame().unwrap();
            }
            assert_eq!(calls.get(), 1, "{width}: repetition must not invoke again");
        }
    }

    #[test]
    fn native_commands_are_admitted_and_operate_in_the_row_and_overflow() {
        use crate::headless::Framed;
        use std::cell::Cell;
        use std::rc::Rc;

        for width in [800, 100] {
            let calls = Rc::new(Cell::new(0));
            let callback = Rc::clone(&calls);
            let toolbar = Toolbar::new(Orientation::Horizontal)
                .push_toggle("Include children in process totals", false, move || {
                    callback.set(callback.get() + 1);
                })
                .with_view("test.include-children");
            let mut frame = Framed::new(crate::State::new(), toolbar, (width, 200), Theme::dark());
            let audit = Framed::harness(&frame).native_app_audit();
            assert!(audit.is_certified(), "{width}: {audit:?}");
            if width == 100 {
                Framed::harness_mut(&mut frame)
                    .perform_named("toolbar.overflow", &Action::Activate)
                    .unwrap();
                Framed::harness_mut(&mut frame).poll_frame().unwrap();
            }
            let command = Framed::harness(&frame)
                .controls()
                .into_iter()
                .find(|control| {
                    control.name.as_deref() == Some("Include children in process totals")
                })
                .and_then(|control| control.bounds)
                .expect("the actual row or overflow command is laid out");
            Framed::harness_mut(&mut frame).click_at(command.center().x, command.center().y);
            Framed::harness_mut(&mut frame).poll_frame().unwrap();
            assert_eq!(calls.get(), 1, "{width}: physical activation invokes once");
            if width == 100 {
                Framed::harness_mut(&mut frame)
                    .perform_named("toolbar.overflow", &Action::Activate)
                    .unwrap();
                Framed::harness_mut(&mut frame).poll_frame().unwrap();
            }
            Framed::harness_mut(&mut frame)
                .perform_named("toolbar.item.0", &Action::Activate)
                .unwrap();
            Framed::harness_mut(&mut frame).poll_frame().unwrap();
            assert_eq!(
                calls.get(),
                2,
                "{width}: assistive activation uses the same handler"
            );
            let audit = Framed::harness(&frame).native_app_audit();
            assert!(audit.is_certified(), "{width}: {audit:?}");
        }
    }

    #[test]
    fn bound_commands_queue_the_same_action_in_the_row_and_overflow_and_obey_availability() {
        use crate::widget::controls::{
            control_action_bindings, refresh_control_action_availability, take_control_invocation,
        };
        let resource = tilo_actions::Resource::new("doc:toolbar", "doc", "Document").with_action(
            tilo_actions::ActionSpec::operation(
                "save",
                "Save",
                tilo_actions::OperationKind::Effect,
                "test.save",
                &(),
            )
            .unwrap(),
        );
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "save").unwrap();
        for width in [800.0, 45.0] {
            let calls = std::rc::Rc::new(std::cell::Cell::new(0));
            let callback = calls.clone();
            let mut toolbar = Toolbar::new(Orientation::Horizontal)
                .push_button("Save document", move || callback.set(callback.get() + 1))
                .with_action(binding.clone())
                .push_button("Another command", || {});
            let mut text = TextRenderer::new();
            let theme = Theme::dark();
            let rect = Rect::new(0.0, 0.0, width, 40.0);
            toolbar.lay_out_in(rect, &mut text, &theme);
            if width < 100.0 {
                assert!(toolbar.has_overflow());
                assert!(!toolbar.is_shown(0));
                toolbar.open_overflow();
            }
            let mut doc = automerge::AutoCommit::new();
            assert!(toolbar.perform_semantic_action(
                "toolbar.item.0",
                &Action::Activate,
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            let invocation =
                take_control_invocation(&mut toolbar).expect("the canonical action is queued");
            assert_eq!(
                invocation.address,
                tilo_actions::ActionAddress::new("doc:toolbar", "save")
            );
            assert_eq!(
                calls.get(),
                0,
                "bound commands never run the imperative callback"
            );
            assert!(take_control_invocation(&mut toolbar).is_none());
            assert_eq!(
                control_action_bindings(&toolbar).len(),
                1,
                "after selecting, only the visible Save or overflow opener is mounted"
            );
            refresh_control_action_availability(&mut toolbar, &std::collections::HashSet::new());
            toolbar.lay_out_in(rect, &mut text, &theme);
            if toolbar.has_overflow() {
                toolbar.open_overflow();
            }
            assert!(!toolbar.perform_semantic_action(
                "toolbar.item.0",
                &Action::Activate,
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            assert!(
                take_control_invocation(&mut toolbar).is_none(),
                "withdrawn actions cannot be queued from overflow"
            );
            assert_eq!(calls.get(), 0);
        }
    }

    /// A separator never stands first, last or beside another separator of
    /// what is shown: a row that starts after a group boundary, or ends at
    /// one, shows no stray rule.
    #[test]
    fn separators_only_stand_between_shown_commands() {
        let tb = Toolbar::new(Orientation::Horizontal)
            .push_separator()
            .push_button("Cut", || {})
            .push_separator()
            .push_separator()
            .push_button("Paste", || {})
            .push_spacer()
            .push_separator();
        let mut tb = tb;
        tb.visible_count = tb.items.len();
        let shown: Vec<bool> = (0..tb.items.len())
            .filter(|index| matches!(tb.items.get(*index), Some(ToolbarItem::Separator)))
            .map(|index| tb.separator_shown(index))
            .collect();
        assert_eq!(shown, [false, true, false, false]);
    }

    /// Icon commands are pointer-target squares, toggles report their state
    /// and a painted host lays a toolbar out without taffy.
    #[test]
    fn icon_commands_and_toggles_lay_out_in_a_painted_host() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut tb = Toolbar::new(Orientation::Horizontal)
            .push_icon(crate::icons::Icon::Search, "Find", || {})
            .push_toggle("Bold", true, || {})
            .push_button("Insert table", || {});
        tb.lay_out_in(Rect::new(0.0, 0.0, 600.0, 40.0), &mut text, &theme);
        let icon = tb.item_rects[0];
        assert_eq!(
            icon.width(),
            f64::from(theme.tokens.metrics.min_pointer_target)
        );
        assert!(!tb.has_overflow());
        let node = Widget::accessibility(&tb).expect("toolbar semantics");
        assert_eq!(node.children[0].name.as_deref(), Some("Find"));
        assert_eq!(node.children[1].role, Role::ToggleButton);
        assert_eq!(node.children[1].state.checked, Some(true));
        tb.set_active(1, false);
        let node = Widget::accessibility(&tb).expect("toolbar semantics");
        assert_eq!(node.children[1].state.checked, Some(false));

        // Squeezed, every command stays reachable through the overflow menu.
        tb.lay_out_in(Rect::new(0.0, 0.0, 90.0, 40.0), &mut text, &theme);
        assert!(tb.has_overflow());
        let reachable = tb.visible_count + tb.menu_indices.iter().flatten().count();
        assert_eq!(reachable, 3);
    }

    #[test]
    fn a_colour_well_is_a_square_command_named_for_its_colour() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let yellow = vello::peniko::Color::from_rgb8(250, 204, 21);
        let green = vello::peniko::Color::from_rgb8(74, 222, 128);
        let mut tb = Toolbar::new(Orientation::Horizontal)
            .push_swatch(yellow, "Marker colour: Yellow", || {})
            .push_button("Insert table", || {});
        tb.lay_out_in(Rect::new(0.0, 0.0, 600.0, 40.0), &mut text, &theme);
        assert_eq!(
            tb.item_rects[0].width(),
            f64::from(theme.tokens.metrics.min_pointer_target)
        );
        tb.set_swatch(0, green, "Marker colour: Green");
        assert_eq!(tb.command(0).and_then(|c| c.swatch), Some(green));
        let node = Widget::accessibility(&tb).expect("toolbar semantics");
        assert_eq!(
            node.children[0].name.as_deref(),
            Some("Marker colour: Green")
        );

        // Squeezed, the well waits in the More menu under its name.
        tb.lay_out_in(Rect::new(0.0, 0.0, 60.0, 40.0), &mut text, &theme);
        assert!(tb.has_overflow());
        assert!(tb.menu_indices.contains(&Some(0)) || tb.is_shown(0));
    }

    /// Build a toolbar with `n` no-op buttons. Convenience for the
    /// overflow + paint tests that don't care which closure is invoked.
    fn n_button_toolbar(n: usize) -> Toolbar {
        let mut t = Toolbar::new(Orientation::Horizontal);
        for i in 0..n {
            t = t.push_button(&format!("B{i}"), || {});
        }
        t
    }

    /// Drive a Toolbar through the full `build_layout` + `apply_layout`
    /// pipeline at a fixed available width. Returns the toolbar with
    /// `item_rects` and `visible_count` populated.
    fn lay_out(mut tb: Toolbar, axis_extent: f64) -> Toolbar {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = tb.build_layout(&mut taffy, &theme, &mut node_map);
        // Force the outer rect to span `axis_extent`; taffy's auto sizing
        // would otherwise shrink to content.
        taffy
            .set_style(
                node,
                Style {
                    size: Size {
                        width: Dimension::length(f32_of_f64(axis_extent)),
                        height: Dimension::length(28.0),
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(f32_of_f64(axis_extent)),
                    height: taffy::AvailableSpace::Definite(28.0),
                },
            )
            .unwrap();
        tb.apply_layout(&taffy, node, Point::ZERO);
        tb
    }

    /// Render `tb` into a fresh scene and return the scene so tests can
    /// inspect `DrawTag::COLOR` counts.
    fn render(tb: &mut Toolbar) -> Scene {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        tb.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        scene
    }

    fn count_color_fills(scene: &Scene) -> usize {
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count()
    }

    #[test]
    fn toolbar_lays_out_named_commands_separators_and_overflow() {
        // Case `toolbar_exposes_focusable_named_commands`.
        {
            let tb = lay_out(n_button_toolbar(3), 100.0);
            assert!(tb.is_focusable());
            let node = tb.accessibility().unwrap();
            assert!(!node.children.is_empty());
            assert!(node.children.iter().all(|c| c.name.is_some()));
        }

        // Case `accessibility_reports_toolbar_role`.
        {
            use crate::a11y::Role;
            let tb = n_button_toolbar(3);
            let node = Widget::accessibility(&tb).expect("toolbar is a semantic strip");
            assert_eq!(node.role, Role::Toolbar);
            assert_eq!(node.name, None, "toolbar carries no name of its own");
        }

        // Case `toolbar_with_three_buttons_paints_three_button_rects`.
        // Three buttons + ample width must lay out three non-empty rects
        // and the draw pass must emit at least three `DrawTag::COLOR`
        // entries (one per button background fill).
        {
            let mut tb = lay_out(n_button_toolbar(3), 800.0);
            assert_eq!(
                tb.item_rects.len(),
                3,
                "three buttons must produce three item rects (got {})",
                tb.item_rects.len()
            );
            for (i, r) in tb.item_rects.iter().enumerate() {
                assert!(
                    !r.is_zero_area(),
                    "item_rects[{i}] must be non-empty (got {r:?})"
                );
            }
            assert_eq!(tb.visible_count(), 3);
            assert!(!tb.has_overflow());

            let scene = render(&mut tb);
            let fills = count_color_fills(&scene);
            assert!(
                fills >= 3,
                "expected at least 3 COLOR fills (one per button bg), got {fills}",
            );
        }

        // Case `toolbar_overflow_creates_overflow_button`.
        // Ten buttons in a width that only fits ~4 must collapse the rest
        // into the overflow chip. `has_overflow()` true, `visible_count()`
        // strictly less than 10.
        {
            // Ten buttons at ~30px each (~320px total) in a 150px budget must
            // overflow, fitting only a handful with room for the chip (24px).
            let tb = lay_out(n_button_toolbar(10), 150.0);
            assert!(
                tb.has_overflow(),
                "10 buttons in 150 px must overflow (visible_count={}, items={})",
                tb.visible_count(),
                tb.items.len()
            );
            assert!(
                tb.visible_count() < 10,
                "visible_count must drop below 10 when overflowing (got {})",
                tb.visible_count()
            );
            // And the visible items still produced real rects.
            for i in 0..tb.visible_count() {
                assert!(
                    !tb.item_rects[i].is_zero_area(),
                    "visible item_rects[{i}] must be non-empty"
                );
            }
            // Overflow chip rect exists.
            assert!(
                !tb.overflow_rect.is_zero_area(),
                "overflow chip rect must be populated when has_overflow() is true"
            );
        }

        // Case `minimum_hit_rect_reads_the_pointer_target_not_the_spacing_scale`.
        // `[Button, Separator, Button]` — the divider paints as a thin filled
        // rect (an extra COLOR draw tag). `separator_count` pins the structural
        // contract independently of the scene-encoding check.
        // The minimum hit rect is a metric, not a spacing step: halving the
        // spacing scale (what a dense custom-theme `spacing_px` does) must leave
        // every button at the 32 px pointer target. Before this read
        // `metrics.min_pointer_target`, the same fixture gave 16 px buttons.
        {
            let mut dense = Theme::dark();
            dense.tokens.spacing = crate::tokens::Spacing::BASE.scaled(0.5);
            assert!(
                dense.tokens.spacing.s8 < dense.tokens.metrics.min_pointer_target,
                "fixture must make s8 ({}) smaller than the target ({})",
                dense.tokens.spacing.s8,
                dense.tokens.metrics.min_pointer_target
            );
            let mut tb = Toolbar::new(Orientation::Horizontal)
                .push_button("A", || {})
                .push_button("Bb", || {});
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            tb.build_layout(&mut taffy, &dense, &mut node_map);
            let mut text = TextRenderer::new();
            tb.update_measurements(&mut text);
            let target = dense.tokens.metrics.min_pointer_target;
            assert_eq!(tb.minimum, target);
            for (index, extent) in tb.extents.iter().enumerate() {
                assert!(
                    extent.width >= target && extent.height >= target,
                    "button {index} extent {extent:?} is under the {target} px pointer target"
                );
            }
            // The renderer-less estimate follows the same floor.
            tb.recompute_visible(400.0, &dense);
            assert_eq!(tb.visible_count(), 2);
        }

        // Case `toolbar_separator_paints_a_divider_line`.
        {
            let mut tb = Toolbar::new(Orientation::Horizontal)
                .push_button("A", || {})
                .push_separator()
                .push_button("B", || {});
            tb = lay_out(tb, 400.0);

            assert_eq!(
                tb.separator_count(),
                1,
                "expected exactly one Separator item (got {})",
                tb.separator_count()
            );
            assert_eq!(tb.visible_count(), 3);

            let scene = render(&mut tb);
            let fills = count_color_fills(&scene);
            // Two button backgrounds + the separator line = at least 3
            // COLOR fills. Strictly greater than a no-separator baseline,
            // proving the separator actually contributed paint.
            assert!(
                fills >= 3,
                "expected at least 3 COLOR fills (2 buttons + 1 separator), got {fills}"
            );

            // Baseline comparison: a two-button toolbar (no separator) emits
            // strictly fewer COLOR fills than the same toolbar with a
            // separator wedged between them.
            let mut bare = lay_out(
                Toolbar::new(Orientation::Horizontal)
                    .push_button("A", || {})
                    .push_button("B", || {}),
                400.0,
            );
            let bare_scene = render(&mut bare);
            let bare_fills = count_color_fills(&bare_scene);
            assert!(
                fills > bare_fills,
                "separator must add at least one COLOR fill (with={fills}, without={bare_fills})"
            );
        }

        // Case `measured_labels_and_tiny_overflow_follow_lifecycle`.
        {
            let mut tb = Toolbar::new(Orientation::Horizontal)
                .push_button("WWW", || {})
                .push_button("iii", || {});
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let mut tree = TaffyTree::new();
            let node = tb.build_layout(&mut tree, &theme, &mut HashMap::new());
            tb.adjust_layout_style(&mut tree, node, &mut text);
            tree.set_style(
                node,
                Style {
                    size: Size {
                        width: Dimension::length(500.0),
                        height: Dimension::length(40.0),
                    },
                    ..Default::default()
                },
            )
            .unwrap();
            tree.compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::MaxContent,
                    height: taffy::AvailableSpace::MaxContent,
                },
            )
            .unwrap();
            tb.apply_layout(&tree, node, Point::ZERO);
            assert!(
                tb.item_rects[0].width() > tb.item_rects[1].width(),
                "renderer widths must replace character counting"
            );
            tree.set_style(
                node,
                Style {
                    size: Size {
                        width: Dimension::length(5.0),
                        height: Dimension::length(40.0),
                    },
                    ..Default::default()
                },
            )
            .unwrap();
            tree.compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::MaxContent,
                    height: taffy::AvailableSpace::MaxContent,
                },
            )
            .unwrap();
            tb.apply_layout(&tree, node, Point::ZERO);
            assert!(tb.overflow_rect.x0 >= tb.rect.x0);
            assert!(tb.item_rects.iter().all(|r| *r == Rect::ZERO));
        }
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use automerge::AutoCommit;
    use std::cell::RefCell;
    use std::rc::Rc;
    use winit::event::DeviceId;

    fn settle(tb: &mut Toolbar, width: f32, height: f32, scale: f32) {
        let theme = Theme::dark().with_text_scale(scale);
        let mut text = TextRenderer::new();
        text.set_text_scale(scale);
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let node = tb.build_layout(&mut tree, &theme, &mut nodes);
        let mut style = tree.style(node).unwrap().clone();
        style.size = Size {
            width: Dimension::length(width),
            height: Dimension::length(height),
        };
        tree.set_style(node, style).unwrap();
        let mut bindings = crate::bound_dispatch::BoundNodeMap::default();
        unsafe {
            bindings.rebuild(tb, &tree, node, &nodes);
            crate::widget::layout::compute_widget_layout(
                &mut tree,
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(width),
                    height: taffy::AvailableSpace::Definite(height),
                },
                &nodes,
                &bindings,
                &mut text,
            )
            .unwrap();
        }
        tb.apply_layout(&tree, node, Point::new(10.0, 10.0));
        let presence = PresenceCtx::default();
        presence
            .viewport
            .set(Some(Rect::new(0.0, 0.0, 360.0, 220.0)));
        let mut scene = Scene::new();
        tb.draw(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
        tb.draw_overlay(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
    }
    /// Complete emitted controls through their actual published operation,
    /// then sync, for the standalone geometry/event fixtures below.
    fn drain_native_controls(widget: &mut dyn Widget) {
        let mut doc = AutoCommit::new();
        while let Some(invocation) = crate::widget::controls::take_control_invocation(widget) {
            let mut resources = Vec::new();
            crate::widget::collect_widget_resources(widget, &doc, &mut resources);
            let resource = resources
                .into_iter()
                .find(|resource| resource.id.0 == invocation.address.resource)
                .expect("the emitted operation has a real published resource");
            let spec = resource
                .actions
                .iter()
                .find(|spec| spec.id == invocation.address.action)
                .expect("the emitted action is declared");
            let encoded = spec.build_json(&invocation.arguments).unwrap();
            let started = crate::widget::ResourceOperation::local(
                tilo_actions::InvocationId::new(),
                spec.kind,
                &encoded.type_id,
                &encoded.payload,
            )
            .start(
                widget,
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            )
            .unwrap();
            assert!(
                started.is_some(),
                "the actual operation handler admits the invocation"
            );
        }
        widget.sync(&doc, &automerge::ROOT);
    }

    fn dispatch(tb: &mut Toolbar, event: WindowEvent) -> bool {
        let handled = crate::testing::dispatch(tb, &event);
        drain_native_controls(tb);
        handled
    }
    fn cursor(tb: &mut Toolbar, p: Point) {
        dispatch(
            tb,
            WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed::<DeviceId>() },
                position: winit::dpi::PhysicalPosition::new(p.x, p.y),
            },
        );
    }
    fn mouse(tb: &mut Toolbar, state: ElementState) -> bool {
        dispatch(
            tb,
            WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed::<DeviceId>() },
                state,
                button: MouseButton::Left,
            },
        )
    }
    fn key_event(key: NamedKey) -> WindowEvent {
        let event = crate::synthetic_input::key_event(Key::Named(key), None, ElementState::Pressed);
        WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event,
            is_synthetic: true,
        }
    }
    fn key(tb: &mut Toolbar, key: NamedKey) -> bool {
        dispatch(tb, key_event(key))
    }
    fn counted(n: usize) -> (Toolbar, Rc<RefCell<Vec<usize>>>) {
        let counts = Rc::new(RefCell::new(vec![0; n]));
        let mut tb = Toolbar::new(Orientation::Horizontal);
        for index in 0..n {
            let counts = counts.clone();
            tb = tb.push_button(&format!("Command {index}"), move || {
                counts.borrow_mut()[index] += 1
            });
        }
        (tb, counts)
    }
    #[test]
    fn commands_are_reachable_once_by_every_route_with_matching_semantic_bounds() {
        // Case `pointer_survives_layout_between_press_and_release`.
        {
            let (mut tb, counts) = counted(2);
            settle(&mut tb, 350.0, 45.0, 1.0);
            let p = tb.item_rects[0].center();
            cursor(&mut tb, p);
            mouse(&mut tb, ElementState::Pressed);
            settle(&mut tb, 350.0, 45.0, 1.0);
            mouse(&mut tb, ElementState::Released);
            assert_eq!(*counts.borrow(), vec![1, 0]);
        }

        // Case `tab_focus_starts_at_first_command_and_arrows_skip_decoration`.
        {
            let mut tb = Toolbar::new(Orientation::Horizontal)
                .push_button("First", || {})
                .push_separator()
                .push_spacer()
                .push_button("Last", || {});
            settle(&mut tb, 350.0, 45.0, 1.0);
            tb.set_focus(true);
            assert_eq!(tb.focus, Some(Target::Item(0)));
            key(&mut tb, NamedKey::ArrowRight);
            assert_eq!(tb.focus, Some(Target::Item(3)));
        }

        // Case `every_callback_is_reachable_once_by_each_input_route`.
        {
            for route in 0..3 {
                let (mut tb, counts) = counted(8);
                settle(&mut tb, 180.0, 45.0, 1.0);
                tb.set_focus(true);
                for index in 0..8 {
                    if index < tb.visible_count {
                        match route {
                            0 => {
                                let p = tb.item_rects[index].center();
                                cursor(&mut tb, p);
                                mouse(&mut tb, ElementState::Pressed);
                                mouse(&mut tb, ElementState::Released);
                            }
                            1 => {
                                tb.focus = Some(Target::Item(index));
                                key(&mut tb, NamedKey::Enter);
                            }
                            _ => {
                                assert!(tb.perform_semantic_action(
                                    &format!("toolbar.item.{index}"),
                                    &Action::Activate,
                                    &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                                    &automerge::ROOT
                                ));
                            }
                        }
                    } else {
                        tb.focus = Some(Target::Overflow);
                        key(&mut tb, NamedKey::Space);
                        settle(&mut tb, 180.0, 45.0, 1.0);
                        let row = tb
                            .menu_indices
                            .iter()
                            .position(|entry| *entry == Some(index))
                            .unwrap();
                        while tb.menu.hovered() != Some(row) {
                            key(&mut tb, NamedKey::ArrowDown);
                        }
                        match route {
                            0 => {
                                let p = tb.menu.item_rect(row).unwrap().center();
                                cursor(&mut tb, p);
                                mouse(&mut tb, ElementState::Pressed);
                                mouse(&mut tb, ElementState::Released);
                            }
                            1 => {
                                key(&mut tb, NamedKey::Enter);
                            }
                            _ => {
                                assert!(tb.perform_semantic_action(
                                    &format!("toolbar.item.{index}"),
                                    &Action::Activate,
                                    &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                                    &automerge::ROOT
                                ));
                            }
                        }
                        assert!(!tb.menu.is_open());
                        assert_eq!(tb.focus, Some(Target::Overflow));
                    }
                }
                assert_eq!(*counts.borrow(), vec![1; 8], "route {route}");
            }
        }

        // Case `accessibility_menu_rows_have_routes_bounds_and_focus`.
        {
            let (mut tb, _) = counted(12);
            settle(&mut tb, 120.0, 45.0, 1.0);
            tb.set_focus(true);
            tb.open_menu(true);
            let menu_child = tb.targets().len();
            let node = tb.accessibility().unwrap();
            assert_eq!(node.children[menu_child].role, Role::Menu);
            assert!(
                node.children[menu_child]
                    .children
                    .iter()
                    .all(|child| child.action_target.is_some())
            );
            let last = node.children[menu_child].children.len() - 1;
            assert!(tb.focus_synthetic_accessibility(&[menu_child, last]));
            let rect = tb
                .synthetic_accessibility_rect(&[menu_child, last])
                .unwrap();
            assert!(rect.y0 >= tb.menu.get_rect().y0 && rect.y1 <= tb.menu.get_rect().y1);
            assert!(
                tb.accessibility().unwrap().children[menu_child].children[last]
                    .state
                    .focused
            );
        }

        // Case `review_emitted_accessibility_bounds_match_paint_and_scrolling`.
        {
            let (mut tb, _) = counted(12);
            settle(&mut tb, 180.0, 45.0, 1.0);
            let node = tb.accessibility().unwrap();
            for (child, target) in node.children.iter().zip(tb.targets()) {
                assert_eq!(
                    child.bounds.map(|bounds| bounds.to_rect()),
                    tb.target_rect(target)
                );
            }
            tb.open_menu(true);
            let menu_child = tb.targets().len();
            let first_height = tb.menu.item_rect(0).unwrap().height();
            tb.menu.focus_item(tb.menu_indices.len() - 1);
            let node = tb.accessibility().unwrap();
            let menu = &node.children[menu_child];
            assert_eq!(menu.bounds.unwrap().to_rect(), tb.menu.get_rect());
            let mut clipped = false;
            for (child, row) in menu.children.iter().zip(
                tb.menu_indices
                    .iter()
                    .enumerate()
                    .filter_map(|(row, index)| index.map(|_| row)),
            ) {
                let expected = tb.menu.item_rect(row).unwrap_or(Rect::ZERO);
                assert_eq!(child.bounds.unwrap().to_rect(), expected);
                clipped |= expected.height() > 0.0 && expected.height() < first_height;
            }
            assert!(clipped);
            assert_eq!(menu.children[0].bounds.unwrap().to_rect(), Rect::ZERO);
            assert!(
                !menu.children[0].state.hidden,
                "offscreen commands retain semantic focus access"
            );
            assert!(tb.focus_synthetic_accessibility(&[menu_child, 0]));
            assert!(
                !tb.accessibility().unwrap().children[menu_child].children[0]
                    .bounds
                    .unwrap()
                    .to_rect()
                    .is_zero_area()
            );
        }
    }
    #[test]
    fn toolbar_lifecycle_measures_scales_orients_and_reconciles_overflow() {
        // Case `measured_unicode_scale_orientation_and_resize`.
        {
            for orientation in [Orientation::Horizontal, Orientation::Vertical] {
                let mut tb = Toolbar::new(orientation)
                    .push_button("WWW", || {})
                    .push_button("iii", || {})
                    .push_button("共有 العربية e\u{301} 😀", || {})
                    .push_button("Eine sehr lange übersetzte Aktion", || {});
                settle(&mut tb, 140.0, 90.0, 1.0);
                assert!(tb.extents[0].width > tb.extents[1].width);
                let small = tb.extents.clone();
                tb.open_menu(true);
                settle(&mut tb, 140.0, 90.0, 2.0);
                assert!(tb.extents[2].width > small[2].width);
                assert!(tb.extents[2].height > small[2].height);
                assert!(
                    tb.item_rects
                        .iter()
                        .skip(tb.visible_count)
                        .all(|rect| *rect == Rect::ZERO)
                );
                assert!(tb.menu.is_open());
                assert!(tb.menu.get_rect().x0 >= 0.0 && tb.menu.get_rect().x1 <= 360.0);
                settle(&mut tb, 2000.0, 1000.0, 2.0);
                assert!(!tb.has_overflow());
                assert!(!tb.menu.is_open());
                assert!(tb.targets().contains(&tb.focus.unwrap()));
            }
        }

        // Case `decorations_never_create_an_overflow_menu`.
        {
            let mut tb = Toolbar::new(Orientation::Horizontal)
                .push_button("A", || {})
                .push_separator()
                .push_spacer();
            settle(&mut tb, 100.0, 45.0, 1.0);
            let exact = tb.extents[0].width;
            settle(&mut tb, exact, 45.0, 1.0);
            assert_eq!(tb.visible_count, 1);
            assert!(!tb.has_overflow());
            assert_eq!(tb.overflow_rect, Rect::ZERO);
            settle(&mut tb, exact - 1.0, 45.0, 1.0);
            assert!(tb.has_overflow());
            assert!(tb.menu_indices.iter().all(Option::is_some));
        }

        // Case `resize_reconciles_original_indices_and_meaningful_separators`.
        {
            let mut tb = Toolbar::new(Orientation::Horizontal)
                .push_separator()
                .push_button("One", || {})
                .push_separator()
                .push_separator()
                .push_spacer()
                .push_button("Two", || {})
                .push_separator()
                .push_button("Three", || {})
                .push_separator();
            settle(&mut tb, 5.0, 45.0, 1.0);
            assert_eq!(tb.menu_indices, vec![Some(1), None, Some(5), None, Some(7)]);
            assert!(tb.overflow_rect.x0 >= tb.rect.x0);
            tb.open_menu(true);
            tb.menu.focus_item(4);
            // The row's used width is the packing model `place` sums: the five
            // extents, the four sibling gaps between them, and the chip with
            // its own gap (HIG §7b: siblings sit `s3` apart, the chip included).
            let width = tb
                .extents
                .iter()
                .take(5)
                .map(|size| size.width)
                .sum::<f32>()
                + tb.gap * 5.0
                + tb.chip_extent.width;
            settle(&mut tb, width, 45.0, 1.0);
            assert!(tb.menu.is_open());
            assert_eq!(tb.menu_indices, vec![Some(5), None, Some(7)]);
            assert_eq!(tb.menu.hovered(), Some(2));
            assert!(tb.menu.get_rect().x0 >= 0.0 && tb.menu.get_rect().x1 <= 360.0);
        }

        // Case `vertical_navigation_and_tiny_chip_stay_inside_allocation`.
        {
            let mut tb = Toolbar::new(Orientation::Vertical)
                .push_button("One", || {})
                .push_separator()
                .push_button("Two", || {});
            settle(&mut tb, 120.0, 180.0, 1.0);
            tb.set_focus(true);
            key(&mut tb, NamedKey::ArrowDown);
            assert_eq!(tb.focus, Some(Target::Item(2)));
            key(&mut tb, NamedKey::ArrowUp);
            assert_eq!(tb.focus, Some(Target::Item(0)));
            settle(&mut tb, 120.0, 5.0, 2.0);
            assert!(tb.has_overflow());
            assert!(tb.overflow_rect.y0 >= tb.rect.y0 && tb.overflow_rect.y1 <= tb.rect.y1);
            assert!(tb.item_rects.iter().all(|rect| *rect == Rect::ZERO));
        }
    }
    #[test]
    fn overflow_popup_dismissal_restores_focus_without_click_through() {
        // Case `escape_and_outside_press_restore_focus_without_click_through`.
        {
            let (mut tb, counts) = counted(5);
            settle(&mut tb, 180.0, 45.0, 1.0);
            tb.set_focus(true);
            tb.focus = Some(Target::Overflow);
            key(&mut tb, NamedKey::Enter);
            assert!(tb.traps_focus() && tb.wants_pointer_capture());
            key(&mut tb, NamedKey::Escape);
            assert!(!tb.traps_focus());
            assert!(tb.is_focused());
            assert_eq!(tb.focus, Some(Target::Overflow));
            key(&mut tb, NamedKey::Enter);
            let p = tb.item_rects[0].center();
            cursor(&mut tb, p);
            mouse(&mut tb, ElementState::Pressed);
            mouse(&mut tb, ElementState::Released);
            assert_eq!(*counts.borrow(), vec![0; 5]);
        }

        // Case `review_popup_close_rehits_pointer_before_second_click`.
        {
            for pointer_open in [false, true] {
                for relayout in [false, true] {
                    let (mut tb, counts) = counted(8);
                    settle(&mut tb, 180.0, 45.0, 1.0);
                    let point = if pointer_open {
                        tb.overflow_rect.center()
                    } else {
                        tb.item_rects[0].center()
                    };
                    cursor(&mut tb, point);
                    if pointer_open {
                        mouse(&mut tb, ElementState::Pressed);
                        mouse(&mut tb, ElementState::Released);
                    } else {
                        tb.set_focus(true);
                        while tb.focus != Some(Target::Overflow) {
                            key(&mut tb, NamedKey::ArrowRight);
                        }
                        key(&mut tb, NamedKey::Space);
                    }
                    let hidden = tb.menu_indices[0].unwrap();
                    let point = tb.menu.item_rect(0).unwrap().center();
                    assert!(!tb.rect.contains(point));
                    cursor(&mut tb, point);
                    mouse(&mut tb, ElementState::Pressed);
                    mouse(&mut tb, ElementState::Released);
                    if relayout {
                        settle(&mut tb, 180.0, 45.0, 1.0);
                    }
                    mouse(&mut tb, ElementState::Pressed);
                    mouse(&mut tb, ElementState::Released);
                    let mut expected = vec![0; 8];
                    expected[hidden] = 1;
                    assert_eq!(
                        *counts.borrow(),
                        expected,
                        "pointer_open={pointer_open}, relayout={relayout}"
                    );
                    assert!(
                        !tb.menu.is_open(),
                        "stale overflow hover must not reopen the menu"
                    );
                }
            }
        }

        // Case `review_nonleft_popup_press_does_not_capture_next_sibling_click`.
        {
            for button in [MouseButton::Right, MouseButton::Middle] {
                let (toolbar, _) = counted(8);
                let (sibling, count) = counted(1);
                let mut host = crate::widget::Row::new()
                    .no_wrap()
                    .push(Box::new(toolbar))
                    .push(Box::new(sibling));
                let mut tree = TaffyTree::new();
                let mut nodes = HashMap::new();
                let theme = Theme::dark();
                let node = host.build_layout(&mut tree, &theme, &mut nodes);
                let children = tree.children(node).unwrap();
                for (child, width) in children.iter().zip([180.0, 150.0]) {
                    let mut style = tree.style(*child).unwrap().clone();
                    style.size = Size {
                        width: Dimension::length(width),
                        height: Dimension::length(45.0),
                    };
                    tree.set_style(*child, style).unwrap();
                }
                let mut text = TextRenderer::new();
                let mut bindings = crate::bound_dispatch::BoundNodeMap::default();
                unsafe {
                    bindings.rebuild(&host, &tree, node, &nodes);
                    crate::widget::compute_widget_layout(
                        &mut tree,
                        node,
                        Size {
                            width: taffy::AvailableSpace::Definite(360.0),
                            height: taffy::AvailableSpace::Definite(220.0),
                        },
                        &nodes,
                        &bindings,
                        &mut text,
                    )
                    .unwrap();
                }
                host.apply_layout(&tree, node, Point::ZERO);
                let presence = PresenceCtx::default();
                presence
                    .viewport
                    .set(Some(Rect::new(0.0, 0.0, 360.0, 220.0)));
                host.draw(
                    &mut Scene::new(),
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    &presence,
                );
                assert!(host.children_mut()[0].perform_semantic_action(
                    "toolbar.overflow",
                    &Action::Activate,
                    &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                    &automerge::ROOT
                ));
                drain_native_controls(&mut host);
                let send = |host: &mut crate::widget::Row, event: WindowEvent| {
                    crate::testing::Dispatch::new()
                        .layout(&tree, node)
                        .send(host, &event);
                };
                let mouse_event = |button, state| WindowEvent::MouseInput {
                    device_id: winit::event::DeviceId::dummy(),
                    button,
                    state,
                };
                send(&mut host, mouse_event(button, ElementState::Pressed));
                send(&mut host, mouse_event(button, ElementState::Released));
                send(&mut host, key_event(NamedKey::Escape));
                assert!(
                    !host.wants_pointer_capture(),
                    "{button:?} must not leave capture stuck"
                );
                let point = host.children()[1]
                    .synthetic_accessibility_rect(&[0])
                    .unwrap()
                    .center();
                send(
                    &mut host,
                    WindowEvent::CursorMoved {
                        device_id: winit::event::DeviceId::dummy(),
                        position: winit::dpi::PhysicalPosition::new(point.x, point.y),
                    },
                );
                send(
                    &mut host,
                    mouse_event(MouseButton::Left, ElementState::Pressed),
                );
                send(
                    &mut host,
                    mouse_event(MouseButton::Left, ElementState::Released),
                );
                assert_eq!(
                    *count.borrow(),
                    vec![1],
                    "next sibling click must arrive through Row"
                );
            }
        }
    }

    #[test]
    #[ignore = "manual offscreen evidence; set TILO_UI_SCREENSHOT_DIR"]
    fn rendered_toolbar_overflow_fixtures() {
        let dir = crate::env::var("TILO_UI_SCREENSHOT_DIR").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        for (name, width, height, scale, toolbar_width) in [
            ("toolbar-ordinary-100", 720u32, 340u32, 1.0, 530.0),
            ("toolbar-narrow-200", 320u32, 260u32, 2.0, 290.0),
        ] {
            let labels = [
                "New",
                "Open",
                "Save",
                "WWW",
                "iii",
                "共有",
                "العربية",
                "Übersetzte Aktion",
                "Export",
                "Last hidden command",
            ];
            let fired = Rc::new(RefCell::new(0));
            let mut tb = Toolbar::new(Orientation::Horizontal);
            for (index, label) in labels.iter().enumerate() {
                let fired = fired.clone();
                tb = tb.push_button(label, move || {
                    if index == 9 {
                        *fired.borrow_mut() += 1;
                    }
                });
            }
            settle(&mut tb, toolbar_width, 44.0 * scale, scale);
            let theme = Theme::dark().with_text_scale(scale);
            let mut text = TextRenderer::new();
            text.set_text_scale(scale);
            let presence = PresenceCtx::default();
            presence.viewport.set(Some(Rect::new(
                0.0,
                0.0,
                f64::from(width),
                f64::from(height),
            )));
            let mut scene = Scene::new();
            tb.draw(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
            tb.set_focus(true);
            tb.focus = Some(Target::Overflow);
            key(&mut tb, NamedKey::Enter);
            let last = tb.menu_indices.len() - 1;
            while tb.menu.hovered() != Some(last) {
                key(&mut tb, NamedKey::ArrowDown);
            }
            scene.reset();
            tb.draw(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
            tb.draw_overlay(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
            assert!(tb.menu.item_rect(last).is_some());
            let mut renderer = crate::offscreen::OffscreenRenderer::new(width, height);
            let path = format!("{dir}/{name}.png");
            renderer.render_png(&scene, theme.tokens.colors.surfaces.content, &path);
            key(&mut tb, NamedKey::Enter);
            assert_eq!(*fired.borrow(), 1);
            eprintln!(
                "capture {path}; viewport={width}x{height}; toolbar_width={toolbar_width}; Theme::dark().with_text_scale({scale}); last command fired once"
            );
        }
    }
}

#[cfg(test)]
mod command_state_tests {
    use super::*;
    use crate::icons::Icon;
    use crate::testing::dispatch;
    use std::cell::Cell;
    use std::rc::Rc;
    use winit::event::DeviceId;

    fn lay(tb: &mut Toolbar, width: f64) {
        let mut text = TextRenderer::new();
        tb.lay_out_in(Rect::new(0.0, 0.0, width, 40.0), &mut text, &Theme::dark());
    }

    fn click(tb: &mut Toolbar, p: Point) {
        let device = unsafe { std::mem::zeroed::<DeviceId>() };
        dispatch(
            tb,
            &WindowEvent::CursorMoved {
                device_id: device,
                position: winit::dpi::PhysicalPosition::new(p.x, p.y),
            },
        );
        for state in [ElementState::Pressed, ElementState::Released] {
            dispatch(
                tb,
                &WindowEvent::MouseInput {
                    device_id: device,
                    state,
                    button: MouseButton::Left,
                },
            );
        }
    }

    #[test]
    fn icon_toggles_disabled_commands_and_labels_follow_the_model() {
        let ran = Rc::new(Cell::new(0));
        let r = Rc::clone(&ran);
        let mut tb = Toolbar::new(Orientation::Horizontal)
            .push_icon_toggle(Icon::List, "Show outline", true, move || r.set(r.get() + 1))
            .push_button("Reset", || {})
            .push_toggle("Aa", false, || {})
            .with_accessible_name("Match case");
        lay(&mut tb, 600.0);
        let node = Widget::accessibility(&tb).unwrap();
        assert_eq!(node.children[0].role, Role::ToggleButton);
        assert_eq!(node.children[0].state.checked, Some(true));
        assert_eq!(node.children[2].name.as_deref(), Some("Match case"));
        // A disabled command is announced as disabled and does nothing.
        tb.set_enabled(1, false);
        let node = Widget::accessibility(&tb).unwrap();
        assert!(node.children[1].state.disabled);
        let reset = tb.item_rect(1).unwrap();
        click(&mut tb, reset.center());
        assert_eq!(tb.take_activated(), None);
        // The toggle runs and reports itself.
        let toggle = tb.item_rect(0).unwrap();
        click(&mut tb, toggle.center());
        assert_eq!(ran.get(), 1);
        assert_eq!(tb.take_activated(), Some(0));
        assert_eq!(tb.take_activated(), None, "once");
        // A label follows the model and the row is measured again.
        let before = tb.item_rect(1).unwrap().width();
        tb.set_label(1, "Reset all adjustments");
        lay(&mut tb, 600.0);
        assert!(tb.item_rect(1).unwrap().width() > before);
        tb.set_dot(0, true);
        let mut scene = Scene::new();
        tb.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::default(),
        );
    }

    #[test]
    fn priority_keeps_a_pair_together_and_the_more_menu_shows_state() {
        let mut tb = Toolbar::new(Orientation::Horizontal)
            .push_button("Insert table", || {})
            .with_priority(-1)
            .push_button("Insert image", || {})
            .with_priority(-1)
            .push_icon(Icon::Minus, "Zoom out", || {})
            .with_priority(1)
            .push_icon(Icon::Plus, "Zoom in", || {})
            .with_priority(1)
            .push_icon_toggle(Icon::Eye, "Preview", true, || {});
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let full = tb.intrinsic_size(&mut text, &theme).width;
        // Narrow enough that something must give.
        lay(&mut tb, full - 40.0);
        assert!(tb.has_overflow());
        assert!(
            tb.item_rect(2).is_some() && tb.item_rect(3).is_some(),
            "the zoom pair stays"
        );
        assert!(
            tb.item_rect(0).is_none() || tb.item_rect(1).is_none(),
            "the low priority gives way"
        );
        // Everything is still reachable once.
        let shown = (0..5).filter(|i| tb.item_rect(*i).is_some()).count();
        assert_eq!(shown + tb.menu_indices.iter().flatten().count(), 5);
        // The More menu opens on demand and a toggle there carries its check.
        lay(&mut tb, 120.0);
        tb.open_overflow();
        assert!(tb.is_overflow_open());
        let preview = tb
            .menu_indices
            .iter()
            .position(|i| *i == Some(4))
            .and_then(|row| tb.menu.items().get(row))
            .and_then(MenuItem::checked);
        assert_eq!(preview, Some(true));
    }

    #[test]
    fn joined_commands_give_way_together() {
        let mut tb = Toolbar::new(Orientation::Horizontal)
            .push_button("Compile", || {})
            .push_icon(Icon::Minus, "Zoom out", || {})
            .push_icon(Icon::Plus, "Zoom in", || {})
            .joined();
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let full = tb.intrinsic_size(&mut text, &theme).width;
        for width in (60..(full as i32 + 10)).step_by(4) {
            lay(&mut tb, f64::from(width));
            assert_eq!(
                tb.item_rect(1).is_some(),
                tb.item_rect(2).is_some(),
                "at {width} px the pair stands or folds together"
            );
        }
    }

    #[test]
    fn a_menu_command_opens_its_menu_under_itself() {
        let sorted = Rc::new(Cell::new(false));
        let mut tb = Toolbar::new(Orientation::Horizontal).push_menu(
            "Sort",
            vec![
                MenuItem::action("By name", || {}),
                MenuItem::action("By date", || {}),
            ],
        );
        lay(&mut tb, 400.0);
        // The window the toolbar stands in, with room under it.
        tb.viewport = Some(Rect::new(0.0, 0.0, 400.0, 400.0));
        let sort = tb.item_rect(0).unwrap();
        click(&mut tb, sort.center());
        let (index, menu) = tb.item_menu.as_ref().expect("the command's menu opens");
        assert_eq!(*index, 0);
        assert!(menu.get_rect().y0 >= sort.y1 - 0.5, "under the command");
        let node = Widget::accessibility(&tb).unwrap();
        assert_eq!(node.children[0].state.expanded, Some(true));
        let _ = sorted;
    }

    #[test]
    fn layout_from_cached_measurements_needs_no_renderer() {
        let mut tb = Toolbar::new(Orientation::Horizontal)
            .push_button("Bold", || {})
            .push_button("Italic", || {});
        let theme = Theme::dark();
        tb.lay_out_at(Rect::new(0.0, 0.0, 400.0, 40.0), &theme);
        let first = tb.item_rect(1).unwrap();
        tb.lay_out_at(Rect::new(20.0, 0.0, 420.0, 40.0), &theme);
        let moved = tb.item_rect(1).unwrap();
        assert!((moved.x0 - first.x0 - 20.0).abs() < 1e-9);
        assert!(!Widget::is_layout_dirty(&tb), "nothing changed since");
        tb.set_label(0, "Bold text");
        assert!(Widget::is_layout_dirty(&tb));
    }
}
