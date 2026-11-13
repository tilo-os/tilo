//! The segmented control: a strip of segments of which one is chosen, its choice a thumb
//! one surface step off the track. `multiple` gives the multi-select form and `Tabs` the
//! page tabs.

use crate::num::f32_of_f64;
use crate::num::f64_of_usize;
use crate::widget::binding::{BoxedBinding, Pulled};
use crate::widget::toolbar::Orientation;
use crate::widget::{AppModel, ModelPath};
use automerge::{AutoCommit, ObjId};
use std::cell::Cell;
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::chrome::{SelectionState, SelectionTier, selection_tier_on};
use crate::color_compat::ColorCompat;
use crate::icons::Icon;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::colors::ON_FILL_MIN_CONTRAST;
#[path = "toggle_group_component.rs"]
mod component;
use crate::widget::{ActionEmitter, ControlBindingMetadata, ControlSize, TaffyTreeExt, Widget};
pub use component::{SegmentProps, ToggleGroupProps, ViewTabsProps};

#[path = "toggle_group_fold.rs"]
mod fold;
#[path = "toggle_group_layout.rs"]
mod layout;
use layout::{flow, packed, partition, partition_sized, stack, tab_fit};

/// No segment chosen, as [`ToggleGroup::selected`] says it ([`ToggleGroup::selection`]
/// says it as `None`).
pub const NONE_CHOSEN: usize = usize::MAX;

#[derive(Clone, Copy, Debug)]
pub struct SegmentedTouchGroup<'a> {
    id: usize,
    rects: &'a [Rect],
    selected: usize,
    swipe: bool,
}

impl<'a> SegmentedTouchGroup<'a> {
    #[must_use]
    pub const fn new(id: usize, rects: &'a [Rect], selected: usize) -> Self {
        Self {
            id,
            rects,
            selected,
            swipe: true,
        }
    }

    // Vertical and multi-select strips accept taps, not mode swipes.
    #[must_use]
    pub const fn tap_only(mut self) -> Self {
        self.swipe = false;
        self
    }

    #[must_use]
    pub fn contains(self, point: Point) -> bool {
        self.rects.iter().any(|rect| rect.contains(point))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentedTouchResult {
    Unhandled,
    Captured,
    Selected { group: usize, index: usize },
}

impl SegmentedTouchResult {
    #[must_use]
    pub fn handled(self) -> bool {
        self != Self::Unhandled
    }
}

#[derive(Clone, Copy, Debug)]
struct SegmentTouchStart {
    finger: u64,
    group: usize,
    point: Point,
    selected: usize,
    count: usize,
}

#[derive(Debug, Default)]
pub struct SegmentedTouch {
    contacts: std::collections::BTreeSet<u64>,
    start: Option<SegmentTouchStart>,
}

impl SegmentedTouch {
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.contacts.is_empty()
    }

    pub fn cancel(&mut self) {
        self.contacts.clear();
        self.start = None;
    }

    pub fn handle(
        &mut self,
        touch: &winit::event::Touch,
        scale_factor: f64,
        groups: &[SegmentedTouchGroup<'_>],
    ) -> SegmentedTouchResult {
        use winit::event::TouchPhase;
        let point = Point::new(
            touch.location.x / scale_factor,
            touch.location.y / scale_factor,
        );
        match touch.phase {
            TouchPhase::Started => {
                if self.is_active() {
                    self.contacts.insert(touch.id);
                    self.start = None;
                    return SegmentedTouchResult::Captured;
                }
                let Some(group) = groups.iter().find(|group| group.contains(point)) else {
                    return SegmentedTouchResult::Unhandled;
                };
                self.contacts.insert(touch.id);
                self.start = Some(SegmentTouchStart {
                    finger: touch.id,
                    group: group.id,
                    point,
                    selected: group.selected,
                    count: group.rects.len(),
                });
                SegmentedTouchResult::Captured
            }
            TouchPhase::Moved if self.contacts.contains(&touch.id) => {
                SegmentedTouchResult::Captured
            }
            TouchPhase::Cancelled if self.contacts.remove(&touch.id) => {
                self.start = None;
                SegmentedTouchResult::Captured
            }
            TouchPhase::Ended if self.contacts.remove(&touch.id) => {
                let start = self.start.take();
                let Some(start) =
                    start.filter(|start| start.finger == touch.id && !self.is_active())
                else {
                    return SegmentedTouchResult::Captured;
                };
                let Some(group) = groups.iter().find(|group| {
                    (group.id, group.selected, group.rects.len())
                        == (start.group, start.selected, start.count)
                }) else {
                    return SegmentedTouchResult::Captured;
                };
                let delta = point - start.point;
                let height = group
                    .rects
                    .iter()
                    .map(|rect| rect.height())
                    .fold(0.0, f64::max);
                let swipe = group
                    .swipe
                    .then(|| {
                        ToggleGroup::swipe_selection(
                            delta,
                            height,
                            group.selected,
                            group.rects.len(),
                        )
                    })
                    .flatten();
                let tap = (delta.hypot() <= crate::gesture::TouchGestureArena::TAP_SLOP)
                    .then(|| group.rects.iter().position(|rect| rect.contains(point)))
                    .flatten();
                swipe
                    .or(tap)
                    .map_or(SegmentedTouchResult::Captured, |index| {
                        SegmentedTouchResult::Selected {
                            group: group.id,
                            index,
                        }
                    })
            }
            _ => SegmentedTouchResult::Unhandled,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub label: String,
    pub short: Option<String>,
    pub icon: Option<Icon>,
    pub hint: Option<String>,
    pub disabled: bool,
}

impl Segment {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            short: None,
            icon: None,
            hint: None,
            disabled: false,
        }
    }

    pub fn icon(icon: Icon, name: impl Into<String>) -> Self {
        Self {
            label: name.into(),
            short: None,
            icon: Some(icon),
            hint: None,
            disabled: false,
        }
    }

    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.disabled = !enabled;
        self
    }

    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    fn tooltip(&self) -> String {
        match &self.hint {
            Some(hint) => format!("{}  {hint}", self.label),
            None => self.label.clone(),
        }
    }

    #[must_use]
    pub fn short(mut self, label: impl Into<String>) -> Self {
        self.short = Some(label.into());
        self
    }
}

struct Choice {
    pull: Box<dyn FnMut() -> Option<usize>>,
    push: Box<dyn FnMut(usize)>,
}

struct Chosen {
    pull: Box<dyn FnMut() -> Option<Vec<bool>>>,
    push: Box<dyn FnMut(&[bool])>,
}

type ChangeCallback = Box<dyn FnMut(usize)>;

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent cached interaction/layout flags are orthogonal states, not one state machine"
)]
pub struct ToggleGroup {
    id: Option<String>,
    segments: Vec<Segment>,
    selected: usize,
    chosen: Option<Vec<bool>>,
    cursor: usize,
    on_change: Option<ChangeCallback>,
    action: Option<ActionEmitter<usize>>,
    chosen_action: Option<ActionEmitter<Vec<usize>>>,
    chosen_state: Option<Chosen>,
    binding: Option<BoxedBinding<usize>>,
    native_resource: Option<crate::tilo_actions::ResourceSpec>,
    segment_rects: Vec<Rect>,
    outer: Rect,
    rect: Rect,
    caption: Option<String>,
    caption_height: f64,
    accessible_name: Option<String>,
    name_resolved: bool,
    enabled: bool,
    last_cursor: Cell<Point>,
    hovered: Option<usize>,
    painted: crate::widget::CachedPaint,
    cache_dirty: bool,
    last_selected: usize,
    is_focused: bool,
    last_focused: bool,
    last_hovered: Option<usize>,
    drawn_palette: Option<[Color; 4]>,
    fill: bool,
    style: SegmentedStyle,
    control_size: ControlSize,
    metrics: StripMetrics,
    naturals: Vec<(f64, f64)>,
    use_short: Vec<bool>,
    measured_width: Option<f32>,
    rows: usize,
    scroll_x: f64,
    touch: SegmentedTouch,
    layout_dirty: bool,
    pressed_into_focus: bool,
    orientation: Orientation,
    stacked: (f64, f64),
    choice: Option<Choice>,
    thumb: crate::motion::MotionValue<Rect>,
    thumb_for: usize,
    last_thumb: Option<Rect>,
    folded: bool,
    menu: crate::widget::ContextMenu,
    viewport: Option<Rect>,
    interaction: crate::interaction::InteractionEnvironment,
    in_window_bar: bool,
}

