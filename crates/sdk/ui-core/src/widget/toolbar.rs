//! The toolbar: a measured strip of commands with a shared `ContextMenu`
//! for overflow, so every command stays reachable however narrow the window.
//! The extent cache is the sole source for intrinsic sizing, fitting and placement.
//!
//! Commands are `Label`-role plain buttons; a toggle that is on is a lift;
//! groups are split by hairline separators that never stand at the start or
//! end of what is shown. A painted widget hosts one with
//! [`Toolbar::lay_out_in`]. The band behind a window's toolbar row is
//! [`paint_band`] with its controls inset by [`band_padding`].
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

pub enum ToolbarItem {
    Button {
        /// The command's name: its visible label, or the accessible name
        /// and tooltip of an icon-only command.
        label: String,
        /// Shown instead of the label when present.
        icon: Option<crate::icons::Icon>,
        /// `Some` for a toggle; `Some(true)` paints the lift.
        active: Option<bool>,
        on_click: Box<dyn FnMut()>,
    },
    Separator,
    Spacer,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Item(usize),
    Overflow,
}

pub struct Toolbar {
    items: Vec<ToolbarItem>,
    orientation: Orientation,
    rect: Rect,
    item_rects: Vec<Rect>,
    visible_count: usize,
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
    hovered: Option<Target>,
    pointer: Option<Point>,
    pressed: Option<Target>,
    focused: bool,
    focus: Option<Target>,
    menu: ContextMenu,
    menu_indices: Vec<Option<usize>>,
    viewport: Option<Rect>,
    swallow_release: bool,
}

impl Toolbar {
    pub fn new(orientation: Orientation) -> Self {
        Self {
            items: Vec::new(),
            orientation,
            rect: Rect::ZERO,
            item_rects: Vec::new(),
            visible_count: 0,
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
            hovered: None,
            pointer: None,
            pressed: None,
            focused: false,
            focus: None,
            menu: ContextMenu::new(),
            menu_indices: Vec::new(),
            viewport: None,
            swallow_release: false,
        }
    }

    #[must_use]
    pub fn push_button(mut self, label: &str, on_click: impl FnMut() + 'static) -> Self {
        self.items.push(ToolbarItem::Button {
            label: label.to_string(),
            icon: None,
            active: None,
            on_click: Box::new(on_click),
        });
        self
    }
    /// An icon-only command; `label` is its accessible name, its tooltip and
    /// its row in the overflow menu.
    #[must_use]
    pub fn push_icon(
        mut self,
        icon: crate::icons::Icon,
        label: &str,
        on_click: impl FnMut() + 'static,
    ) -> Self {
        self.items.push(ToolbarItem::Button {
            label: label.to_string(),
            icon: Some(icon),
            active: None,
            on_click: Box::new(on_click),
        });
        self
    }
    /// A command that is on or off (bold, a panel's visibility): a lift
    /// while on, a checked row in the overflow menu.
    #[must_use]
    pub fn push_toggle(
        mut self,
        label: &str,
        active: bool,
        on_click: impl FnMut() + 'static,
    ) -> Self {
        self.items.push(ToolbarItem::Button {
            label: label.to_string(),
            icon: None,
            active: Some(active),
            on_click: Box::new(on_click),
        });
        self
    }
    /// Turn the toggle at `index` on or off (the host syncing from its model).
    pub fn set_active(&mut self, index: usize, on: bool) {
        if let Some(ToolbarItem::Button { active, .. }) = self.items.get_mut(index)
            && active.is_some()
        {
            *active = Some(on);
            self.menu_indices.clear();
            self.reconcile_menu();
        }
    }

    /// Lay the toolbar out in `rect` without a taffy pass, for a painted
    /// widget that hosts one in its own chrome. Measures with `text` in
    /// `theme`, then fits and places the commands; draw and dispatch as
    /// usual afterwards.
    pub fn lay_out_in(&mut self, rect: Rect, text: &mut TextRenderer, theme: &Theme) {
        self.resolve_theme(theme);
        self.update_measurements(text);
        self.rect = rect;
        self.place();
    }

    /// Whether the separator at `index` is shown: only between two shown
    /// commands, never first, last, or beside another separator.
    fn separator_shown(&self, index: usize) -> bool {
        let shown = self.items.get(..self.visible_count).unwrap_or_default();
        let before = shown.get(..index).unwrap_or_default();
        let after = shown.get(index.saturating_add(1)..).unwrap_or_default();
        let command_before = before
            .iter()
            .rev()
            .find(|item| !matches!(item, ToolbarItem::Spacer))
            .is_some_and(|item| matches!(item, ToolbarItem::Button { .. }));
        let command_after = after
            .iter()
            .any(|item| matches!(item, ToolbarItem::Button { .. }));
        command_before && command_after
    }
    #[must_use]
    pub fn push_separator(mut self) -> Self {
        self.items.push(ToolbarItem::Separator);
        self
    }
    #[must_use]
    pub fn push_spacer(mut self) -> Self {
        self.items.push(ToolbarItem::Spacer);
        self
    }

    pub fn has_overflow(&self) -> bool {
        self.items
            .iter()
            .skip(self.visible_count)
            .any(|item| matches!(item, ToolbarItem::Button { .. }))
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
                ToolbarItem::Button { label, .. } => {
                    let text = match self.orientation {
                        Orientation::Horizontal => count_to_f64(label.chars().count()) * 7.0,
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
        let total = axes.iter().sum::<f64>() + gap * count_to_f64(axes.len().saturating_sub(1));
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
        let extents: Vec<_> = self
            .items
            .iter()
            .map(|item| match item {
                ToolbarItem::Button { icon: Some(_), .. } => square,
                ToolbarItem::Button { label, .. } => button(label, text),
                ToolbarItem::Spacer => Size::ZERO,
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
        let changed = extents != self.extents || chip != self.chip_extent;
        self.extents = extents;
        self.chip_extent = chip;
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
        changed
    }

    fn fit(&self, budget: f64) -> usize {
        let gap = f64::from(self.gap);
        let mut used = 0.0;
        self.extents
            .iter()
            .enumerate()
            .take_while(|(index, size)| {
                used += self.axis(**size) + if *index > 0 { gap } else { 0.0 };
                used <= budget
            })
            .count()
    }

    fn place(&mut self) {
        let axis_total = match self.orientation {
            Orientation::Horizontal => self.rect.width(),
            Orientation::Vertical => self.rect.height(),
        }
        .max(0.0);
        let gap = f64::from(self.gap);
        self.visible_count = self.fit(axis_total);
        if self.has_overflow() {
            // The chip is one more sibling and takes its own gap.
            self.visible_count =
                self.fit((axis_total - self.axis(self.chip_extent) - gap).max(0.0));
        }
        let chip = if self.has_overflow() {
            self.axis(self.chip_extent).min(axis_total)
        } else {
            0.0
        };
        let used: f64 = self
            .extents
            .iter()
            .take(self.visible_count)
            .map(|size| self.axis(*size))
            .sum::<f64>()
            + gap * count_to_f64(self.visible_count.saturating_sub(1));
        let spacers = self
            .items
            .iter()
            .take(self.visible_count)
            .filter(|item| matches!(item, ToolbarItem::Spacer))
            .count();
        let chip_gap = if self.has_overflow() { gap } else { 0.0 };
        let spacer = if spacers == 0 {
            0.0
        } else {
            (axis_total - used - chip - chip_gap).max(0.0) / count_to_f64(spacers)
        };
        self.item_rects = vec![Rect::ZERO; self.items.len()];
        let mut cursor = match self.orientation {
            Orientation::Horizontal => self.rect.x0,
            Orientation::Vertical => self.rect.y0,
        };
        for (index, item) in self.items.iter().take(self.visible_count).enumerate() {
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
            if let Some(slot) = self.item_rects.get_mut(index) {
                *slot = rect;
            }
            cursor += extent + gap;
        }
        self.overflow_rect = if self.has_overflow() {
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
    }

    fn reconcile_menu(&mut self) {
        let mut indices = Vec::new();
        let mut separator = false;
        for (index, item) in self.items.iter().enumerate().skip(self.visible_count) {
            match item {
                ToolbarItem::Button { .. } => {
                    if separator && !indices.is_empty() {
                        indices.push(None);
                    }
                    indices.push(Some(index));
                    separator = false;
                }
                ToolbarItem::Separator => separator = true,
                ToolbarItem::Spacer => {}
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
                    .push(match index.and_then(|index| self.items.get(index)) {
                        Some(ToolbarItem::Button {
                            label,
                            icon,
                            active,
                            ..
                        }) => {
                            let mut row = MenuItem::action(label.clone(), || {});
                            if let Some(icon) = icon {
                                row = row.with_icon(*icon);
                            }
                            if let Some(active) = active {
                                row = row.with_checked(*active);
                            }
                            row
                        }
                        _ => MenuItem::separator(),
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
        self.menu
            .open_anchored(self.overflow_rect, self.viewport.unwrap_or(self.rect));
        self.focus = Some(Target::Overflow);
        self.hovered = None;
        self.pressed = None;
        if keyboard {
            self.menu.focus_first_action();
        }
    }
    fn invoke(&mut self, index: usize) {
        if let Some(ToolbarItem::Button { on_click, .. }) = self.items.get_mut(index) {
            on_click();
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
        if !self.menu.is_open() {
            self.focus = Some(Target::Overflow);
            self.pressed = None;
            self.refresh_hover();
        }
    }

    /// Popup routing still updates `pointer`, so closing or relayout cannot
    /// arm a command using a hover target from before the popup opened.
    fn refresh_hover(&mut self) -> bool {
        let hovered = if self.menu.is_open() {
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
            .take(self.visible_count)
            .filter_map(|(index, item)| {
                matches!(item, ToolbarItem::Button { .. }).then_some(Target::Item(index))
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
    fn activate(&mut self, target: Target) {
        match target {
            Target::Item(index) => self.invoke(index),
            Target::Overflow => self.open_menu(true),
        }
    }
    fn handle_key(&mut self, key: &Key, handled: &mut bool) {
        match key {
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                if let Some(target) = self.focus {
                    self.activate(target);
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
            Target::Item(index) if index >= self.visible_count => {
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

fn count_to_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}
#[allow(
    clippy::cast_possible_truncation,
    reason = "bounded logical layout dimensions"
)]
#[cfg(test)]
fn to_f32(value: f64) -> f32 {
    value as f32
}
impl Widget for Toolbar {
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.resolve_theme(theme);
        // Bootstrap only for legacy callers that skip renderer settlement.
        self.extents = self
            .items
            .iter()
            .map(|item| match item {
                ToolbarItem::Button { .. } => Size {
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
        let node = tree.leaf_node(Style {
            flex_shrink: 1.0,
            min_size: Size {
                width: Dimension::length(0.0),
                height: Dimension::length(0.0),
            },
            ..Default::default()
        });
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
    fn adjust_layout_with_measure(
        &mut self,
        tree: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        let changed = self.update_measurements(text);
        if changed {
            let _ = tree.mark_dirty(node);
        }
        changed
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = tree.layout(node).copied().unwrap_or_default();
        let x = origin.x + f64::from(layout.location.x);
        let y = origin.y + f64::from(layout.location.y);
        self.rect = Rect::new(
            x,
            y,
            x + f64::from(layout.size.width),
            y + f64::from(layout.size.height),
        );
        self.place();
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
        for (index, item) in self.items.iter().enumerate().take(self.visible_count) {
            let Some(rect) = self
                .item_rects
                .get(index)
                .copied()
                .filter(|rect| !rect.is_zero_area())
            else {
                continue;
            };
            match item {
                ToolbarItem::Button {
                    label,
                    icon,
                    active,
                    ..
                } => self.draw_button(
                    &mut PaintCtx::new(scene, transform, text, theme),
                    Target::Item(index),
                    label,
                    *icon,
                    active.unwrap_or(false),
                ),
                ToolbarItem::Separator if !self.separator_shown(index) => {}
                ToolbarItem::Separator => {
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
                ToolbarItem::Spacer => {}
            }
        }
        if self.has_overflow() {
            self.draw_button(
                &mut PaintCtx::new(scene, transform, text, theme),
                Target::Overflow,
                "More commands",
                Some(crate::icons::Icon::MoreHorizontal),
                false,
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
        self.menu.draw(scene, transform, text, theme, presence);
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
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
                self.pressed = self.hovered;
                if let Some(target) = self.pressed {
                    self.focus_target(target);
                    *ctx.focus_target = Some(ctx.node_id);
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
                    self.activate(target);
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
    fn is_focusable(&self) -> bool {
        self.items
            .iter()
            .any(|item| matches!(item, ToolbarItem::Button { .. }))
    }
    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if focused && self.focus.is_none() {
            self.focus = self.targets().first().copied();
        }
        if !focused {
            self.menu.close();
            self.pressed = None;
            self.refresh_hover();
        }
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn traps_focus(&self) -> bool {
        self.menu.is_open()
    }
    fn wants_pointer_capture(&self) -> bool {
        self.menu.is_open() || self.pressed.is_some() || self.swallow_release
    }
    fn modal_capture(&self) -> Option<Rect> {
        self.menu.modal_capture()
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
                let (name, route, expanded, checked) = match target {
                    Target::Item(index) => {
                        let (name, checked) = match self.items.get(*index) {
                            Some(ToolbarItem::Button { label, active, .. }) => {
                                (label.clone(), *active)
                            }
                            _ => (String::new(), None),
                        };
                        (name, format!("toolbar.item.{index}"), None, checked)
                    }
                    Target::Overflow => (
                        "More commands".to_owned(),
                        "toolbar.overflow".to_owned(),
                        Some(self.menu.is_open()),
                        None,
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
                    let ToolbarItem::Button { label, .. } = self.items.get(index)? else {
                        return None;
                    };
                    Some(
                        AccessNode::new(Role::MenuItem)
                            .with_name(label.clone())
                            .with_action_target(format!("toolbar.item.{index}"))
                            // Keep scrolled-out actions focusable so semantic focus
                            // can reveal them, but export no fabricated painted area.
                            .with_bounds(self.menu.item_rect(row).unwrap_or(Rect::ZERO))
                            .with_state(AccessState {
                                focusable: true,
                                focused: self.focused && self.menu.hovered() == Some(row),
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
            Target::Item(index) if index >= self.visible_count => self
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
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        let target = if target == "toolbar.overflow" && self.has_overflow() {
            Target::Overflow
        } else if let Some(index) = target
            .strip_prefix("toolbar.item.")
            .and_then(|index| index.parse::<usize>().ok())
        {
            if !matches!(self.items.get(index), Some(ToolbarItem::Button { .. }))
                || (index >= self.visible_count && !self.menu.is_open())
            {
                return false;
            }
            Target::Item(index)
        } else {
            return false;
        };
        match action {
            Action::Focus => self.focus_target(target),
            Action::Activate => {
                if let Target::Item(index) = target
                    && index >= self.visible_count
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
                    self.activate(target);
                }
            }
            _ => return false,
        }
        true
    }
}

impl Toolbar {
    /// The theme's metrics for a toolbar: `Label` text, `s3` insets and
    /// gaps, and the pointer target as every command's floor.
    fn resolve_theme(&mut self, theme: &Theme) {
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
        icon: Option<crate::icons::Icon>,
        active: bool,
    ) {
        let Some(rect) = self.target_rect(target).filter(|rect| !rect.is_zero_area()) else {
            return;
        };
        let theme = ctx.theme;
        let face = match icon {
            Some(icon) => crate::widget::ButtonFace::icon_only(rect, icon, label),
            None => crate::widget::ButtonFace::new(rect, label),
        }
        .role(crate::widget::ButtonRole::Plain)
        .toggled(active)
        .hovered(self.hovered == Some(target))
        .pressed(self.pressed == Some(target))
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
                        width: Dimension::length(to_f32(axis_extent)),
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
                    width: taffy::AvailableSpace::Definite(to_f32(axis_extent)),
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
            tb.adjust_layout_with_measure(&mut tree, node, &mut text);
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
    use winit::event::{DeviceId, KeyEvent};

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
    fn dispatch(tb: &mut Toolbar, event: WindowEvent) -> bool {
        let mut tree = TaffyTree::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        tb.handle_event(
            &event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Default::default(),
                state: &mut AutoCommit::new(),
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut None,
                node_id: node,
                taffy: &tree,
            },
        );
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
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.logical_key = Key::Named(key);
        event.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        event.location = winit::keyboard::KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
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
                                    &mut AutoCommit::new(),
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
                                    &mut AutoCommit::new(),
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
                    &mut AutoCommit::new(),
                    &automerge::ROOT
                ));
                let send = |host: &mut crate::widget::Row, event: WindowEvent| {
                    host.handle_event(
                        &event,
                        &mut crate::widget::EventCtx {
                            scale_factor: 1.0,
                            modifiers: &Default::default(),
                            state: &mut AutoCommit::new(),
                            path: &automerge::ROOT,
                            handled: &mut false,
                            focus_target: &mut None,
                            node_id: node,
                            taffy: &tree,
                        },
                    );
                };
                let mouse_event = |button, state| WindowEvent::MouseInput {
                    device_id: unsafe { std::mem::zeroed() },
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
                        device_id: unsafe { std::mem::zeroed() },
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