impl ToggleGroup {
    #[must_use]
    pub fn into_widget(self) -> Box<dyn crate::widget::Widget> {
        Box::new(self)
    }

    pub fn new(segments: Vec<Segment>, selected: usize) -> Self {
        let len = segments.len();
        // Saturate rather than panic on an out-of-bounds index: construction stays infallible.
        let selected = if selected == NONE_CHOSEN {
            NONE_CHOSEN
        } else {
            selected.min(len.saturating_sub(1))
        };
        Self {
            id: None,
            segments,
            selected,
            chosen: None,
            cursor: 0,
            on_change: None,
            action: None,
            chosen_action: None,
            chosen_state: None,
            binding: None,
            native_resource: None,
            segment_rects: Vec::new(),
            outer: Rect::ZERO,
            rect: Rect::ZERO,
            caption: None,
            caption_height: 0.0,
            accessible_name: None,
            name_resolved: false,
            enabled: true,
            last_cursor: Cell::new(Point::ZERO),
            hovered: None,
            painted: crate::widget::CachedPaint::default(),
            cache_dirty: true,
            last_selected: usize::MAX - 1,
            is_focused: false,
            last_focused: false,
            last_hovered: None,
            drawn_palette: None,
            fill: false,
            style: SegmentedStyle::Segmented,
            control_size: ControlSize::Regular,
            metrics: StripMetrics::stock(ControlSize::Regular),
            naturals: Vec::new(),
            use_short: Vec::new(),
            measured_width: None,
            rows: 1,
            scroll_x: 0.0,
            touch: SegmentedTouch::default(),
            layout_dirty: true,
            pressed_into_focus: false,
            orientation: Orientation::Horizontal,
            stacked: (0.0, 0.0),
            choice: None,
            thumb: crate::motion::MotionValue::new(Rect::ZERO),
            thumb_for: NONE_CHOSEN,
            last_thumb: None,
            folded: false,
            menu: Self::fold_menu(),
            interaction: crate::interaction::InteractionEnvironment::default(),
            in_window_bar: false,
            viewport: None,
        }
    }

    pub fn from_state<T: Clone + PartialEq + 'static>(
        choices: Vec<(T, Segment)>,
        state: &crate::LocalState<T>,
    ) -> Self {
        let (values, segments): (Vec<T>, Vec<Segment>) = choices.into_iter().unzip();
        let values = std::rc::Rc::new(values);
        let (current, binding) = crate::local_state::LocalBinding::new(state);
        let binding = std::rc::Rc::new(std::cell::RefCell::new(binding));
        let selected = values.iter().position(|value| *value == current);
        let mut group = Self::new(segments, selected.unwrap_or(NONE_CHOSEN));
        let (pull_values, pull_binding) =
            (std::rc::Rc::clone(&values), std::rc::Rc::clone(&binding));
        group.choice = Some(Choice {
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
        group
    }

    #[must_use]
    pub fn vertical(mut self) -> Self {
        self.orientation = Orientation::Vertical;
        self.style = SegmentedStyle::Quiet;
        self.cache_dirty = true;
        self
    }

    pub fn unchosen(segments: Vec<Segment>) -> Self {
        Self::new(segments, NONE_CHOSEN)
    }

    pub fn multiple(segments: Vec<Segment>, chosen: Vec<bool>) -> Self {
        let mut chosen = chosen;
        chosen.resize(segments.len(), false);
        let mut group = Self::new(segments, NONE_CHOSEN);
        group.chosen = Some(chosen);
        group
    }

    pub fn multiple_from_state<T: Clone + Ord + 'static>(
        choices: Vec<(T, Segment)>,
        state: &crate::LocalState<std::collections::BTreeSet<T>>,
    ) -> Self {
        let (values, segments): (Vec<T>, Vec<Segment>) = choices.into_iter().unzip();
        let values = std::rc::Rc::new(values);
        let (current, binding) = crate::local_state::LocalBinding::new(state);
        let binding = std::rc::Rc::new(std::cell::RefCell::new(binding));
        let flags = {
            let values = std::rc::Rc::clone(&values);
            move |set: &std::collections::BTreeSet<T>| -> Vec<bool> {
                values.iter().map(|value| set.contains(value)).collect()
            }
        };
        let mut group = Self::multiple(segments, flags(&current));
        let pull_binding = std::rc::Rc::clone(&binding);
        group.chosen_state = Some(Chosen {
            pull: Box::new(move || pull_binding.borrow_mut().poll().map(|set| flags(&set))),
            push: Box::new(move |chosen| {
                let set = values
                    .iter()
                    .zip(chosen)
                    .filter(|(_, on)| **on)
                    .map(|(value, _)| value.clone())
                    .collect();
                binding.borrow_mut().publish(&set);
            }),
        });
        group
    }

    #[must_use]
    pub fn fill(mut self) -> Self {
        self.fill = true;
        self.layout_dirty = true;
        self
    }

    #[must_use]
    pub fn with_style(mut self, style: SegmentedStyle) -> Self {
        self.style = style;
        self.cache_dirty = true;
        self.layout_dirty = true;
        self
    }

    #[must_use]
    pub fn control_size(mut self, size: ControlSize) -> Self {
        self.control_size = size;
        self.metrics = StripMetrics::stock(size);
        self.cache_dirty = true;
        self.layout_dirty = true;
        self
    }

    #[must_use]
    pub fn caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self.layout_dirty = true;
        self
    }

    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self.name_resolved = false;
        self
    }

    #[must_use]
    pub fn with_resolved_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self.name_resolved = true;
        self
    }

    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.set_enabled(enabled);
        self
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.cache_dirty = true;
            if !enabled {
                self.hovered = None;
            }
        }
    }

    #[must_use]
    pub fn bind_selection(mut self, state: &crate::LocalState<usize>) -> Self {
        let (selected, binding) = super::binding::local(state);
        self.set_selected(selected);
        self.binding = Some(binding);
        self
    }

    #[must_use]
    pub fn on_change<F: FnMut(usize) + 'static>(mut self, f: F) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    #[must_use]
    pub fn on_chosen_action(
        mut self,
        binding: crate::tilo_actions::ActionBinding<Vec<usize>>,
    ) -> Self {
        self.chosen_action = Some(ActionEmitter::new(binding));
        self.chosen_state = None;
        self
    }

    pub fn set_chosen(&mut self, chosen: Vec<bool>) {
        let mut chosen = chosen;
        chosen.resize(self.segments.len(), false);
        if self.chosen.as_ref() != Some(&chosen) {
            self.chosen = Some(chosen);
            self.cache_dirty = true;
        }
    }

    #[must_use]
    pub fn on_action(mut self, binding: crate::tilo_actions::ActionBinding<usize>) -> Self {
        self.action = Some(ActionEmitter::new(binding));
        self.native_resource = None;
        self.on_change = None;
        self
    }

    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    #[must_use]
    pub fn on_view<F: FnMut(usize) + 'static>(self, id: &str, f: F) -> Self {
        self.on_view_boxed(id, Box::new(f))
    }

    #[expect(
        clippy::expect_used,
        reason = "the generated integer schema and action are internally consistent"
    )]
    fn on_view_boxed(mut self, id: &str, f: Box<dyn FnMut(usize)>) -> Self {
        use crate::tilo_actions::{
            ActionBinding, ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType,
            ResourceSpec,
        };

        const TYPE_ID: &str = "tilo.ui.segmented.select";
        self.id = Some(id.to_owned());
        let control_id = id.to_owned();
        let spec = ActionSpec::typed_operation(
            "select",
            "Select",
            "Choose a segment",
            OperationKind::View,
            TYPE_ID,
            vec![ActionParam::required(
                "index",
                ParamType::Integer,
                "Zero-based segment index",
            )],
            move |args| {
                EncodedAction::encode_operation(
                    TYPE_ID,
                    &serde_json::json!({
                        "control": control_id,
                        "index": args.integer("index").unwrap_or_default(),
                    }),
                )
                .expect("generated segmented action payload is serializable")
            },
        );
        let resource =
            ResourceSpec::new(format!("tilo.control:{id}"), "tilo.control.segmented", id)
                .with_action(spec.clone());
        let binding = ActionBinding::new(&resource, &spec, |index: usize| {
            serde_json::Map::from_iter([("index".into(), serde_json::json!(index))])
        })
        .expect("segment index matches the generated integer parameter");
        self.action = Some(ActionEmitter::new(binding));
        self.native_resource = Some(resource);
        self.on_change = Some(f);
        self
    }

    fn request_selection(&mut self, index: usize) {
        if self.segments.get(index).is_some_and(|s| s.disabled) {
            return;
        }
        if self.chosen.is_some() {
            self.toggle_chosen(index);
            return;
        }
        if self.selected == index {
            return;
        }
        if let Some(action) = &mut self.action {
            if let Err(error) = action.emit(index) {
                tracing::warn!(%error, "segmented action refused its index");
            }
            return;
        }
        self.apply_selection(index);
    }

    fn apply_selection(&mut self, index: usize) {
        self.selected = index;
        self.cache_dirty = true;
        self.reveal(index);
        if let Some(binding) = &mut self.binding {
            binding.publish(&index);
        }
        if let Some(choice) = self.choice.as_mut() {
            (choice.push)(index);
        }
        if let Some(callback) = self.on_change.as_mut() {
            callback(index);
        }
    }

    fn toggle_chosen(&mut self, index: usize) {
        let Some(mut next) = self.chosen.clone() else {
            return;
        };
        let Some(slot) = next.get_mut(index) else {
            return;
        };
        *slot = !*slot;
        self.cursor = index;
        self.cache_dirty = true;
        if let Some(action) = &mut self.chosen_action {
            let indices = next
                .iter()
                .enumerate()
                .filter(|(_, on)| **on)
                .map(|(index, _)| index)
                .collect();
            if let Err(error) = action.emit(indices) {
                tracing::warn!(%error, "segmented action refused its choices");
            }
            return;
        }
        if let Some(state) = self.chosen_state.as_mut() {
            (state.push)(&next);
        }
        self.chosen = Some(next);
    }

    fn segment_prefix(&self) -> String {
        self.id
            .as_ref()
            .map_or_else(|| "segment:".to_owned(), |id| format!("{id}:segment:"))
    }

    fn segment_id(&self, index: usize) -> String {
        format!("{}{index}", self.segment_prefix())
    }

    #[must_use]
    pub fn count(&self) -> usize {
        self.segments.len()
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn selection(&self) -> Option<usize> {
        (self.selected < self.segments.len()).then_some(self.selected)
    }

    pub fn chosen(&self) -> Option<&[bool]> {
        self.chosen.as_deref()
    }

    /// Does **not** fire `on_change`, which is for user-driven changes. [`NONE_CHOSEN`] clears
    /// the choice.
    pub fn set_selected(&mut self, selected: usize) {
        let clamped = if selected == NONE_CHOSEN {
            NONE_CHOSEN
        } else {
            selected.min(self.segments.len().saturating_sub(1))
        };
        if self.selected != clamped {
            self.selected = clamped;
            self.cache_dirty = true;
            self.reveal(clamped);
        }
    }

    pub fn set_selection(&mut self, selection: Option<usize>) {
        self.set_selected(selection.unwrap_or(NONE_CHOSEN));
    }

    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.place(rect);
    }

    #[must_use]
    pub fn segment_rect(&self, index: usize) -> Option<Rect> {
        self.segment_rects.get(index).copied()
    }

    fn place(&mut self, outer: Rect) {
        self.outer = outer;
        let caption = if self.caption.is_some() {
            self.caption_height
        } else {
            0.0
        };
        let y0 = (outer.y0 + caption).min(outer.y1);
        self.set_rect_and_partition(Rect::new(outer.x0, y0, outer.x1, outer.y1));
    }

    fn set_rect_and_partition(&mut self, rect: Rect) {
        self.rect = rect;
        let measured = self.naturals.len() == self.segments.len();
        let (rects, use_short) = if self.orientation == Orientation::Vertical {
            (
                stack(rect, self.segments.len()),
                vec![false; self.segments.len()],
            )
        } else if self.chosen.is_some() && measured {
            let (rects, rows) = flow(rect, &self.naturals, self.face_height(), self.metrics.gap);
            self.rows = rows;
            (rects, vec![false; self.segments.len()])
        } else if self.style == SegmentedStyle::Tabs && measured {
            let (widths, use_short) = tab_fit(rect.width(), &self.naturals);
            let content: f64 = widths.iter().map(|(width, _)| width).sum();
            self.scroll_x = self.scroll_x.clamp(0.0, (content - rect.width()).max(0.0));
            (packed(rect, &widths, self.scroll_x), use_short)
        } else if measured {
            partition_sized(rect, &self.naturals)
        } else {
            (
                partition(rect, self.segments.len()),
                vec![false; self.segments.len()],
            )
        };
        if rects != self.segment_rects || use_short != self.use_short {
            self.cache_dirty = true;
        }
        self.segment_rects = rects;
        self.use_short = use_short;
        self.refold(measured);
        if self.painted.resized(self.rect) {
            self.cache_dirty = true;
        }
    }

    fn reveal(&mut self, index: usize) {
        if self.style != SegmentedStyle::Tabs || self.naturals.len() != self.segments.len() {
            return;
        }
        let (widths, _) = tab_fit(self.rect.width(), &self.naturals);
        let start: f64 = widths.iter().take(index).map(|(width, _)| width).sum();
        let Some(&(width, _)) = widths.get(index) else {
            return;
        };
        let room = self.rect.width();
        let scroll = if start < self.scroll_x {
            start
        } else if start + width > self.scroll_x + room {
            start + width - room
        } else {
            self.scroll_x
        };
        if (scroll - self.scroll_x).abs() > f64::EPSILON {
            self.scroll_x = scroll;
            self.set_rect_and_partition(self.rect);
        }
    }

    fn overflows(&self) -> bool {
        self.style == SegmentedStyle::Tabs
            && tab_fit(self.rect.width(), &self.naturals)
                .0
                .iter()
                .map(|(width, _)| width)
                .sum::<f64>()
                > self.rect.width() + 0.5
    }

    pub fn separator_count(&self) -> usize {
        separators(self.segments.len(), self.selected, self.style).count()
    }

    fn adopt_theme(&mut self, theme: &Theme) -> bool {
        let metrics = StripMetrics::of(self.control_size, theme);
        let caption = if self.caption.is_some() {
            f64::from(theme.text(TextRole::Caption).line_height_px())
                + f64::from(theme.tokens.spacing.s1)
        } else {
            0.0
        };
        let changed =
            metrics != self.metrics || (caption - self.caption_height).abs() > f64::EPSILON;
        self.metrics = metrics;
        self.caption_height = caption;
        changed
    }

    fn focus_visible(&self) -> bool {
        self.is_focused && !self.pressed_into_focus && crate::focus_visible::shown()
    }

    fn strip_height(&self) -> f64 {
        if self.chosen.is_some() {
            let rows = f64_of_usize(self.rows.max(1));
            rows * self.face_height() + (rows - 1.0) * self.metrics.gap
        } else {
            self.face_height()
        }
    }

    fn face_height(&self) -> f64 {
        if self.in_window_bar || self.interaction.supports_hover() {
            self.metrics.height
        } else {
            self.metrics
                .height
                .max(f64::from(self.interaction.minimum_target()))
        }
    }

    fn target_rect(&self, rect: Rect) -> Rect {
        let side = f64::from(self.interaction.minimum_target());
        let h = rect.height().max(side);
        Rect::new(
            rect.x0,
            rect.center().y - h / 2.0,
            rect.x1,
            rect.center().y + h / 2.0,
        )
    }
}

fn separators(n: usize, selected: usize, style: SegmentedStyle) -> impl Iterator<Item = usize> {
    (0..n.saturating_sub(1)).filter(move |i| {
        style == SegmentedStyle::Segmented && *i != selected && i.saturating_add(1) != selected
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SegmentedStyle {
    #[default]
    Segmented,
    Tabs,
    Quiet,
}

#[derive(Clone, Debug, PartialEq)]
struct StripMetrics {
    height: f64,
    inset: f64,
    pad_x: f64,
    gap: f64,
    icon: f64,
    icon_gap: f64,
    track_radius: f64,
    thumb_radius: f64,
    label: TextStyle,
}

impl StripMetrics {
    fn of(size: ControlSize, theme: &Theme) -> Self {
        let radii = &theme.tokens.radii;
        let inset = (radii.control - radii.item).max(0.0);
        let label = size.label_style(theme);
        Self {
            height: size.height(theme),
            inset: f64::from(inset),
            pad_x: f64::from(theme.tokens.spacing.s3),
            gap: f64::from(theme.tokens.spacing.s2),
            icon: f64::from(label.line_height_px()).round(),
            icon_gap: f64::from(theme.tokens.spacing.s1),
            track_radius: f64::from(radii.control),
            thumb_radius: f64::from(radii.concentric(radii.control, inset)),
            label,
        }
    }

    fn stock(size: ControlSize) -> Self {
        static STOCK: std::sync::LazyLock<[StripMetrics; 3]> = std::sync::LazyLock::new(|| {
            let theme = Theme::dark();
            [
                StripMetrics::of(ControlSize::Small, &theme),
                StripMetrics::of(ControlSize::Regular, &theme),
                StripMetrics::of(ControlSize::Large, &theme),
            ]
        });
        let index = match size {
            ControlSize::Small => 0,
            ControlSize::Regular => 1,
            ControlSize::Large => 2,
        };
        STOCK
            .get(index)
            .cloned()
            .unwrap_or_else(|| Self::of(size, &Theme::dark()))
    }

    fn natural(&self, label: &str, icon: bool, text: &mut TextRenderer) -> f64 {
        if icon {
            return self.height.ceil();
        }
        let w = f64::from(
            text.measure_style(label, None, &self.label.clone().strong())
                .0,
        );
        (w + 2.0 * self.pad_x).max(self.height).ceil()
    }

    fn tab_natural(&self, label: &str, icon: bool, text: &mut TextRenderer) -> f64 {
        let w = f64::from(
            text.measure_style(label, None, &self.label.clone().strong())
                .0,
        );
        let lead = if icon { self.icon + self.icon_gap } else { 0.0 };
        (lead + w + 2.0 * self.pad_x).ceil()
    }

    fn naturals(
        &self,
        segments: &[Segment],
        style: SegmentedStyle,
        text: &mut TextRenderer,
    ) -> Vec<(f64, f64)> {
        segments
            .iter()
            .map(|segment| {
                if style == SegmentedStyle::Tabs {
                    let icon = segment.icon.is_some();
                    let full = self.tab_natural(&segment.label, icon, text);
                    let short = segment
                        .short
                        .as_deref()
                        .map_or(full, |short| self.tab_natural(short, icon, text));
                    return (full, short);
                }
                let icon = segment.icon.is_some();
                let full = self.natural(&segment.label, icon, text);
                let short = segment
                    .short
                    .as_deref()
                    .filter(|_| !icon)
                    .map_or(full, |short| self.natural(short, false, text));
                (full, short)
            })
            .collect()
    }

    fn intrinsic_width(
        &self,
        segments: &[Segment],
        style: SegmentedStyle,
        text: &mut TextRenderer,
    ) -> f64 {
        let naturals = self.naturals(segments, style, text);
        if style == SegmentedStyle::Tabs {
            return naturals.iter().map(|(full, _)| full).sum::<f64>().ceil();
        }
        let widest = naturals.iter().map(|(full, _)| *full).fold(0.0, f64::max);
        (widest * f64_of_usize(segments.len()) + 2.0 * self.inset).ceil()
    }
}

pub fn segment_colors(
    theme: &Theme,
    style: SegmentedStyle,
    selected: bool,
    hovered: bool,
) -> (Option<Color>, Color) {
    let colors = &theme.tokens.colors;
    let ground = segment_ground(theme, style);
    let tier = segment_tier(theme, style, SelectionState::new(selected, hovered, false));
    let chosen = segment_tier(theme, style, SelectionState::active(true)).fill;
    let hover = segment_tier(theme, style, SelectionState::new(false, true, false)).fill;
    let quiet_ink = colors.ink_on_any(
        &[ground, crate::tokens::colors::composite(hover, ground)],
        colors.text_dim,
        ON_FILL_MIN_CONTRAST,
    );
    let fill = (selected || hovered).then_some(tier.fill);
    let ink = if selected {
        colors.ink_on_any(&[chosen], colors.text_primary, ON_FILL_MIN_CONTRAST)
    } else {
        quiet_ink
    };
    (fill, ink)
}

fn segment_ground(theme: &Theme, style: SegmentedStyle) -> Color {
    let surfaces = &theme.tokens.colors.surfaces;
    match style {
        SegmentedStyle::Segmented => surfaces.raised,
        SegmentedStyle::Tabs | SegmentedStyle::Quiet => surfaces.content,
    }
}

fn segment_tier(
    theme: &Theme,
    style: SegmentedStyle,
    state: SelectionState,
) -> crate::chrome::SelectionTier {
    let ground = segment_ground(theme, style);
    let palette = &theme.tokens.colors.palette;
    match style {
        SegmentedStyle::Segmented if !palette.dark && !palette.high_contrast && state.active => {
            SelectionTier {
                fill: theme.tokens.colors.surfaces.overlay,
                ..crate::chrome::selector_tier(theme, state, ground)
            }
        }
        SegmentedStyle::Segmented | SegmentedStyle::Tabs => {
            crate::chrome::selector_tier(theme, state, ground)
        }
        SegmentedStyle::Quiet => SelectionTier {
            border: None,
            inner_glow: None,
            ..selection_tier_on(theme, state, ground)
        },
    }
}

#[derive(Clone, Copy, Debug)]
struct FaceItem<'a> {
    disabled: bool,
    label: &'a str,
    short: Option<&'a str>,
    icon: Option<Icon>,
}

pub struct SegmentedFace<'a> {
    segments: &'a [Rect],
    items: Vec<FaceItem<'a>>,
    selected: usize,
    chosen: Option<&'a [bool]>,
    hovered: Option<usize>,
    focused: bool,
    disabled: bool,
    style: SegmentedStyle,
    control_size: ControlSize,
    thumb: Option<Rect>,
    stacked: bool,
    fold: Option<super::window_toolbar_row::LocationLift>,
}

struct StackedMetrics {
    pad_y: f64,
    gap: f64,
    icon: f64,
    label: TextStyle,
}

impl StackedMetrics {
    fn of(size: ControlSize, theme: &Theme) -> Self {
        let strip = StripMetrics::of(size, theme);
        Self {
            pad_y: f64::from(theme.tokens.spacing.s2),
            gap: f64::from(theme.tokens.spacing.s1),
            icon: strip.icon,
            label: theme.text(TextRole::Caption),
        }
    }

    fn line(&self) -> f64 {
        f64::from(self.label.line_height_px()).ceil()
    }

    fn height(&self) -> f64 {
        (2.0 * self.pad_y + self.icon + self.gap + self.line()).ceil()
    }
}

impl<'a> SegmentedFace<'a> {
    pub fn new(segments: &'a [Rect], labels: &'a [&'a str], selected: usize) -> Self {
        Self {
            segments,
            items: labels
                .iter()
                .map(|label| FaceItem {
                    disabled: false,
                    label,
                    short: None,
                    icon: None,
                })
                .collect(),
            selected,
            chosen: None,
            hovered: None,
            focused: false,
            disabled: false,
            style: SegmentedStyle::Segmented,
            control_size: ControlSize::Regular,
            thumb: None,
            stacked: false,
            fold: None,
        }
    }

    #[must_use]
    pub fn stacked(mut self, stacked: bool) -> Self {
        self.stacked = stacked;
        self
    }

    pub fn stacked_height(size: ControlSize, theme: &Theme) -> f64 {
        StackedMetrics::of(size, theme).height()
    }

    pub fn stacked_segment_width(
        labels: &[&str],
        size: ControlSize,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        let strip = StripMetrics::of(size, theme);
        let stacked = StackedMetrics::of(size, theme);
        let strong = stacked.label.clone().strong();
        let widest = labels
            .iter()
            .map(|label| f64::from(text.measure_style(label, None, &strong).0))
            .fold(stacked.icon, f64::max);
        (widest + 2.0 * strip.pad_x)
            .max(stacked.height() - 2.0 * strip.inset)
            .ceil()
    }

    #[must_use]
    pub fn thumb_at(mut self, rect: Option<Rect>) -> Self {
        self.thumb = rect;
        self
    }

    pub fn of_segments(segments: &'a [Rect], items: &'a [Segment], selected: usize) -> Self {
        let mut face = Self::new(segments, &[], selected);
        face.items = items
            .iter()
            .map(|segment| FaceItem {
                disabled: segment.disabled,
                label: &segment.label,
                short: segment.short.as_deref(),
                icon: segment.icon,
            })
            .collect();
        face
    }

    #[must_use]
    pub fn selection(mut self, selection: Option<usize>) -> Self {
        self.selected = selection.unwrap_or(NONE_CHOSEN);
        self
    }

    #[must_use]
    pub fn chosen(mut self, chosen: &'a [bool]) -> Self {
        self.chosen = Some(chosen);
        self
    }

    #[must_use]
    pub fn style(mut self, style: SegmentedStyle) -> Self {
        self.style = style;
        self
    }
    #[must_use]
    pub fn hovered(mut self, idx: Option<usize>) -> Self {
        self.hovered = idx;
        self
    }
    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }
    #[must_use]
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    #[must_use]
    pub fn control_size(mut self, size: ControlSize) -> Self {
        self.control_size = size;
        self
    }
    pub fn height(size: ControlSize, theme: &Theme) -> f64 {
        StripMetrics::of(size, theme).height
    }
    pub fn intrinsic_width(
        labels: &[&str],
        size: ControlSize,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        let segments: Vec<Segment> = labels.iter().map(|label| Segment::new(*label)).collect();
        StripMetrics::of(size, theme).intrinsic_width(&segments, SegmentedStyle::Segmented, text)
    }

    pub(crate) fn tab_widths(
        labels: &[&str],
        size: ControlSize,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> Vec<f64> {
        let segments: Vec<Segment> = labels.iter().map(|label| Segment::new(*label)).collect();
        StripMetrics::of(size, theme)
            .naturals(&segments, SegmentedStyle::Tabs, text)
            .into_iter()
            .map(|(full, _)| full)
            .collect()
    }
    pub fn partition(
        rect: Rect,
        labels: &[&str],
        size: ControlSize,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> Vec<Rect> {
        let segments: Vec<Segment> = labels.iter().map(|label| Segment::new(*label)).collect();
        Self::partition_segments(
            rect,
            &segments,
            SegmentedStyle::Segmented,
            size,
            text,
            theme,
        )
    }
    pub fn partition_segments(
        rect: Rect,
        segments: &[Segment],
        style: SegmentedStyle,
        size: ControlSize,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> Vec<Rect> {
        let metrics = StripMetrics::of(size, theme);
        let naturals = metrics.naturals(segments, style, text);
        if style == SegmentedStyle::Tabs {
            packed(rect, &naturals, 0.0)
        } else {
            partition_sized(rect, &naturals).0
        }
    }
}

impl ToggleGroup {
    fn paint_ground(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        face: &SegmentedFace<'_>,
        metrics: &StripMetrics,
        outer: Rect,
    ) {
        let colors = &theme.tokens.colors;
        match face.style {
            SegmentedStyle::Segmented if face.chosen.is_none() => {
                let track = colors.surfaces.raised;
                let edge = crate::surface::edge_on(track);
                let bordered = crate::surface::control_face(
                    scene,
                    transform,
                    &RoundedRect::from_rect(outer, metrics.track_radius),
                    track,
                    theme,
                );
                if !face.focused && !bordered {
                    scene.stroke(
                        &Stroke::new(1.0),
                        transform,
                        edge,
                        None,
                        &crate::surface::edge_outline(outer, metrics.track_radius),
                    );
                }
                // Separators stop short of the edge so they clear the corner radius, and vanish beside
                // the thumb.
                let inset = f64::from(theme.tokens.spacing.s2);
                for i in separators(face.segments.len(), face.selected, face.style) {
                    let Some(seg) = face.segments.get(i) else {
                        continue;
                    };
                    let x = seg.x1.round() - 0.5;
                    scene.stroke(
                        &Stroke::new(1.0),
                        transform,
                        edge,
                        None,
                        &Line::new(
                            Point::new(x, outer.y0 + inset),
                            Point::new(x, outer.y1 - inset),
                        ),
                    );
                }
            }
            SegmentedStyle::Segmented => {
                let track = colors.surfaces.raised;
                let edge = crate::surface::edge_on(track);
                for seg in face.segments {
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        track,
                        None,
                        &RoundedRect::from_rect(*seg, metrics.track_radius),
                    );
                    scene.stroke(
                        &Stroke::new(1.0),
                        transform,
                        edge,
                        None,
                        &crate::surface::edge_outline(*seg, metrics.track_radius),
                    );
                }
            }
            SegmentedStyle::Tabs | SegmentedStyle::Quiet => {}
        }
    }

    pub fn paint(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        face: &SegmentedFace<'_>,
    ) {
        if face.paint_folded(scene, transform, (text_renderer, theme)) {
            return;
        }
        let segs = face.segments;
        let (Some(first), Some(last)) = (segs.first(), segs.last()) else {
            return;
        };
        let metrics = StripMetrics::of(face.control_size, theme);
        let outer = segs.iter().fold(
            Rect::new(first.x0, first.y0, last.x1, first.y1),
            |acc, seg| acc.union(*seg),
        );
        if face.disabled {
            crate::widget::paint_disabled(scene, transform, &outer);
        }
        let hovered = face.hovered.filter(|index| {
            !face.disabled && face.items.get(*index).is_some_and(|item| !item.disabled)
        });
        let is_on = |i: usize| match face.chosen {
            Some(chosen) => chosen.get(i).copied().unwrap_or(false),
            None => i == face.selected,
        };
        Self::paint_ground(scene, transform, theme, face, &metrics, outer);

        let lifts_hover = face.chosen.is_some();
        for (i, seg) in segs.iter().enumerate() {
            let selected = is_on(i);
            let hovered = lifts_hover && hovered == Some(i) && !selected;
            if !(selected || hovered) {
                continue;
            }
            let tier = segment_tier(
                theme,
                face.style,
                SelectionState::new(selected, hovered, false),
            );
            let seg = match face.thumb {
                Some(thumb) if selected && face.chosen.is_none() => thumb,
                _ => *seg,
            };
            let body = RoundedRect::from_rect(seg.inset(-metrics.inset), metrics.thumb_radius);
            crate::chrome::paint_selector(
                scene,
                transform,
                &body,
                crate::chrome::SelectorRole::Item(&tier),
                theme,
            );
        }

        let selected_style = metrics.label.clone().strong();
        for (i, seg) in segs.iter().enumerate() {
            let Some(item) = face.items.get(i) else {
                continue;
            };
            let selected = is_on(i);
            let (_, ink) = segment_colors(theme, face.style, selected, hovered == Some(i));
            let ink = if item.disabled {
                ink.with_alpha_factor(crate::widget::DISABLED_ALPHA)
            } else {
                ink
            };
            if face.stacked
                && let Some(icon) = item.icon
            {
                let stacked = StackedMetrics::of(face.control_size, theme);
                let label_style = if selected {
                    stacked.label.clone().strong()
                } else {
                    stacked.label.clone()
                };
                let content = stacked.icon + stacked.gap + stacked.line();
                let top = seg.center().y - content / 2.0;
                crate::icons::draw_icon(
                    scene,
                    transform,
                    Rect::new(
                        seg.center().x - stacked.icon / 2.0,
                        top,
                        seg.center().x + stacked.icon / 2.0,
                        top + stacked.icon,
                    ),
                    icon,
                    ink,
                    crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
                let room = (seg.width() - 2.0 * metrics.pad_x.min(seg.width() / 4.0)).max(1.0);
                let shown = text_renderer.elide_to_width_style(
                    item.label,
                    f32_of_f64(room + 0.5),
                    &label_style,
                );
                let (tw, _) = text_renderer.measure_style(&shown, None, &label_style);
                #[allow(
                    clippy::arithmetic_side_effects,
                    reason = "Affine composition is float matrix multiplication; it cannot \
                              overflow or panic"
                )]
                let label_transform = transform
                    * Affine::translate((
                        seg.center().x - f64::from(tw) / 2.0,
                        top + stacked.icon + stacked.gap,
                    ));
                text_renderer.draw_style(scene, &shown, ink, label_transform, None, &label_style);
                continue;
            }
            let tabs = face.style == SegmentedStyle::Tabs;
            let pad = metrics.pad_x.min(seg.width() / 4.0);
            let mut lead = pad;
            if let Some(icon) = item.icon {
                let edge = metrics.icon.min(seg.height());
                let at = if tabs {
                    Rect::new(
                        seg.x0 + pad,
                        seg.center().y - edge / 2.0,
                        seg.x0 + pad + edge,
                        seg.center().y + edge / 2.0,
                    )
                } else {
                    Rect::from_center_size(seg.center(), (edge, edge))
                };
                crate::icons::draw_icon(
                    scene,
                    transform,
                    at,
                    icon,
                    ink,
                    crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
                if !tabs {
                    continue;
                }
                lead = pad + metrics.icon + metrics.icon_gap;
            }
            if item.label.is_empty() {
                continue;
            }
            let style = if selected {
                &selected_style
            } else {
                &metrics.label
            };
            let room = (seg.width() - lead - pad).max(1.0);
            let full_w = f64::from(text_renderer.measure_style(item.label, None, style).0);
            let label = match item.short {
                Some(short) if full_w > room + 0.5 => short,
                _ => item.label,
            };
            let shown = text_renderer.elide_to_width_style(label, f32_of_f64(room + 0.5), style);
            let (tw, th) = text_renderer.measure_style(&shown, None, style);
            let text_x = if tabs {
                seg.x0 + lead
            } else {
                seg.center().x - f64::from(tw) / 2.0
            };
            let text_y = seg.y0 + (seg.height() - f64::from(th)) / 2.0;
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine composition is float matrix multiplication; it cannot \
                          overflow or panic"
            )]
            let label_transform = transform * Affine::translate((text_x, text_y));
            text_renderer.draw_style(scene, &shown, ink, label_transform, None, style);
        }

        if face.disabled {
            scene.pop_layer();
        }
        if face.focused && !face.disabled {
            let ring = match face.style {
                SegmentedStyle::Tabs => segs.get(face.selected).copied().unwrap_or(outer),
                SegmentedStyle::Segmented | SegmentedStyle::Quiet => outer,
            };
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.tokens.colors.intent.accent_ink,
                None,
                &crate::surface::edge_outline(ring, metrics.track_radius),
            );
        }
    }

    #[cfg(test)]
    fn shown_labels(&self) -> Vec<String> {
        self.shown_segments().into_iter().map(|s| s.label).collect()
    }

    fn shown_segments(&self) -> Vec<Segment> {
        self.segments
            .iter()
            .enumerate()
            .map(|(i, segment)| match &segment.short {
                Some(short) if self.use_short.get(i).copied().unwrap_or(false) => Segment {
                    label: short.clone(),
                    short: None,
                    icon: segment.icon,
                    hint: segment.hint.clone(),
                    disabled: segment.disabled,
                },
                _ => Segment {
                    label: segment.label.clone(),
                    short: None,
                    icon: segment.icon,
                    hint: segment.hint.clone(),
                    disabled: segment.disabled,
                },
            })
            .collect()
    }

    fn measure_naturals(&mut self, text: &mut TextRenderer) -> bool {
        let naturals = self.metrics.naturals(&self.segments, self.style, text);
        if naturals == self.naturals {
            return false;
        }
        self.naturals = naturals;
        true
    }

    fn handle_key_event(&mut self, key: &KeyEvent, handled: &mut bool) {
        if !self.is_focused
            || !self.is_enabled()
            || key.state != ElementState::Pressed
            || key.repeat
        {
            return;
        }
        self.pressed_into_focus = false;
        if self.folded
            && matches!(
                key.logical_key,
                Key::Named(NamedKey::Enter | NamedKey::Space)
            )
        {
            self.open_fold_menu();
            *handled = true;
            return;
        }
        let len = self.segments.len();
        let current = if self.chosen.is_some() {
            self.cursor
        } else {
            self.selected
        };
        let none = current >= len;
        let step = |forward: bool| {
            let mut at = if none { len.saturating_sub(1) } else { current };
            for _ in 0..len {
                at = if forward {
                    if at.saturating_add(1) >= len {
                        0
                    } else {
                        at.saturating_add(1)
                    }
                } else {
                    at.checked_sub(1).unwrap_or_else(|| len.saturating_sub(1))
                };
                if self
                    .segments
                    .get(at)
                    .is_some_and(|segment| !segment.disabled)
                {
                    return Some(at);
                }
            }
            None
        };
        let moved = match key.logical_key {
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) if len > 0 => step(false),
            Key::Named(NamedKey::ArrowRight | NamedKey::ArrowDown) if len > 0 => step(true),
            Key::Named(NamedKey::Home) => {
                self.segments.iter().position(|segment| !segment.disabled)
            }
            Key::Named(NamedKey::End) => {
                self.segments.iter().rposition(|segment| !segment.disabled)
            }
            _ => None,
        };
        if self.chosen.is_some() {
            if let Some(index) = moved {
                self.cursor = index;
                self.cache_dirty = true;
                *handled = true;
            } else if matches!(
                key.logical_key,
                Key::Named(NamedKey::Enter | NamedKey::Space)
            ) && self.cursor < len
            {
                self.toggle_chosen(self.cursor);
                *handled = true;
            }
            return;
        }
        let selection = moved.or(match key.logical_key {
            Key::Named(NamedKey::Enter | NamedKey::Space) if !none => Some(self.selected),
            _ => None,
        });
        if let Some(selection) = selection {
            *handled = true;
            self.request_selection(selection);
        }
    }

    fn handle_pointer_press(
        &mut self,
        state: ElementState,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
    ) {
        if state != ElementState::Pressed || !self.is_enabled() {
            return;
        }
        let pos = self.last_cursor.get();
        if !self.rect.contains(pos) {
            return;
        }
        if self.folded {
            *handled = true;
            self.pressed_into_focus |= !self.is_focused;
            *focus_target = Some(node_id);
            self.open_fold_menu();
            return;
        }
        if let Some(index) = self
            .segment_rects
            .iter()
            .position(|segment| segment.contains(pos))
        {
            *handled = true;
            self.pressed_into_focus |= !self.is_focused;
            *focus_target = Some(node_id);
            self.request_selection(index);
        }
    }

    // One deliberate horizontal swipe advances a mode; vertical scrolling and sub-target
    // drags do not change the choice.
    #[must_use]
    pub fn swipe_selection(
        delta: vello::kurbo::Vec2,
        target_size: f64,
        selected: usize,
        count: usize,
    ) -> Option<usize> {
        if count == 0
            || selected >= count
            || delta.x.abs()
                < target_size.max(f64::from(
                    crate::InteractionEnvironment::TOUCH.minimum_target(),
                ))
            || delta.x.abs() < delta.y.abs() * 1.5
        {
            return None;
        }
        let next = if delta.x < 0.0 {
            selected.saturating_add(1).min(count - 1)
        } else {
            selected.saturating_sub(1)
        };
        (next != selected).then_some(next)
    }

    fn handle_touch_event(
        &mut self,
        touch: &winit::event::Touch,
        scale_factor: f64,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
    ) {
        if !self.is_enabled() {
            self.touch.cancel();
            return;
        }
        if self.folded {
            let at = Point::new(
                touch.location.x / scale_factor,
                touch.location.y / scale_factor,
            );
            if self.target_rect(self.rect).contains(at) {
                *handled = true;
                if touch.phase == winit::event::TouchPhase::Ended {
                    self.pressed_into_focus |= !self.is_focused;
                    *focus_target = Some(node_id);
                    self.open_fold_menu();
                }
            }
            return;
        }
        let targets: Vec<Rect> = self
            .segment_rects
            .iter()
            .map(|rect| self.target_rect(*rect))
            .collect();
        let group = SegmentedTouchGroup::new(0, &targets, self.selected);
        let group = if self.orientation == Orientation::Horizontal && self.chosen.is_none() {
            group
        } else {
            group.tap_only()
        };
        let result = self.touch.handle(touch, scale_factor, &[group]);
        if result.handled() {
            *handled = true;
            if touch.phase == winit::event::TouchPhase::Started {
                self.pressed_into_focus |= !self.is_focused;
                *focus_target = Some(node_id);
            }
        }
        if let SegmentedTouchResult::Selected { index, .. } = result {
            self.request_selection(index);
        }
    }

    fn handle_wheel(&mut self, delta: &MouseScrollDelta, handled: &mut bool) {
        if !self.overflows() || !self.rect.contains(self.last_cursor.get()) {
            return;
        }
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => {
                let line = self.metrics.height;
                (f64::from(*x) * line, f64::from(*y) * line)
            }
            MouseScrollDelta::PixelDelta(p) => (p.x, p.y),
        };
        let step = if dx.abs() > dy.abs() { dx } else { dy };
        self.scroll_x -= step;
        self.set_rect_and_partition(self.rect);
        self.cache_dirty = true;
        *handled = true;
    }
}

#[cfg(test)]
fn tab_label_span(
    seg: &Rect,
    label: &str,
    metrics: &StripMetrics,
    text: &mut TextRenderer,
) -> (f64, f64) {
    let lead = metrics.pad_x.min(seg.width() / 4.0);
    let width = f64::from(
        text.measure_style(label, None, &metrics.label.clone().strong())
            .0,
    );
    let x0 = seg.x0 + lead;
    (x0, (x0 + width).min(seg.x1))
}

impl ToggleGroup {
    fn pin_measured_width(&self, taffy: &mut TaffyTree<()>, node: NodeId) -> bool {
        let Some(width) = self
            .measured_width
            .filter(|_| !self.fill && self.chosen.is_none())
        else {
            return false;
        };
        let Ok(style) = taffy.style(node) else {
            return false;
        };
        let width = Dimension::length(width);
        if style.size.width == width {
            return false;
        }
        let mut style = style.clone();
        style.size.width = width;
        taffy.set_style(node, style).is_ok()
    }

    fn select_segment_operation(
        &mut self,
        kind: crate::tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
    ) -> Result<bool, String> {
        if kind != crate::tilo_actions::OperationKind::View
            || type_id != "tilo.ui.segmented.select"
            || self.action.is_none()
        {
            return Ok(false);
        }
        let value: serde_json::Value =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        if value.get("control").and_then(serde_json::Value::as_str) != self.id.as_deref() {
            return Ok(false);
        }
        let index = value
            .get("index")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .filter(|index| *index < self.segments.len())
            .ok_or_else(|| "segmented action index is out of range".to_owned())?;
        self.apply_selection(index);
        Ok(true)
    }
}

impl ToggleGroup {
    fn follow_thumb(&mut self, motion: crate::tokens::EffectiveMotion) -> Option<Rect> {
        use crate::motion::{Direction, ListChange};
        let now = crate::time::now();
        let target = self
            .chosen
            .is_none()
            .then(|| self.segment_rects.get(self.selected).copied())
            .flatten();
        let Some(target) = target else {
            self.thumb_for = NONE_CHOSEN;
            return None;
        };
        let chose_another = self.thumb_for != self.selected
            && self.thumb_for != NONE_CHOSEN
            && self.thumb_for < self.segment_rects.len()
            && self.thumb.target().area() > 0.0;
        if chose_another {
            let timing =
                ListChange::MOVE.timing(Direction::Enter, motion, crate::motion::input_source());
            self.thumb.set(target, timing, now);
            if let Some(until) = self.thumb.settles_at() {
                crate::motion::want_frames_until(until);
            }
        } else if self.thumb_for == NONE_CHOSEN || self.thumb.target() != target {
            self.thumb.snap(target);
        }
        self.thumb_for = self.selected;
        self.thumb.is_moving(now).then(|| self.thumb.get(now))
    }
}

impl Widget for ToggleGroup {
    crate::described!();

    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::resources::PublishesResources,
        dyn crate::widget::ResourceOperations
    );

    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        if let Some(binding) = &mut self.binding
            && let Pulled::Value(selected) = binding.pull(state, root)
        {
            self.set_selected(selected);
        }
        if let Some(index) = self.choice.as_mut().and_then(|choice| (choice.pull)()) {
            self.set_selected(index);
        }
        if let Some(chosen) = self.chosen_state.as_mut().and_then(|state| (state.pull)()) {
            self.set_chosen(chosen);
        }
    }

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        if self.interaction != environment {
            self.interaction = environment;
            self.layout_dirty = true;
            self.cache_dirty = true;
        }
    }

    fn set_window_bar_control(&mut self, in_bar: bool) {
        if self.in_window_bar != in_bar {
            self.in_window_bar = in_bar;
            self.layout_dirty = true;
            self.cache_dirty = true;
        }
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        let index = self
            .segment_rects
            .iter()
            .position(|segment| segment.contains(point))?;
        let segment = self.segments.get(index)?;
        let rect = *self.segment_rects.get(index)?;
        (segment.icon.is_some() || segment.hint.is_some()).then(|| (rect, segment.tooltip()))
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        if self.adopt_theme(theme) {
            self.cache_dirty = true;
        }
        self.measured_width = None;
        if self.orientation == Orientation::Vertical {
            let side = self
                .face_height()
                .max(f64::from(theme.tokens.metrics.min_pointer_target));
            let gap = f64::from(theme.tokens.spacing.s1);
            self.stacked = (side, gap);
            let n = f64_of_usize(self.segments.len().max(1));
            let node = taffy.leaf_node(Style {
                size: Size::from_lengths(f32_of_f64(side), f32_of_f64(side * n + gap * (n - 1.0))),
                flex_shrink: 0.0,
                ..Default::default()
            });
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            return node;
        }
        let height = self.strip_height() + self.caption_height;
        let style = Style {
            size: Size {
                width: if self.fill || self.chosen.is_some() {
                    Dimension::percent(1.0)
                } else {
                    Dimension::auto()
                },
                height: if self.chosen.is_some() {
                    Dimension::auto()
                } else {
                    Dimension::length(f32_of_f64(height))
                },
            },
            max_size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            flex_shrink: if self.style == SegmentedStyle::Tabs {
                1.0
            } else {
                0.0
            },
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
        if self.orientation == Orientation::Vertical {
            let (side, gap) = self.stacked;
            let n = f64_of_usize(self.segments.len().max(1));
            return Size {
                width: known.width.unwrap_or(f32_of_f64(side)),
                height: known
                    .height
                    .unwrap_or(f32_of_f64(side * n + gap * (n - 1.0))),
            };
        }
        let intrinsic = f32_of_f64(
            self.metrics
                .intrinsic_width(&self.segments, self.style, text),
        );
        self.measure_naturals(text);
        if self.chosen.is_some() {
            let width = known.width.unwrap_or(match available.width {
                taffy::AvailableSpace::Definite(width) => width,
                _ => intrinsic,
            });
            let (_, rows) = flow(
                Rect::new(0.0, 0.0, f64::from(width), self.face_height()),
                &self.naturals,
                self.face_height(),
                self.metrics.gap,
            );
            self.rows = rows;
            return Size {
                width,
                height: f32_of_f64(self.strip_height() + self.caption_height),
            };
        }
        if !self.fill {
            self.measured_width = Some(intrinsic);
        }
        let fitted = match available.width {
            taffy::AvailableSpace::Definite(room) => intrinsic.min(room.max(0.0)),
            _ => intrinsic,
        };
        Size {
            width: known.width.unwrap_or(fitted),
            height: known
                .height
                .unwrap_or(f32_of_f64(self.strip_height() + self.caption_height)),
        }
    }

    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        if self.orientation == Orientation::Vertical {
            return false;
        }
        self.measure_naturals(text);
        if self.fill || self.chosen.is_some() {
            return false;
        }
        if self.measured_width.is_none() {
            self.measured_width = Some(f32_of_f64(self.metrics.intrinsic_width(
                &self.segments,
                self.style,
                text,
            )));
        }
        self.pin_measured_width(taffy, node)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.place(crate::widget::layout_rect(taffy, node, origin));
    }

    fn draw(
        &mut self,
        out_scene: &mut Scene,
        out_transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        if self.segments.is_empty() || self.segment_rects.len() != self.segments.len() {
            return;
        }

        if self.selected != self.last_selected {
            self.cache_dirty = true;
            self.last_selected = self.selected;
        }
        let thumb = self.follow_thumb(theme.motion());
        if thumb != self.last_thumb {
            self.cache_dirty = true;
            self.last_thumb = thumb;
        }
        if self.focus_visible() != self.last_focused {
            self.cache_dirty = true;
            self.last_focused = self.focus_visible();
        }
        if self.hovered != self.last_hovered {
            self.cache_dirty = true;
            self.last_hovered = self.hovered;
        }
        if self.adopt_theme(theme) {
            self.cache_dirty = true;
            self.measure_naturals(text_renderer);
            self.place(self.outer);
        }
        let colors = &theme.tokens.colors;
        let palette = [
            colors.surfaces.raised,
            colors.surfaces.content,
            colors.text_primary,
            colors.intent.accent_ink,
        ];
        if self.drawn_palette != Some(palette) {
            self.drawn_palette = Some(palette);
            self.cache_dirty = true;
        }
        if !self.cache_dirty && !self.painted.is_empty() {
            self.painted.append(out_scene, out_transform, self.rect);
            return;
        }
        if self.measure_naturals(text_renderer) {
            self.place(self.outer);
        }
        let mut painted = std::mem::take(&mut self.painted);
        let cached = painted.repaint(self.rect);
        if let Some(caption) = &self.caption {
            let style = theme.text(TextRole::Caption);
            let shown = text_renderer.elide_to_width_style(
                caption,
                f32_of_f64(self.outer.width().max(1.0)),
                &style,
            );
            let at = Affine::translate((self.outer.x0, self.outer.y0));
            text_renderer.draw_style(cached, &shown, theme.text_dim, at, None, &style);
        }
        let shown = self.shown_segments();
        let clip = self.overflows();
        if clip {
            cached.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                Affine::IDENTITY,
                &self.rect,
            );
        }
        let mut face = SegmentedFace::of_segments(&self.segment_rects, &shown, self.selected)
            .style(self.style)
            .control_size(self.control_size)
            .hovered(self.hovered)
            .focused(self.focus_visible() && self.chosen.is_none())
            .disabled(!self.is_enabled())
            .thumb_at(thumb);
        if let Some(chosen) = &self.chosen {
            face = face.chosen(chosen);
        }
        if self.folded {
            face = face.folded(self.hovered.is_some(), self.menu.is_open());
        }
        Self::paint(cached, Affine::IDENTITY, text_renderer, theme, &face);
        if clip {
            cached.pop_layer();
        }
        if self.chosen.is_some()
            && self.is_focused
            && let Some(seg) = self.segment_rects.get(self.cursor)
        {
            cached.stroke(
                &Stroke::new(2.0),
                Affine::IDENTITY,
                theme.tokens.colors.intent.accent_ink,
                None,
                &crate::surface::edge_outline(*seg, self.metrics.track_radius),
            );
        }
        painted.append(out_scene, out_transform, self.rect);
        self.painted = painted;
    }

    fn needs_animation_frame(&self) -> bool {
        self.thumb.is_moving(crate::motion::frame_now()) || self.menu.needs_animation_frame()
    }

    fn needs_redraw(&self) -> bool {
        self.cache_dirty
            || self.thumb.is_moving(crate::motion::frame_now())
            || self.last_thumb.is_some()
            || self.selected != self.last_selected
            || self.focus_visible() != self.last_focused
            || self.hovered != self.last_hovered
    }

    fn mark_drawn(&mut self) {
        self.cache_dirty = false;
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO || self.layout_dirty
    }

    fn layout_measured(&mut self) {
        self.layout_dirty = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor.set(ctx.logical(position));
        }
        if self.fold_menu_event(event, ctx) {
            return;
        }
        match event {
            WindowEvent::KeyboardInput { event, .. } => self.handle_key_event(event, ctx.handled),
            WindowEvent::CursorMoved { position, .. } => {
                let logical_pos = ctx.logical(position);
                self.hovered = if !self.is_enabled() || !self.rect.contains(logical_pos) {
                    None
                } else if self.folded {
                    Some(self.selected)
                } else {
                    self.segment_rects
                        .iter()
                        .position(|segment| segment.contains(logical_pos))
                };
            }
            WindowEvent::CursorLeft { .. } => self.hovered = None,
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } => self.handle_pointer_press(
                *element_state,
                ctx.handled,
                ctx.focus_target,
                ctx.node_id,
            ),
            WindowEvent::MouseWheel { delta, .. } => self.handle_wheel(delta, ctx.handled),
            WindowEvent::Touch(touch) => {
                self.handle_touch_event(
                    touch,
                    ctx.scale_factor,
                    ctx.handled,
                    ctx.focus_target,
                    ctx.node_id,
                );
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ToggleGroup"
    }

    fn wants_pointer_capture(&self) -> bool {
        self.touch.is_active() || self.menu.is_open()
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.draw_fold_menu(scene, transform, (text_renderer, theme, presence));
    }

    fn claims_raw_touch(&self, point: Point) -> bool {
        self.target_rect(self.rect).contains(point)
    }

    fn get_rect(&self) -> Rect {
        if self.caption.is_some() {
            self.outer
        } else {
            self.rect
        }
    }

    fn tooltip_side(&self) -> crate::widget::tooltip::TooltipSide {
        if self.orientation == Orientation::Vertical {
            crate::widget::tooltip::TooltipSide::Beside
        } else {
            crate::widget::tooltip::TooltipSide::Below
        }
    }

    fn is_focusable(&self) -> bool {
        self.is_enabled()
    }
    fn set_focus(&mut self, focused: bool) {
        self.pressed_into_focus &= focused;
        self.is_focused = focused;
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn is_enabled(&self) -> bool {
        self.enabled
            && self.action.as_ref().is_none_or(ActionEmitter::is_available)
            && self
                .chosen_action
                .as_ref()
                .is_none_or(ActionEmitter::is_available)
    }

    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Segment labels are runtime strings the host gives, not FTL keys, so no lookup.
        let value = if self.chosen.is_some() {
            None
        } else {
            self.segments.get(self.selected).map(|s| s.label.clone())
        };
        // Segments are drawn, not child widgets: each is a synthetic child so assistive
        // technology enumerates the choices, not just the active one.
        let count = self.segments.len();
        let seg_children: Vec<AccessNode> = self
            .segments
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let on = match &self.chosen {
                    Some(chosen) => chosen.get(i).copied().unwrap_or(false),
                    None => i == self.selected,
                };
                let node = AccessNode::new(Role::ToggleButton)
                    .with_action_target(self.segment_id(i))
                    .with_name(s.label.clone())
                    .with_state(
                        AccessState::new()
                            .checked(on)
                            .disabled(!self.is_enabled() || s.disabled),
                    )
                    .with_position(i.saturating_add(1), count);
                match self.segment_rects.get(i) {
                    Some(rect) => node.with_bounds(self.target_rect(*rect)),
                    None => node,
                }
            })
            .collect();
        let name = self
            .accessible_name
            .as_deref()
            .and_then(|name| crate::i18n::spoken_name(name, self.name_resolved))
            .or_else(|| self.caption.clone());
        let mut state = AccessState::new()
            .focusable(self.is_focusable())
            .focused(self.is_focused())
            .disabled(!self.is_enabled());
        state.expanded = self.folded.then(|| self.menu.is_open());
        Some(
            AccessNode::new(Role::Group)
                .with_optional_name(name)
                .with_optional_value(value)
                .with_state(state)
                .with_children(seg_children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if !self.is_enabled() {
            return false;
        }
        let prefix = self.segment_prefix();
        let Some(index) = target
            .strip_prefix(&prefix)
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|index| *index < self.segments.len())
        else {
            return false;
        };
        if !matches!(
            action,
            crate::a11y::Action::Activate | crate::a11y::Action::ToggleCheck
        ) {
            return false;
        }
        self.request_selection(index);
        true
    }
}

impl crate::widget::ResourceOperations for ToggleGroup {
    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        self.native_resource
            .as_ref()
            .map(|_| std::iter::once("tilo.ui.segmented.select".to_owned()).collect())
            .unwrap_or_default()
    }

    fn start_operation(
        &mut self,
        op: &super::ResourceOperation<'_>,
        _doc: &mut crate::widget::Edits<'_>,
        _path: &ObjId,
    ) -> Result<Option<super::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        Ok(self
            .select_segment_operation(request.kind, request.type_id, request.payload)?
            .then(|| {
                super::ResourceOperationStart::Completed(
                    crate::tilo_actions::ActionOutput::default(),
                )
            }))
    }
}

impl crate::resources::PublishesResources for ToggleGroup {
    fn resources(&self, _doc: &AutoCommit) -> Vec<crate::tilo_actions::ResourceSpec> {
        self.native_resource.iter().cloned().collect()
    }
}

impl crate::widget::controls::ActionControls for ToggleGroup {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        if let Some(action) = &mut self.action {
            let _ = action.install_custody(domain, attachment);
        }
        if let Some(action) = &mut self.chosen_action {
            let _ = action.install_custody(domain, attachment);
        }
    }
    fn take_control_invocation(&mut self) -> Option<crate::tilo_actions::ControlInvocation> {
        self.action
            .as_mut()
            .and_then(ActionEmitter::take)
            .or_else(|| self.chosen_action.as_mut().and_then(ActionEmitter::take))
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<crate::tilo_actions::ActionAddress>,
    ) {
        if let Some(action) = &mut self.action {
            action.refresh_availability(allowed);
        }
        if let Some(action) = &mut self.chosen_action {
            action.refresh_availability(allowed);
        }
    }
    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        if let Some(action) = &self.chosen_action {
            return (0..self.segments.len())
                .map(|index| action.metadata(self.segment_id(index)))
                .collect();
        }
        let Some(action) = &self.action else {
            if self.choice.is_none() {
                return Vec::new();
            }
            return (0..self.segments.len())
                .map(|index| ControlBindingMetadata {
                    control_id: self.segment_id(index),
                    kind: crate::widget::ControlBindingKind::SemanticAction(self.segment_id(index)),
                })
                .collect();
        };
        (0..self.segments.len())
            .map(|index| action.metadata(self.segment_id(index)))
            .collect()
    }
}

#[cfg(test)]
#[path = "toggle_group_tests.rs"]
mod tests;
