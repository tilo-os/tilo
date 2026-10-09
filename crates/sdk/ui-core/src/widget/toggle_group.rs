//! `ToggleGroup` — the segmented control. A strip of segments of which one
//! is chosen (or, before the person has chosen, none). Clicking a segment
//! chooses it and fires the `on_change` callback with the new index.
//!
//! State lives on the widget unless it is bound: [`ToggleGroup::bind_selection`]
//! keeps a [`crate::LocalState`] and the strip in step both ways, and
//! `on_change` / `on_action` report the person's choices.
//!
//! The chosen segment is a *lift* (HIG §9.3): a thumb one surface step off
//! the track, set concentrically inside it, with primary ink and no hue.
//!
//! [`ToggleGroup::multiple`] is the multi-select form (the weekdays an alarm
//! repeats on): every segment is its own toggle, each as wide as its label,
//! wrapping onto further rows when the strip runs out of room.
//!
//! Page tabs ([`SegmentedStyle::Tabs`]) start flush: the first label and its
//! underline sit on the strip's leading edge, each tab as wide as its label,
//! and a strip longer than its room scrolls sideways instead of eliding.
//!
//! [`ToggleGroup::from_state`] binds the choice to typed view state instead
//! of a callback. [`ToggleGroup::vertical`] stacks icon segments into a tool
//! rail: each segment shows its icon, and its label (with the
//! [`Segment::with_hint`] shortcut) is its tooltip; the label is its
//! accessible name.
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

use crate::chrome::{SelectionState, selection_tier_on};
use crate::color_compat::ColorCompat;
use crate::icons::Icon;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::colors::ON_FILL_MIN_CONTRAST;
use crate::widget::{ActionEmitter, ControlBindingMetadata, ControlSize, TaffyTreeExt, Widget};

/// No segment chosen: what [`ToggleGroup::selected`] and
/// [`SegmentedFace::new`] use for "none" ([`ToggleGroup::selection`] says
/// it as `None`).
pub const NONE_CHOSEN: usize = usize::MAX;

/// One painted segmented strip, identified independently of its position.
/// Keep `id` stable while the strip remains mounted.
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

    /// Vertical and multi-select strips accept taps, not mode swipes.
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

/// The input owner applies a selection through its existing admitted action.
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

/// The retained control and painted strips share one touch recognizer.
/// Only a completed single-finger gesture chooses; cancellation, a second
/// finger, a removed strip or an externally changed selection refuses it.
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

    /// Refuse an in-flight gesture when a modal or mode removes its owner.
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

/// Lossless `usize -> f64` widening for segment-strip geometry. Segment
/// counts are bounded far below `u32::MAX`, so routing through `u32` (which
/// `f64` represents exactly) avoids the precision loss a direct cast risks.
fn usize_to_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "logical pixel counts far below f32 precision"
)]
fn px_f32(value: f64) -> f32 {
    value as f32
}

/// One choice of a [`ToggleGroup`] or a [`SegmentedFace`].
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    /// What the segment says; for an icon segment, its accessible name.
    pub label: String,
    /// What the segment shows when its full label does not fit its share
    /// of the strip ("Mon" for "Monday").
    pub short: Option<String>,
    /// A glyph shown instead of the label ([`Segment::icon`]).
    pub icon: Option<Icon>,
    /// The segment's keyboard shortcut, named after the label in its
    /// tooltip.
    pub hint: Option<String>,
    /// Shown dimmed and never chosen: a choice that does not apply now
    /// (exporting the selection with nothing selected).
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

    /// A segment that shows `icon` (a list or grid view), named `name` for
    /// assistive technology.
    pub fn icon(icon: Icon, name: impl Into<String>) -> Self {
        Self {
            label: name.into(),
            short: None,
            icon: Some(icon),
            hint: None,
            disabled: false,
        }
    }

    /// Whether the segment can be chosen now; a disabled one shows dimmed
    /// and the pointer and keys pass over it.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.disabled = !enabled;
        self
    }

    /// Show `icon` instead of the label.
    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Name the segment's shortcut in its tooltip (`B`, `Shift+R`).
    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// The tooltip: the label, and the hint when there is one.
    fn tooltip(&self) -> String {
        match &self.hint {
            Some(hint) => format!("{}  {hint}", self.label),
            None => self.label.clone(),
        }
    }

    /// The label shown when the full one does not fit.
    #[must_use]
    pub fn short(mut self, label: impl Into<String>) -> Self {
        self.short = Some(label.into());
        self
    }
}

/// Two-way storage of the chosen segment as the caller's own value.
struct Choice {
    /// The index of the value the state holds now, when it changed.
    pull: Box<dyn FnMut() -> Option<usize>>,
    /// Store the value of a chosen segment.
    push: Box<dyn FnMut(usize)>,
}

/// What a strip reports to its owner.
enum ChangeCallback {
    One(Box<dyn FnMut(usize)>),
    Many(Box<dyn FnMut(&[bool])>),
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent cached interaction/layout flags are orthogonal states, not one state machine"
)]
pub struct ToggleGroup {
    id: Option<String>,
    segments: Vec<Segment>,
    /// The chosen segment, or [`NONE_CHOSEN`].
    selected: usize,
    /// The chosen segments of a multi-select strip.
    chosen: Option<Vec<bool>>,
    /// The segment the keyboard is on in a multi-select strip.
    cursor: usize,
    on_change: Option<ChangeCallback>,
    action: Option<ActionEmitter<usize>>,
    binding: Option<BoxedBinding<usize>>,
    native_resource: Option<crate::tilo_actions::Resource>,
    /// Indices aligned 1:1 with `segments`; drawing also consults this for
    /// the fill.
    segment_rects: Vec<Rect>,
    /// The whole widget: the caption (if any) above the strip.
    outer: Rect,
    /// The strip itself, below its caption.
    rect: Rect,
    /// A short label stacked above the strip.
    caption: Option<String>,
    /// The caption's height in the latest theme.
    caption_height: f64,
    /// A Fluent message id naming the strip for assistive technology.
    accessible_name: Option<String>,
    enabled: bool,
    /// Captured on `CursorMoved` so the subsequent `MouseInput` (which
    /// carries no position of its own) can hit-test against `segment_rects`.
    last_cursor: Cell<Point>,
    /// The segment under the pointer, for its hover lift.
    hovered: Option<usize>,
    cached: Scene,
    cache_dirty: bool,
    cache_rect: Rect,
    last_selected: usize,
    /// Drives the focus ring drawn round the track.
    is_focused: bool,
    /// `is_focused` at the last cache emit, so a focus change invalidates
    /// the cached scene.
    last_focused: bool,
    /// `hovered` at the last cache emit.
    last_hovered: Option<usize>,
    /// The theme colours `cached` was painted in, so a theme change
    /// repaints it.
    drawn_palette: Option<[Color; 4]>,
    /// Stretch to the parent's width instead of fitting the labels.
    fill: bool,
    /// What the strip is (see [`SegmentedStyle`]).
    style: SegmentedStyle,
    /// Height and label role.
    control_size: ControlSize,
    /// The geometry seen in `build_layout`, for `measure`.
    metrics: StripMetrics,
    /// Each segment's natural width with its full label and with its short
    /// form, measured with the text renderer; the strip is partitioned from
    /// them.
    naturals: Vec<(f64, f64)>,
    /// Per segment, whether the partition had to fall back to its short
    /// label.
    use_short: Vec<bool>,
    /// The width `measure` settled on, pinned onto the node so a stretching
    /// parent cannot widen a fitted strip.
    measured_width: Option<f32>,
    /// Rows of a wrapping multi-select strip at the latest measured width.
    rows: usize,
    /// How far a tab strip longer than its room is scrolled.
    scroll_x: f64,
    /// Finger owning the current gesture. Selection commits on release, so a
    /// scroll or cancelled touch never changes tabs on initial contact.
    touch: SegmentedTouch,
    /// Something that changes the measured size landed since the last layout.
    layout_dirty: bool,
    /// Focus came from a press: the ring is for keyboard focus, and returns
    /// once the keys are used.
    pressed_into_focus: bool,
    /// A strip, or a stack of square segments.
    orientation: Orientation,
    /// The side of a stacked segment and the gap between two, as of the
    /// last layout.
    stacked: (f64, f64),
    /// Where the chosen segment's value lives, when bound with
    /// [`Self::from_state`].
    choice: Option<Choice>,
}

impl ToggleGroup {
    pub fn new(segments: Vec<Segment>, selected: usize) -> Self {
        let len = segments.len();
        // Saturate rather than panic on an out-of-bounds index — keeps
        // construction infallible. `len.saturating_sub(1)` is also `0` when
        // `len == 0`, so this collapses to the same no-segments case.
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
            binding: None,
            native_resource: None,
            segment_rects: Vec::new(),
            outer: Rect::ZERO,
            rect: Rect::ZERO,
            caption: None,
            caption_height: 0.0,
            accessible_name: None,
            enabled: true,
            last_cursor: Cell::new(Point::ZERO),
            hovered: None,
            cached: Scene::new(),
            cache_dirty: true,
            cache_rect: Rect::ZERO,
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
        }
    }

    /// A choice among `choices`, kept in `state`: choosing a segment stores
    /// its value, and a value set anywhere else (a keyboard shortcut, the
    /// app) selects its segment.
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

    /// Stack the segments as square icon segments: a tool rail. There is
    /// no track; the chosen segment is a lift of the ground the rail stands
    /// on, as in [`SegmentedStyle::Quiet`].
    #[must_use]
    pub fn vertical(mut self) -> Self {
        self.orientation = Orientation::Vertical;
        self.style = SegmentedStyle::Quiet;
        self.cache_dirty = true;
        self
    }

    /// A strip with no segment chosen yet: the person has not picked one.
    pub fn unchosen(segments: Vec<Segment>) -> Self {
        Self::new(segments, NONE_CHOSEN)
    }

    /// A multi-select strip: every segment its own toggle (`chosen[i]`),
    /// each as wide as its label, wrapping onto more rows when narrow.
    /// Report changes with [`Self::on_chosen_change`].
    pub fn multiple(segments: Vec<Segment>, chosen: Vec<bool>) -> Self {
        let mut chosen = chosen;
        chosen.resize(segments.len(), false);
        let mut group = Self::new(segments, NONE_CHOSEN);
        group.chosen = Some(chosen);
        group
    }

    /// Stretch to the parent's width, segments sharing it equally. Without
    /// this the strip is as wide as its labels need.
    #[must_use]
    pub fn fill(mut self) -> Self {
        self.fill = true;
        self.layout_dirty = true;
        self
    }

    /// What the strip is: a segmented control (the default), page tabs, or
    /// a quiet refinement under a primary choice.
    #[must_use]
    pub fn with_style(mut self, style: SegmentedStyle) -> Self {
        self.style = style;
        self.cache_dirty = true;
        self.layout_dirty = true;
        self
    }

    /// Height and label role (see [`ControlSize`]).
    #[must_use]
    pub fn control_size(mut self, size: ControlSize) -> Self {
        self.control_size = size;
        self.metrics = StripMetrics::stock(size);
        self.cache_dirty = true;
        self.layout_dirty = true;
        self
    }

    /// A short label stacked above the strip ("Repeat", "Units") in the
    /// `Caption` role. It also names the strip to assistive technology
    /// unless an accessible name was given.
    #[must_use]
    pub fn caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self.layout_dirty = true;
        self
    }

    /// Name the strip for assistive technology (a Fluent message id); the
    /// chosen segment stays its value.
    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self
    }

    /// Set the enabled state at build time: a disabled strip is skipped by
    /// focus, takes no input and paints under the disabled alpha.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.set_enabled(enabled);
        self
    }

    /// Change the enabled state; see [`Self::with_enabled`].
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.cache_dirty = true;
            if !enabled {
                self.hovered = None;
            }
        }
    }

    /// Keep the chosen index in `state`: a choice the person makes updates
    /// it, and a change through any clone of it shows on the next sync.
    #[must_use]
    pub fn bind_selection(mut self, state: &crate::LocalState<usize>) -> Self {
        let (selected, binding) = super::binding::local(state);
        self.set_selected(selected);
        self.binding = Some(binding);
        self
    }

    #[must_use]
    pub fn on_change<F: FnMut(usize) + 'static>(mut self, f: F) -> Self {
        self.on_change = Some(ChangeCallback::One(Box::new(f)));
        self
    }

    /// Observe a multi-select strip's choices: called with every segment's
    /// state after the person toggles one.
    #[must_use]
    pub fn on_chosen_change<F: FnMut(&[bool]) + 'static>(mut self, f: F) -> Self {
        self.on_change = Some(ChangeCallback::Many(Box::new(f)));
        self
    }

    /// Route every pointer, keyboard, and accessibility selection through a
    /// caller-owned canonical action. The selected index is still painted
    /// optimistically only after the document/model reflects that action, so
    /// collaborative filters do not acquire a second callback-only path.
    #[must_use]
    pub fn on_action(mut self, binding: crate::tilo_actions::ActionBinding<usize>) -> Self {
        self.action = Some(ActionEmitter::new(binding));
        self.native_resource = None;
        self.on_change = None;
        self
    }

    /// Give this compound control a stable identity for headless driving,
    /// accessibility actions, and resource/action publication.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Publish segment selection as a transient view action while preserving
    /// the familiar closure API. UI, accessibility, search, and agents all
    /// enter through the same typed `index` operation.
    ///
    /// # Panics
    ///
    /// Panics only if the framework-generated integer action contract cannot
    /// encode its own generated payload.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "the generated integer schema and action are internally consistent"
    )]
    pub fn on_view<F: FnMut(usize) + 'static>(mut self, id: &str, f: F) -> Self {
        use crate::tilo_actions::{
            ActionBinding, ActionParam, ActionSpec, DiscoveryScope, EncodedAction, OperationKind,
            ParamType, Resource,
        };

        const TYPE_ID: &str = "tilo.ui.segmented.select";
        self.id = Some(id.to_owned());
        let control_id = id.to_owned();
        let mut spec = ActionSpec::typed_operation(
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
        spec.contract.discovery = DiscoveryScope::Launchable;
        let resource = Resource::new(format!("tilo.control:{id}"), "tilo.control.segmented", id)
            .with_action(spec.clone());
        let binding = ActionBinding::new(&resource, &spec, |index: usize| {
            serde_json::Map::from_iter([("index".into(), serde_json::json!(index))])
        })
        .expect("segment index matches the generated integer parameter");
        self.action = Some(ActionEmitter::new(binding));
        self.native_resource = Some(resource);
        self.on_change = Some(ChangeCallback::One(Box::new(f)));
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
        if let Some(ChangeCallback::One(callback)) = self.on_change.as_mut() {
            callback(index);
        }
    }

    /// Flip one segment of a multi-select strip and report the whole set.
    fn toggle_chosen(&mut self, index: usize) {
        let Some(chosen) = self.chosen.as_mut() else {
            return;
        };
        let Some(slot) = chosen.get_mut(index) else {
            return;
        };
        *slot = !*slot;
        self.cursor = index;
        self.cache_dirty = true;
        let snapshot = chosen.clone();
        if let Some(ChangeCallback::Many(callback)) = self.on_change.as_mut() {
            callback(&snapshot);
        }
    }

    fn segment_prefix(&self) -> String {
        self.id
            .as_ref()
            .map_or_else(|| "segment:".to_owned(), |id| format!("{id}:segment:"))
    }

    fn segment_id(&self, index: usize) -> String {
        format!("{}{index}", self.segment_prefix())
    }

    /// The chosen index, or [`NONE_CHOSEN`] before anything is chosen.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The chosen index, if the person has chosen one.
    pub fn selection(&self) -> Option<usize> {
        (self.selected < self.segments.len()).then_some(self.selected)
    }

    /// Every segment's state in a multi-select strip.
    pub fn chosen(&self) -> Option<&[bool]> {
        self.chosen.as_deref()
    }

    /// Set a multi-select strip's segments from the model. Does not fire
    /// the change callback.
    pub fn set_chosen(&mut self, chosen: &[bool]) {
        if let Some(current) = self.chosen.as_mut()
            && current.as_slice() != chosen
        {
            current.clear();
            current.extend_from_slice(chosen);
            current.resize(self.segments.len(), false);
            self.cache_dirty = true;
        }
    }

    /// Does **not** fire `on_change`, which is reserved for user-driven
    /// changes. [`NONE_CHOSEN`] clears the choice.
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

    /// Choose `selection`, or nothing. Does not fire `on_change`.
    pub fn set_selection(&mut self, selection: Option<usize>) {
        self.set_selected(selection.unwrap_or(NONE_CHOSEN));
    }

    /// Places the control at an absolute rect without going through taffy,
    /// for an app that hand-positions a shared control over its own chrome.
    /// The partition uses the metrics and label widths of the theme the
    /// strip last drew in; a paint in a new theme refreshes both and
    /// re-partitions before the next hit-test.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.place(rect);
    }

    /// Lay the widget out in `outer`: the caption on top, the strip below.
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

    /// Shared by the taffy [`Widget::apply_layout`] path and
    /// [`Self::apply_layout_external`] so both compute identical geometry.
    fn set_rect_and_partition(&mut self, rect: Rect) {
        self.rect = rect;
        let measured = self.naturals.len() == self.segments.len();
        let (rects, use_short) = if self.orientation == Orientation::Vertical {
            (
                stack(rect, self.segments.len()),
                vec![false; self.segments.len()],
            )
        } else if self.chosen.is_some() && measured {
            let (rects, rows) = flow(rect, &self.naturals, self.metrics.height, self.metrics.gap);
            self.rows = rows;
            (rects, vec![false; self.segments.len()])
        } else if self.style == SegmentedStyle::Tabs && measured {
            let content: f64 = self.naturals.iter().map(|(full, _)| full).sum();
            self.scroll_x = self.scroll_x.clamp(0.0, (content - rect.width()).max(0.0));
            (
                packed(rect, &self.naturals, self.scroll_x),
                vec![false; self.segments.len()],
            )
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
        #[allow(
            clippy::float_cmp,
            reason = "exact equality is intentional: both rects come from the same taffy \
                      layout pass, so a bit-identical width/height means the cached scene is \
                      still valid, not merely close enough"
        )]
        let size_changed = self.rect.width() != self.cache_rect.width()
            || self.rect.height() != self.cache_rect.height();
        if size_changed {
            self.cache_dirty = true;
        }
    }

    /// Scroll a tab strip longer than its room so segment `index` shows.
    fn reveal(&mut self, index: usize) {
        if self.style != SegmentedStyle::Tabs || self.naturals.len() != self.segments.len() {
            return;
        }
        let start: f64 = self.naturals.iter().take(index).map(|(full, _)| full).sum();
        let Some((width, _)) = self.naturals.get(index) else {
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

    /// Whether the tab strip is longer than its room.
    fn overflows(&self) -> bool {
        self.style == SegmentedStyle::Tabs
            && self.naturals.iter().map(|(full, _)| full).sum::<f64>() > self.rect.width() + 0.5
    }

    /// The number of separators the strip draws at rest: one between each
    /// pair of neighbouring segments that are both unchosen.
    pub fn separator_count(&self) -> usize {
        separators(self.segments.len(), self.selected, self.style).count()
    }

    /// Adopt the theme's strip geometry; returns whether it changed.
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

    /// Focus a person needs to be shown: keyboard focus, not the focus a
    /// press on a segment gave it.
    fn focus_visible(&self) -> bool {
        self.is_focused && !self.pressed_into_focus
    }

    fn strip_height(&self) -> f64 {
        if self.chosen.is_some() {
            let rows = usize_to_f64(self.rows.max(1));
            rows * self.metrics.height + (rows - 1.0) * self.metrics.gap
        } else {
            self.metrics.height
        }
    }
}

/// `rect` stacked into `n` square segments as wide as it is, the gaps
/// between them sharing the height left over.
fn stack(rect: Rect, n: usize) -> Vec<Rect> {
    let side = rect.width();
    let gaps = usize_to_f64(n.saturating_sub(1)).max(1.0);
    let gap = ((rect.height() - side * usize_to_f64(n)) / gaps).max(0.0);
    (0..n)
        .map(|i| {
            let y = rect.y0 + usize_to_f64(i) * (side + gap);
            Rect::new(rect.x0, y, rect.x1, y + side)
        })
        .collect()
}

/// `rect` split into `n` equal segments, the last one anchored to the right
/// edge so accumulated rounding leaves no hairline gap.
fn partition(rect: Rect, n: usize) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }
    let seg_w = rect.width() / usize_to_f64(n);
    let last = n.saturating_sub(1);
    (0..n)
        .map(|i| {
            let x0 = rect.x0 + usize_to_f64(i) * seg_w;
            let x1 = if i == last {
                rect.x1
            } else {
                rect.x0 + usize_to_f64(i.saturating_add(1)) * seg_w
            };
            Rect::new(x0, rect.y0, x1, rect.y1)
        })
        .collect()
}

/// Tabs packed from the leading edge, each as wide as its label needs,
/// shifted left by `scroll`.
fn packed(rect: Rect, naturals: &[(f64, f64)], scroll: f64) -> Vec<Rect> {
    let mut x = rect.x0 - scroll;
    naturals
        .iter()
        .map(|(full, _)| {
            let seg = Rect::new(x, rect.y0, x + full, rect.y1);
            x += full;
            seg
        })
        .collect()
}

/// Segments of a multi-select strip, each its natural width, `gap` apart,
/// wrapping onto rows `height` tall. Returns the rects and the row count.
fn flow(rect: Rect, naturals: &[(f64, f64)], height: f64, gap: f64) -> (Vec<Rect>, usize) {
    let mut rows = 1usize;
    let mut x = rect.x0;
    let mut y = rect.y0;
    let rects = naturals
        .iter()
        .map(|(full, _)| {
            if x > rect.x0 && x + full > rect.x1 + 0.5 {
                x = rect.x0;
                y += height + gap;
                rows = rows.saturating_add(1);
            }
            let seg = Rect::new(x, y, x + full, y + height);
            x += full + gap;
            seg
        })
        .collect();
    (rects, rows)
}

/// `rect` split among segments of natural widths `naturals` (full label,
/// short label): equal segments while every full label fits an equal share;
/// otherwise each segment as wide as its label needs with the room left
/// shared equally; only when the full labels cannot all fit, the short forms
/// where a segment has one; and only past that, shares in proportion (the
/// labels then elide). Returns the rects and, per segment, whether it shows
/// its short form.
fn partition_sized(rect: Rect, naturals: &[(f64, f64)]) -> (Vec<Rect>, Vec<bool>) {
    let n = naturals.len();
    if n == 0 {
        return (Vec::new(), Vec::new());
    }
    let avail = rect.width().max(0.0);
    let count = usize_to_f64(n);
    let full: Vec<f64> = naturals.iter().map(|(full, _)| *full).collect();
    let widest = full.iter().copied().fold(0.0, f64::max);
    if widest * count <= avail + 1e-6 {
        return (partition(rect, n), vec![false; n]);
    }
    let fitted = |widths: &[f64]| -> Option<Vec<f64>> {
        let sum: f64 = widths.iter().sum();
        (sum <= avail + 1e-6).then(|| {
            let extra = (avail - sum) / count;
            widths.iter().map(|w| w + extra).collect()
        })
    };
    let (widths, use_short) = if let Some(widths) = fitted(&full) {
        (widths, vec![false; n])
    } else {
        let short: Vec<f64> = naturals
            .iter()
            .map(|(full, short)| full.min(*short))
            .collect();
        let use_short: Vec<bool> = naturals.iter().map(|(full, short)| short < full).collect();
        if let Some(widths) = fitted(&short) {
            (widths, use_short)
        } else {
            let sum: f64 = short.iter().sum::<f64>().max(1.0);
            (short.iter().map(|w| w * avail / sum).collect(), use_short)
        }
    };
    let last = n.saturating_sub(1);
    let mut x = rect.x0;
    let rects = widths
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let x1 = if i == last { rect.x1 } else { x + w };
            let seg = Rect::new(x, rect.y0, x1, rect.y1);
            x = x1;
            seg
        })
        .collect();
    (rects, use_short)
}

/// The indices `i` whose right edge carries a separator: between two
/// unchosen neighbours on a segmented track, nowhere else.
fn separators(n: usize, selected: usize, style: SegmentedStyle) -> impl Iterator<Item = usize> {
    (0..n.saturating_sub(1)).filter(move |i| {
        style == SegmentedStyle::Segmented && *i != selected && i.saturating_add(1) != selected
    })
}

/// What a segmented strip is. The style never chooses a colour: the chosen
/// segment is always a lift with primary ink.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SegmentedStyle {
    /// A segmented control: a raised track with an edge, the chosen
    /// segment a thumb lifted off it, concentric inside it.
    #[default]
    Segmented,
    /// Page tabs: no track; the chosen tab is primary ink over a 2 px
    /// primary-ink underline. Tabs start flush at the strip's leading edge,
    /// each as wide as its label.
    Tabs,
    /// A quiet refinement under a primary choice (a sort order under a
    /// view switcher): no track; the chosen segment is a lift of the ground
    /// it stands on.
    Quiet,
}

/// The geometry of a strip of one [`ControlSize`] in one theme.
#[derive(Clone, Debug, PartialEq)]
struct StripMetrics {
    height: f64,
    /// Inset of the thumb inside the track.
    inset: f64,
    /// Label inset inside each segment.
    pad_x: f64,
    /// Between the toggles of a multi-select strip and its rows.
    gap: f64,
    /// The edge of a segment's icon: the label's line.
    icon: f64,
    track_radius: f64,
    thumb_radius: f64,
    label: TextStyle,
}

impl StripMetrics {
    fn of(size: ControlSize, theme: &Theme) -> Self {
        // The thumb is concentric with the track: its inset is the step
        // between the control and item radii.
        let radii = &theme.tokens.radii;
        let inset = (radii.control - radii.item).max(0.0);
        let label = size.label_style(theme);
        Self {
            height: size.height(theme),
            inset: f64::from(inset),
            pad_x: f64::from(theme.tokens.spacing.s3),
            gap: f64::from(theme.tokens.spacing.s2),
            icon: f64::from(label.line_height_px()).round(),
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

    /// One segment's natural width for `label`: the chosen (strong) label
    /// plus its padding, never narrower than the strip is tall. An icon
    /// segment is a square.
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

    /// A tab's natural width: its label and the padding between tabs; the
    /// first tab has none before it, so its label starts the strip.
    fn tab_natural(&self, label: &str, first: bool, text: &mut TextRenderer) -> f64 {
        let w = f64::from(
            text.measure_style(label, None, &self.label.clone().strong())
                .0,
        );
        let lead = if first { 0.0 } else { self.pad_x };
        (lead + w + self.pad_x).ceil()
    }

    /// Each segment's `(full, short)` natural widths in `style`.
    fn naturals(
        &self,
        segments: &[Segment],
        style: SegmentedStyle,
        text: &mut TextRenderer,
    ) -> Vec<(f64, f64)> {
        segments
            .iter()
            .enumerate()
            .map(|(i, segment)| {
                if style == SegmentedStyle::Tabs && segment.icon.is_none() {
                    let full = self.tab_natural(&segment.label, i == 0, text);
                    return (full, full);
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

    /// The narrowest strip that shows every full label: equal segments as
    /// wide as the widest label plus its padding; tabs packed.
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
        (widest * usize_to_f64(segments.len()) + 2.0 * self.inset).ceil()
    }
}

/// `(fill, label)` of one segment; `fill == None` means nothing is painted
/// under it. On a [`SegmentedStyle::Segmented`] track the fill is the thumb.
pub fn segment_colors(
    theme: &Theme,
    style: SegmentedStyle,
    selected: bool,
    hovered: bool,
) -> (Option<Color>, Color) {
    let colors = &theme.tokens.colors;
    let ground = match style {
        SegmentedStyle::Segmented => colors.surfaces.raised,
        SegmentedStyle::Tabs | SegmentedStyle::Quiet => colors.surfaces.content,
    };
    let tier = selection_tier_on(theme, SelectionState::new(selected, hovered, false), ground);
    let chosen = selection_tier_on(theme, SelectionState::active(true), ground).fill;
    let hover = selection_tier_on(theme, SelectionState::new(false, true, false), ground).fill;
    // Resolved over the ground and its hover lift at once, so an idle label
    // does not change colour under the pointer.
    let quiet_ink = colors.ink_on_any(&[ground, hover], colors.text_dim, ON_FILL_MIN_CONTRAST);
    match style {
        SegmentedStyle::Segmented | SegmentedStyle::Quiet => {
            let fill = (selected || hovered).then_some(tier.fill);
            let ink = if selected {
                colors.ink_on_any(&[chosen], colors.text_primary, ON_FILL_MIN_CONTRAST)
            } else {
                quiet_ink
            };
            (fill, ink)
        }
        SegmentedStyle::Tabs => {
            // An inactive tab's label holds its colour under the pointer;
            // the underline alone marks the chosen one.
            let ink = if selected {
                colors.ink_on_any(&[ground], colors.text_primary, ON_FILL_MIN_CONTRAST)
            } else {
                quiet_ink
            };
            (None, ink)
        }
    }
}

/// What one segment of a face shows.
#[derive(Clone, Copy, Debug)]
struct FaceItem<'a> {
    disabled: bool,
    label: &'a str,
    short: Option<&'a str>,
    icon: Option<Icon>,
}

/// An immediate-mode segmented strip over caller-owned segment rects (the
/// rects must be contiguous, except a multi-select strip's). The caller owns
/// the hit-test; this paints the themed look over them. Size the strip with
/// [`SegmentedFace::height`] and [`SegmentedFace::intrinsic_width`], and
/// split it with [`SegmentedFace::partition_segments`].
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
}

impl<'a> SegmentedFace<'a> {
    /// A strip of plain `labels`; `selected` may be [`NONE_CHOSEN`].
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
        }
    }

    /// A strip of [`Segment`]s: a segment whose label does not fit its rect
    /// shows its short form, an icon segment its icon.
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

    /// Choose `selection`, or nothing.
    #[must_use]
    pub fn selection(mut self, selection: Option<usize>) -> Self {
        self.selected = selection.unwrap_or(NONE_CHOSEN);
        self
    }

    /// A multi-select strip: each segment on where `chosen` says so, each
    /// its own toggle.
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
    /// Draw the focus ring, in your colour, round the strip.
    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }
    /// The strip takes no input: it paints under the disabled alpha, with
    /// no hover and no focus ring.
    #[must_use]
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    /// Label role; the host sizes the rects with [`Self::height`].
    #[must_use]
    pub fn control_size(mut self, size: ControlSize) -> Self {
        self.control_size = size;
        self
    }
    /// The height of a strip of `size` in `theme`.
    pub fn height(size: ControlSize, theme: &Theme) -> f64 {
        StripMetrics::of(size, theme).height
    }
    /// The narrowest strip that shows every full label in `theme`.
    pub fn intrinsic_width(
        labels: &[&str],
        size: ControlSize,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        let segments: Vec<Segment> = labels.iter().map(|label| Segment::new(*label)).collect();
        StripMetrics::of(size, theme).intrinsic_width(&segments, SegmentedStyle::Segmented, text)
    }
    /// The segment rects of a strip in `rect`: equal while every label fits
    /// an equal share, otherwise each as wide as its label needs with the
    /// rest shared, as a `ToggleGroup` partitions itself.
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
    /// The segment rects of a strip of [`Segment`]s in `rect`, as a
    /// `ToggleGroup` of `style` partitions itself: short forms where the full
    /// labels cannot fit, icon segments square, tabs packed from the leading
    /// edge.
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
    /// The track (and its separators) under a [`SegmentedStyle::Segmented`]
    /// strip, or the underline of [`SegmentedStyle::Tabs`].
    fn paint_ground(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
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
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    track,
                    None,
                    &RoundedRect::from_rect(outer, metrics.track_radius),
                );
                if !face.focused {
                    scene.stroke(
                        &Stroke::new(1.0),
                        transform,
                        edge,
                        None,
                        &crate::surface::edge_outline(outer, metrics.track_radius),
                    );
                }
                // Separators stop short of the edge so they never run into
                // the corner radius, and vanish beside the thumb.
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
                // A multi-select strip: every toggle its own small track.
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
            SegmentedStyle::Tabs => {
                if let (Some(seg), Some(item)) = (
                    face.segments.get(face.selected),
                    face.items.get(face.selected),
                ) {
                    // The underline runs under the label: flush with the
                    // strip's leading edge on the first tab.
                    let thickness = 2.0 * f64::from(theme.ui_scale);
                    let (x0, x1) =
                        tab_label_span(seg, face.selected == 0, item.label, metrics, text_renderer);
                    let bar = Rect::new(x0, seg.y1 - thickness, x1, seg.y1);
                    scene.fill(
                        vello::peniko::Fill::NonZero,
                        transform,
                        colors.text_primary,
                        None,
                        &RoundedRect::from_rect(bar, thickness / 2.0),
                    );
                }
            }
            SegmentedStyle::Quiet => {}
        }
    }

    /// For canvas apps that own their own hit-testing: call this to paint,
    /// keep your own hit-test to fire the selection.
    pub fn paint(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        face: &SegmentedFace<'_>,
    ) {
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
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                transform,
                &outer,
            );
        }
        let hovered = face.hovered.filter(|index| {
            !face.disabled && face.items.get(*index).is_some_and(|item| !item.disabled)
        });
        let is_on = |i: usize| match face.chosen {
            Some(chosen) => chosen.get(i).copied().unwrap_or(false),
            None => i == face.selected,
        };
        Self::paint_ground(
            scene,
            transform,
            text_renderer,
            theme,
            face,
            &metrics,
            outer,
        );

        // The thumb (and the hover lift) of every style but tabs: a lift set
        // concentrically inside the segment.
        if face.style != SegmentedStyle::Tabs {
            for (i, seg) in segs.iter().enumerate() {
                let selected = is_on(i);
                let hovered = hovered == Some(i) && !selected;
                let (Some(fill), _) = segment_colors(theme, face.style, selected, hovered) else {
                    continue;
                };
                let thumb = seg.inset(-metrics.inset);
                let shape = RoundedRect::from_rect(thumb, metrics.thumb_radius);
                scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
                let ground = if face.style == SegmentedStyle::Segmented {
                    theme.tokens.colors.surfaces.raised
                } else {
                    theme.tokens.colors.surfaces.content
                };
                let tier =
                    selection_tier_on(theme, SelectionState::new(selected, hovered, false), ground);
                if let Some(border) = tier.border {
                    scene.stroke(
                        &Stroke::new(1.0),
                        transform,
                        border,
                        None,
                        &crate::surface::edge_outline(thumb, metrics.thumb_radius),
                    );
                }
            }
        }

        // Segment labels are `Label`s; the chosen one is set `.strong()` so
        // it reads first. A label that does not fit falls back to its short
        // form, then elides. An icon segment shows its icon.
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
            if let Some(icon) = item.icon {
                let edge = metrics.icon.min(seg.height());
                crate::icons::draw_icon(
                    scene,
                    transform,
                    Rect::from_center_size(seg.center(), (edge, edge)),
                    icon,
                    ink,
                    crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
                continue;
            }
            if item.label.is_empty() {
                continue;
            }
            let style = if selected {
                &selected_style
            } else {
                &metrics.label
            };
            let tabs = face.style == SegmentedStyle::Tabs;
            let pad = metrics.pad_x.min(seg.width() / 4.0);
            let lead = if tabs && i == 0 { 0.0 } else { pad };
            let room = (seg.width() - lead - pad).max(1.0);
            let full_w = f64::from(text_renderer.measure_style(item.label, None, style).0);
            let label = match item.short {
                Some(short) if full_w > room + 0.5 => short,
                _ => item.label,
            };
            let shown = text_renderer.elide_to_width_style(label, px_f32(room + 0.5), style);
            let (tw, th) = text_renderer.measure_style(&shown, None, style);
            // Tabs set their labels from the leading edge; segments centre.
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
        // Keyboard focus is your colour round the whole strip; on tabs, which
        // may scroll past their room, round the chosen tab.
        if face.focused && !face.disabled {
            let ring = match face.style {
                SegmentedStyle::Tabs => segs.get(face.selected).copied().unwrap_or(outer),
                SegmentedStyle::Segmented | SegmentedStyle::Quiet => outer,
            };
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.tokens.colors.intent.you,
                None,
                &crate::surface::edge_outline(ring, metrics.track_radius),
            );
        }
    }

    /// The labels this strip shows: each full label, or its short form where
    /// the partition had to fall back to it; icon segments keep their icon.
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

    /// Measure every segment's natural widths; returns whether they changed.
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
        // The keys are in use: show where they act.
        self.pressed_into_focus = false;
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
            // Multi-select: arrows move between toggles, Space and Enter
            // flip the one the keyboard is on.
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
            // A segmented control is single-select: activation confirms its
            // current choice rather than toggling it off.
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

    /// One deliberate horizontal swipe advances a mode; vertical scrolling
    /// and sub-target drags do not change selection. The caller supplies its
    /// shared control target size rather than an app-specific threshold.
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
        let group = SegmentedTouchGroup::new(0, &self.segment_rects, self.selected);
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

    /// A wheel over a tab strip longer than its room scrolls it sideways.
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
        // A vertical wheel scrolls a horizontal strip too.
        let step = if dx.abs() > dy.abs() { dx } else { dy };
        self.scroll_x -= step;
        self.set_rect_and_partition(self.rect);
        self.cache_dirty = true;
        *handled = true;
    }
}

/// Where a tab's label runs in `seg`: from the leading edge on the first
/// tab, after the padding on the rest.
fn tab_label_span(
    seg: &Rect,
    first: bool,
    label: &str,
    metrics: &StripMetrics,
    text: &mut TextRenderer,
) -> (f64, f64) {
    let lead = if first {
        0.0
    } else {
        metrics.pad_x.min(seg.width() / 4.0)
    };
    let width = f64::from(
        text.measure_style(label, None, &metrics.label.clone().strong())
            .0,
    );
    let x0 = seg.x0 + lead;
    (x0, (x0 + width).min(seg.x1))
}

impl ToggleGroup {
    /// Apply a `tilo.ui.segmented.select` addressed to this control.
    /// `Ok(false)` when the operation is not this control's.
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

impl Widget for ToggleGroup {
    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::resources::PublishesResources
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
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        // An icon segment shows no label, so its label is its tooltip.
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
            // Square segments a pointer target high, one tight step apart.
            let side = self
                .metrics
                .height
                .max(f64::from(theme.tokens.metrics.min_pointer_target));
            let gap = f64::from(theme.tokens.spacing.s1);
            self.stacked = (side, gap);
            let n = usize_to_f64(self.segments.len().max(1));
            let node = taffy.leaf_node(Style {
                size: Size {
                    width: Dimension::length(px_f32(side)),
                    height: Dimension::length(px_f32(side * n + gap * (n - 1.0))),
                },
                flex_shrink: 0.0,
                ..Default::default()
            });
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            return node;
        }
        let height = self.strip_height() + self.caption_height;
        let style = Style {
            size: Size {
                // Fit the labels (measured in `measure`) unless asked to
                // fill; a multi-select strip and a tab strip take the row's
                // width and wrap or scroll inside it.
                width: if self.fill || self.chosen.is_some() {
                    Dimension::percent(1.0)
                } else {
                    Dimension::auto()
                },
                height: if self.chosen.is_some() {
                    Dimension::auto()
                } else {
                    Dimension::length(px_f32(height))
                },
            },
            max_size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            // A segmented strip has an intrinsic label-bearing width; a
            // parent without room caps it (`max_size`) and the labels fall
            // back to their short forms, then elide. A tab strip scrolls.
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
            let n = usize_to_f64(self.segments.len().max(1));
            return Size {
                width: known.width.unwrap_or(px_f32(side)),
                height: known.height.unwrap_or(px_f32(side * n + gap * (n - 1.0))),
            };
        }
        let intrinsic = px_f32(
            self.metrics
                .intrinsic_width(&self.segments, self.style, text),
        );
        self.measure_naturals(text);
        if self.chosen.is_some() {
            // Wrap the toggles at the width the row offers.
            let width = known.width.unwrap_or(match available.width {
                taffy::AvailableSpace::Definite(width) => width,
                _ => intrinsic,
            });
            let (_, rows) = flow(
                Rect::new(0.0, 0.0, f64::from(width), self.metrics.height),
                &self.naturals,
                self.metrics.height,
                self.metrics.gap,
            );
            self.rows = rows;
            return Size {
                width,
                height: px_f32(self.strip_height() + self.caption_height),
            };
        }
        if !self.fill {
            self.measured_width = Some(intrinsic);
        }
        Size {
            width: known.width.unwrap_or(intrinsic),
            height: known
                .height
                .unwrap_or(px_f32(self.strip_height() + self.caption_height)),
        }
    }

    fn adjust_layout_style(&mut self, taffy: &mut TaffyTree<()>, node: NodeId) -> bool {
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

    fn adjust_layout_with_measure(
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
            self.measured_width = Some(px_f32(self.metrics.intrinsic_width(
                &self.segments,
                self.style,
                text,
            )));
        }
        self.adjust_layout_style(taffy, node)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let x0 = origin.x + f64::from(layout.location.x);
        let y0 = origin.y + f64::from(layout.location.y);
        let x1 = x0 + f64::from(layout.size.width);
        let y1 = y0 + f64::from(layout.size.height);
        self.place(Rect::new(x0, y0, x1, y1));
        self.layout_dirty = false;
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
        if self.focus_visible() != self.last_focused {
            self.cache_dirty = true;
            self.last_focused = self.focus_visible();
        }
        if self.hovered != self.last_hovered {
            self.cache_dirty = true;
            self.last_hovered = self.hovered;
        }
        // The theme's geometry, refreshed on every paint so an externally
        // placed strip partitions in the theme it shows in.
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
            colors.intent.you,
        ];
        if self.drawn_palette != Some(palette) {
            self.drawn_palette = Some(palette);
            self.cache_dirty = true;
        }
        let needs_emit = self.cache_dirty || self.cache_rect == Rect::ZERO;
        if !needs_emit {
            let dx = self.rect.x0 - self.cache_rect.x0;
            let dy = self.rect.y0 - self.cache_rect.y0;
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine composition is float matrix multiplication; it cannot \
                          overflow or panic"
            )]
            let cached_transform = out_transform * Affine::translate((dx, dy));
            out_scene.append(&self.cached, Some(cached_transform));
            return;
        }
        // The partition follows the measured labels; a strip laid out before
        // its labels were measured re-partitions here, before it paints and
        // before the next hit-test.
        if self.measure_naturals(text_renderer) {
            self.place(self.outer);
        }
        self.cached.reset();
        if let Some(caption) = &self.caption {
            let style = theme.text(TextRole::Caption);
            let shown = text_renderer.elide_to_width_style(
                caption,
                px_f32(self.outer.width().max(1.0)),
                &style,
            );
            let at = Affine::translate((self.outer.x0, self.outer.y0));
            text_renderer.draw_style(&mut self.cached, &shown, theme.text_dim, at, None, &style);
        }
        let shown = self.shown_segments();
        let clip = self.overflows();
        if clip {
            self.cached.push_layer(
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
            .disabled(!self.is_enabled());
        if let Some(chosen) = &self.chosen {
            face = face.chosen(chosen);
        }
        Self::paint(
            &mut self.cached,
            Affine::IDENTITY,
            text_renderer,
            theme,
            &face,
        );
        if clip {
            self.cached.pop_layer();
        }
        // A multi-select strip rings the toggle the keyboard is on.
        if self.chosen.is_some()
            && self.is_focused
            && let Some(seg) = self.segment_rects.get(self.cursor)
        {
            self.cached.stroke(
                &Stroke::new(2.0),
                Affine::IDENTITY,
                theme.tokens.colors.intent.you,
                None,
                &crate::surface::edge_outline(*seg, self.metrics.track_radius),
            );
        }
        self.cache_rect = self.rect;
        out_scene.append(&self.cached, Some(out_transform));
    }

    fn needs_redraw(&self) -> bool {
        self.cache_dirty
            || self.selected != self.last_selected
            || self.focus_visible() != self.last_focused
            || self.hovered != self.last_hovered
    }

    fn mark_drawn(&mut self) {
        self.cache_dirty = false;
    }

    /// Fixed intrinsic size until a segment, the style, the size or the
    /// caption changes; `Rect::ZERO` covers the pre-`apply_layout` state.
    fn is_layout_dirty(&self) -> bool {
        self.cache_rect == Rect::ZERO || self.layout_dirty
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::KeyboardInput { event, .. } => self.handle_key_event(event, ctx.handled),
            WindowEvent::CursorMoved { position, .. } => {
                let logical_pos =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.last_cursor.set(logical_pos);
                self.hovered = if self.is_enabled() && self.rect.contains(logical_pos) {
                    self.segment_rects
                        .iter()
                        .position(|segment| segment.contains(logical_pos))
                } else {
                    None
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
        self.touch.is_active()
    }

    fn claims_raw_touch(&self, point: Point) -> bool {
        self.rect.contains(point)
    }

    fn get_rect(&self) -> Rect {
        if self.caption.is_some() {
            self.outer
        } else {
            self.rect
        }
    }

    fn tooltip_side(&self) -> crate::widget::tooltip::TooltipSide {
        // A vertical group is a rail: its neighbours are above and below.
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
        self.enabled && self.action.as_ref().is_none_or(ActionEmitter::is_available)
    }

    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

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

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Segment labels are runtime strings (host-provided), not FTL
        // keys, so no `i18n!` wrap.
        let value = if self.chosen.is_some() {
            None
        } else {
            self.segments.get(self.selected).map(|s| s.label.clone())
        };
        // Segments are drawn, not child widgets, so expose each as a
        // synthetic child so an AT enumerates the choices, not just the
        // active one.
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
                    .with_state(AccessState {
                        checked: Some(on),
                        disabled: !self.is_enabled() || s.disabled,
                        ..Default::default()
                    })
                    .with_position(i.saturating_add(1), count);
                // Where the segment is drawn, so pointing at it lands on it.
                match self.segment_rects.get(i) {
                    Some(rect) => node.with_bounds(*rect),
                    None => node,
                }
            })
            .collect();
        let name = self
            .accessible_name
            .as_ref()
            .map(|name| crate::i18n!(name))
            .or_else(|| self.caption.clone());
        Some(
            AccessNode::new(Role::Group)
                .with_optional_name(name)
                .with_optional_value(value)
                .with_state(AccessState {
                    focusable: self.is_focusable(),
                    focused: self.is_focused(),
                    disabled: !self.is_enabled(),
                    ..Default::default()
                })
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

impl crate::resources::PublishesResources for ToggleGroup {
    fn resources(&self, _doc: &AutoCommit) -> Vec<crate::tilo_actions::Resource> {
        self.native_resource.iter().cloned().collect()
    }
}

impl crate::widget::controls::ActionControls for ToggleGroup {
    crate::widget::controls::optional_emitter_controls!();
    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        let Some(action) = &self.action else {
            // A choice kept in view state is made through the segments'
            // own semantic actions.
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
mod tests {
    use super::*;
    use crate::presence::PresenceCtx;
    use crate::tokens::TextRole;
    use crate::widget::Widget;
    use std::cell::RefCell;
    use std::rc::Rc;
    use taffy::prelude::TaffyMaxContent;
    use vello_encoding::DrawTag;

    #[test]
    fn hosted_mode_swipes_commit_once_and_reject_scroll_cancel_and_multitouch() {
        use winit::event::TouchPhase::{Cancelled, Ended, Moved, Started};
        let selected = crate::LocalState::new(0_usize);
        let next = selected.clone();
        let commits = Rc::new(RefCell::new(Vec::new()));
        let receipts = Rc::clone(&commits);
        let mut frame = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            ToggleGroup::new(
                vec![
                    Segment::new("Photo"),
                    Segment::new("Video"),
                    Segment::new("Scan"),
                ],
                0,
            )
            .bind_selection(&selected)
            .on_view("camera-modes", move |index| {
                next.set(index);
                receipts.borrow_mut().push(index);
            }),
            (360, 100),
            Theme::dark(),
        );
        let rect = frame.root::<ToggleGroup>().get_rect();
        let start = rect.center();
        frame.touch(1, Started, start.x, start.y);
        frame.touch(1, Moved, start.x - 90.0, start.y);
        assert_eq!(selected.get(), 0, "motion alone cannot change mode");
        frame.touch(1, Ended, start.x - 90.0, start.y);
        assert_eq!(selected.get(), 1);
        frame.touch(1, Ended, start.x - 90.0, start.y);
        assert_eq!(selected.get(), 1, "a release commits once");
        frame.touch(2, Started, start.x, start.y);
        frame.touch(2, Ended, start.x - 20.0, start.y + 90.0);
        assert_eq!(selected.get(), 1, "vertical scrolling keeps the mode");
        frame.touch(3, Started, start.x, start.y);
        frame.touch(3, Cancelled, start.x - 90.0, start.y);
        frame.touch(3, Ended, start.x - 90.0, start.y);
        assert_eq!(selected.get(), 1, "cancelled swipes cannot commit");
        frame.touch(4, Started, start.x, start.y);
        frame.touch(5, Started, start.x, start.y);
        frame.touch(4, Ended, start.x - 90.0, start.y);
        frame.touch(5, Ended, start.x - 90.0, start.y);
        assert_eq!(selected.get(), 1, "multitouch cannot change modes");
        frame.touch(6, Started, start.x, start.y);
        frame.touch(6, Ended, start.x + 90.0, start.y);
        assert_eq!(selected.get(), 0, "right swipe selects the previous mode");
        assert_eq!(
            &*commits.borrow(),
            &[1, 0],
            "only two deliberate swipes invoke the real view operation"
        );
    }

    #[test]
    fn painted_strips_refuse_removed_changed_and_interrupted_touch_owners() {
        use winit::event::TouchPhase::{Cancelled, Ended, Started};
        let rects = [
            Rect::new(0.0, 0.0, 100.0, 48.0),
            Rect::new(100.0, 0.0, 200.0, 48.0),
        ];
        let group = SegmentedTouchGroup::new(42, &rects, 0);
        let mut recognizer = SegmentedTouch::default();
        let event = |id, phase, x, y| winit::event::Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: winit::dpi::PhysicalPosition::new(x, y),
            force: None,
            id,
        };
        assert_eq!(
            recognizer.handle(&event(1, Started, 180.0, 24.0), 1.0, &[group]),
            SegmentedTouchResult::Captured
        );
        assert_eq!(
            recognizer.handle(&event(1, Ended, 80.0, 24.0), 1.0, &[]),
            SegmentedTouchResult::Captured
        );
        assert!(!recognizer.is_active());
        recognizer.handle(&event(2, Started, 180.0, 24.0), 1.0, &[group]);
        let changed = SegmentedTouchGroup::new(42, &rects, 1);
        assert_eq!(
            recognizer.handle(&event(2, Ended, 80.0, 24.0), 1.0, &[changed]),
            SegmentedTouchResult::Captured
        );
        recognizer.handle(&event(3, Started, 180.0, 24.0), 1.0, &[group]);
        recognizer.handle(&event(4, Started, 180.0, 24.0), 1.0, &[group]);
        recognizer.handle(&event(3, Ended, 80.0, 24.0), 1.0, &[group]);
        recognizer.handle(&event(5, Started, 180.0, 24.0), 1.0, &[group]);
        recognizer.handle(&event(4, Cancelled, 80.0, 24.0), 1.0, &[group]);
        assert_eq!(
            recognizer.handle(&event(5, Ended, 80.0, 24.0), 1.0, &[group]),
            SegmentedTouchResult::Captured
        );
        assert!(!recognizer.is_active());
        recognizer.handle(&event(6, Started, 180.0, 24.0), 1.0, &[group]);
        assert_eq!(
            recognizer.handle(&event(6, Ended, 80.0, 24.0), 1.0, &[group]),
            SegmentedTouchResult::Selected {
                group: 42,
                index: 1
            }
        );
    }

    /// Fixed rect so tests can simulate clicks without a full taffy pass.
    fn group_at(segments: Vec<Segment>, selected: usize, rect: Rect) -> ToggleGroup {
        let mut g = ToggleGroup::new(segments, selected);
        g.rect = rect;
        // Populate segment_rects manually (mirrors apply_layout).
        let n = g.segments.len();
        if n > 0 {
            let w = rect.width() / usize_to_f64(n);
            let last = n.saturating_sub(1);
            g.segment_rects = (0..n)
                .map(|i| {
                    let x0 = rect.x0 + usize_to_f64(i) * w;
                    let x1 = if i == last {
                        rect.x1
                    } else {
                        rect.x0 + usize_to_f64(i.saturating_add(1)) * w
                    };
                    Rect::new(x0, rect.y0, x1, rect.y1)
                })
                .collect();
        }
        g
    }

    use crate::synthetic_input::cursor_moved;

    use crate::synthetic_input::mouse_press;

    use crate::synthetic_input::named_key;

    fn touch(phase: winit::event::TouchPhase, point: Point) -> WindowEvent {
        WindowEvent::Touch(winit::event::Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: winit::dpi::PhysicalPosition::new(point.x, point.y),
            force: None,
            id: 7,
        })
    }
    fn dispatch(g: &mut ToggleGroup, event: &WindowEvent) {
        let _ = dispatch_handled(g, event);
    }

    fn dispatch_handled(g: &mut ToggleGroup, event: &WindowEvent) -> bool {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        g.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: crate::widget::Edits::lend(&mut doc),
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &tree,
            },
        );
        handled
    }

    #[test]
    fn a_vertical_rail_stacks_square_segments_bound_to_view_state() {
        let state = crate::LocalState::new('b');
        let mut rail = ToggleGroup::from_state(
            vec![
                (
                    'a',
                    Segment::new("Select")
                        .with_icon(Icon::MousePointer2)
                        .with_hint("V"),
                ),
                (
                    'b',
                    Segment::new("Brush").with_icon(Icon::Brush).with_hint("B"),
                ),
                ('c', Segment::icon(Icon::Type, "Text")),
            ],
            &state,
        )
        .vertical();
        assert_eq!(
            rail.selected(),
            1,
            "the state's value is the chosen segment"
        );
        rail.apply_layout_external(Rect::new(0.0, 0.0, 40.0, 128.0));
        let rects = rail.segment_rects.clone();
        assert_eq!(rects.len(), 3);
        assert!(
            rects
                .iter()
                .all(|rect| (rect.width() - rect.height()).abs() < 1e-9)
        );
        assert!(
            (rects[2].y1 - 128.0).abs() < 1e-9,
            "the gaps share the rest"
        );

        // A press chooses the segment and stores its value.
        dispatch(&mut rail, &cursor_moved(20.0, rects[2].center().y));
        dispatch(&mut rail, &mouse_press());
        assert_eq!(state.with(|value| *value), 'c');

        // A value set elsewhere (a keyboard shortcut) is shown on sync.
        state.set('a');
        let doc = AutoCommit::new();
        Widget::sync(&mut rail, &doc, &automerge::ROOT);
        assert_eq!(rail.selected(), 0);

        // An icon segment's label and shortcut are its tooltip.
        let (_, tip) = rail
            .tooltip_target_at(rects[0].center())
            .expect("an icon segment has a tooltip");
        assert_eq!(tip, "Select  V");
    }

    #[test]
    fn a_press_focuses_without_a_ring_until_the_keys_are_used() {
        let mut g = group_at(
            vec![Segment::new("A"), Segment::new("B")],
            0,
            Rect::new(0.0, 0.0, 200.0, 32.0),
        );
        dispatch(&mut g, &cursor_moved(150.0, 16.0));
        dispatch(&mut g, &mouse_press());
        g.set_focus(true);
        assert!(!g.focus_visible(), "a click shows no focus ring");
        dispatch(&mut g, &named_key(NamedKey::ArrowLeft));
        assert!(g.focus_visible(), "the keys bring the ring back");
        g.set_focus(false);
        g.set_focus(true);
        assert!(g.focus_visible(), "keyboard focus shows the ring");
    }

    #[test]
    fn toggle_group_geometry_scales_labels_once_and_is_touch_sized() {
        // Case `retained_labels_apply_resolved_text_scale_exactly_once`.
        {
            for scale in [1.0, 2.0] {
                let theme = Theme::dark().with_text_scale(scale);
                let mut text = crate::font_catalog::fixture_renderer();
                text.set_text_scale(scale);
                let mut group = group_at(
                    vec![Segment::new("Params"), Segment::new("Headers")],
                    0,
                    Rect::new(0.0, 0.0, 384.0, 52.0),
                );
                let mut scene = Scene::new();
                group.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                    &PresenceCtx::default(),
                );
                let runs = &scene.encoding().resources.glyph_runs;
                assert!(!runs.is_empty());
                for run in runs {
                    assert!(
                        (run.font_size - theme.tokens.typography.size(TextRole::Label)).abs()
                            < 0.001,
                        "{scale}x text must paint once at resolved size {}, got {}",
                        theme.tokens.typography.size(TextRole::Label),
                        run.font_size
                    );
                }
            }
        }

        // Case `retained_group_fits_its_labels_at_its_control_height`.
        {
            let harness = crate::headless::FrameHarness::new(
                crate::State::new(),
                Box::new(
                    crate::widget::Column::new()
                        .with_no_padding()
                        .push(Box::new(ToggleGroup::new(
                            vec![Segment::new("World"), Segment::new("Timers")],
                            0,
                        ))),
                ),
                (600, 200),
            );
            let rect = harness.inspection().root.children[0].rect;
            assert_eq!(rect[3], ControlSize::Regular.height(&Theme::dark()));
            assert!(
                rect[2] < 300.0,
                "a fitted strip does not stretch to its parent: {rect:?}"
            );
            let mut text = TextRenderer::new();
            let widest = text
                .measure_style(
                    "Timers",
                    None,
                    &Theme::dark().text(TextRole::Label).strong(),
                )
                .0;
            assert!(
                rect[2] / 2.0
                    >= f64::from(widest) + 2.0 * f64::from(Theme::dark().tokens.spacing.s3),
                "each segment holds the widest label: {rect:?}"
            );
            let harness = crate::headless::FrameHarness::new(
                crate::State::new(),
                Box::new(
                    crate::widget::Column::new()
                        .with_no_padding()
                        .push(Box::new(
                            ToggleGroup::new(
                                vec![Segment::new("World"), Segment::new("Timers")],
                                0,
                            )
                            .fill(),
                        )),
                ),
                (600, 200),
            );
            assert_eq!(harness.inspection().root.children[0].rect[2], 600.0);
        }

        // Case `toggle_group_paints_separator_lines_between_segments`.
        {
            let mut g = ToggleGroup::new(
                vec![Segment::new("a"), Segment::new("b"), Segment::new("c")],
                0,
            );
            // Run a layout pass so `segment_rects` is populated.
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = g.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
            let root = taffy.parent_node(Style::default(), &[node]);
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            g.apply_layout(&taffy, node, Point::ZERO);

            assert_eq!(
                g.separator_count(),
                1,
                "a separator stands only between two unchosen neighbours"
            );

            // `>=` since fills/border/text also contribute `DrawTag::COLOR`
            // entries; the floor proves the separator strokes were emitted.
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let presence = PresenceCtx::default();
            let mut scene = Scene::new();
            g.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

            let colour_tags = scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|t| **t == DrawTag::COLOR)
                .count();

            assert!(
                colour_tags >= 2,
                "3-segment ToggleGroup must emit >= 2 colour draw-tags for separator lines (got {colour_tags})"
            );
        }

        // Case `segmented_paint_emits_fills_over_caller_rects`.
        {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let segs = [
                Rect::new(0.0, 0.0, 60.0, 28.0),
                Rect::new(60.0, 0.0, 120.0, 28.0),
                Rect::new(120.0, 0.0, 180.0, 28.0),
            ];
            let labels = ["A", "B", "C"];
            for style in [
                SegmentedStyle::Segmented,
                SegmentedStyle::Tabs,
                SegmentedStyle::Quiet,
            ] {
                let mut scene = Scene::new();
                ToggleGroup::paint(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut tr,
                    &theme,
                    &SegmentedFace::new(&segs, &labels, 1).style(style),
                );
                let colour_tags = scene
                    .encoding()
                    .draw_tags
                    .iter()
                    .filter(|t| **t == DrawTag::COLOR)
                    .count();
                assert!(colour_tags > 0, "{style:?} paint must emit draw commands");
            }
        }

        // Case `a_label_that_does_not_fit_falls_back_to_its_short_form`.
        {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let mut group = group_at(
                vec![
                    Segment::new("Monday").short("Mon"),
                    Segment::new("Tuesday").short("Tue"),
                ],
                0,
                Rect::new(0.0, 0.0, 90.0, 32.0),
            );
            group.metrics = StripMetrics::of(ControlSize::Regular, &theme);
            group.measure_naturals(&mut text);
            group.set_rect_and_partition(Rect::new(0.0, 0.0, 90.0, 32.0));
            assert_eq!(group.shown_labels(), ["Mon", "Tue"]);
            group.set_rect_and_partition(Rect::new(0.0, 0.0, 400.0, 32.0));
            assert_eq!(group.shown_labels(), ["Monday", "Tuesday"]);
        }

        // Case `segments_size_to_their_labels_before_any_label_shortens`.
        // "Day / Week / Year" in a strip too narrow for three equal "Week"
        // segments but wide enough for the three labels: no segment elides,
        // the wider label gets the wider segment.
        {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let mut group = group_at(
                vec![
                    Segment::new("Day"),
                    Segment::new("Week"),
                    Segment::new("Year"),
                ],
                1,
                Rect::new(0.0, 0.0, 400.0, 32.0),
            );
            group.metrics = StripMetrics::of(ControlSize::Regular, &theme);
            group.measure_naturals(&mut text);
            let naturals: Vec<f64> = group.naturals.iter().map(|(full, _)| *full).collect();
            let widest = naturals.iter().copied().fold(0.0, f64::max);
            let sum: f64 = naturals.iter().sum();
            assert!(
                sum < widest * 3.0,
                "the labels differ in width: {naturals:?}"
            );

            // Room for three equal widest segments: equal widths.
            group.set_rect_and_partition(Rect::new(0.0, 0.0, (widest * 3.0).ceil(), 32.0));
            let widths: Vec<f64> = group.segment_rects.iter().map(Rect::width).collect();
            assert!(
                widths.iter().all(|w| (w - widths[0]).abs() < 1e-6),
                "{widths:?}"
            );

            // Less than that but room for every label: sized to the labels.
            let tight = (sum + widest * 3.0) / 2.0;
            group.set_rect_and_partition(Rect::new(0.0, 0.0, tight, 32.0));
            for (rect, natural) in group.segment_rects.iter().zip(&naturals) {
                assert!(rect.width() + 1e-6 >= *natural, "{rect:?} holds {natural}");
            }
            assert_eq!(group.shown_labels(), ["Day", "Week", "Year"]);
            assert!(
                (group.segment_rects.last().unwrap().x1 - tight).abs() < 1e-6,
                "the strip is filled exactly"
            );
        }
    }

    #[test]
    fn a_disabled_segment_is_never_chosen_and_the_keys_step_over_it() {
        let mut g = group_at(
            vec![
                Segment::new("Whole"),
                Segment::new("Selection").with_enabled(false),
                Segment::new("View"),
            ],
            0,
            Rect::new(0.0, 0.0, 300.0, 36.0),
        );
        dispatch(&mut g, &cursor_moved(150.0, 18.0));
        dispatch(&mut g, &mouse_press());
        assert_eq!(g.selected(), 0, "a click on it chooses nothing");
        g.set_focus(true);
        assert!(dispatch_handled(&mut g, &named_key(NamedKey::ArrowRight)));
        assert_eq!(g.selected(), 2, "the arrow steps over it");
        assert!(dispatch_handled(&mut g, &named_key(NamedKey::ArrowLeft)));
        assert_eq!(g.selected(), 0);
        let node = g.accessibility().expect("a group node");
        assert!(
            node.children
                .get(1)
                .is_some_and(|segment| segment.state.disabled)
        );
    }

    #[test]
    fn toggle_group_selection_follows_pointer_touch_and_keyboard() {
        // Case `toggle_group_initial_selection_zero`.
        {
            let g = ToggleGroup::new(vec![Segment::new("a"), Segment::new("b")], 0);
            assert_eq!(g.selected(), 0);
        }

        // Case `toggle_group_click_segment_two_selects_index_two`.
        {
            let captured: Rc<RefCell<Option<usize>>> = Rc::new(RefCell::new(None));
            let captured_clone = captured.clone();
            let mut g = group_at(
                vec![Segment::new("a"), Segment::new("b"), Segment::new("c")],
                0,
                Rect::new(0.0, 0.0, 300.0, 36.0),
            )
            .on_change(move |i| {
                *captured_clone.borrow_mut() = Some(i);
            });

            dispatch(&mut g, &cursor_moved(250.0, 18.0));
            dispatch(&mut g, &mouse_press());

            assert_eq!(g.selected(), 2, "click in segment 2 must select index 2");
            assert_eq!(
                *captured.borrow(),
                Some(2),
                "on_change must fire with the new index"
            );
        }

        // Case `focused_toggle_group_supports_keyboard_navigation_and_activation`.
        {
            let changes = Rc::new(RefCell::new(Vec::new()));
            let captured = Rc::clone(&changes);
            let mut group = ToggleGroup::new(
                vec![
                    Segment::new("One"),
                    Segment::new("Two"),
                    Segment::new("Three"),
                ],
                0,
            )
            .on_change(move |index| captured.borrow_mut().push(index));
            group.set_focus(true);

            assert!(dispatch_handled(
                &mut group,
                &named_key(NamedKey::ArrowRight)
            ));
            assert_eq!(group.selected(), 1);
            assert!(dispatch_handled(&mut group, &named_key(NamedKey::End)));
            assert_eq!(group.selected(), 2);
            assert!(dispatch_handled(
                &mut group,
                &named_key(NamedKey::ArrowRight)
            ));
            assert_eq!(group.selected(), 0, "arrow navigation wraps");
            assert!(dispatch_handled(&mut group, &named_key(NamedKey::Enter)));
            assert_eq!(&*changes.borrow(), &[1, 2, 0]);

            group.set_focus(false);
            assert!(!dispatch_handled(
                &mut group,
                &named_key(NamedKey::ArrowRight)
            ));
            assert_eq!(
                group.selected(),
                0,
                "unfocused controls do not consume keys"
            );
        }

        // Case `touch_commits_on_release_and_keyboard_arrows_wrap`.
        {
            let mut group = group_at(
                vec![
                    Segment::new("World"),
                    Segment::new("Alarms"),
                    Segment::new("Timers"),
                ],
                0,
                Rect::new(0.0, 0.0, 300.0, 44.0),
            );
            dispatch(
                &mut group,
                &touch(winit::event::TouchPhase::Started, Point::new(250.0, 22.0)),
            );
            assert_eq!(group.selected(), 0, "touch down does not switch a tab");
            dispatch(
                &mut group,
                &touch(winit::event::TouchPhase::Ended, Point::new(250.0, 22.0)),
            );
            assert_eq!(group.selected(), 2);

            group.set_focus(true);
            dispatch(&mut group, &named_key(NamedKey::ArrowRight));
            assert_eq!(group.selected(), 0, "right arrow wraps to the first tab");
            dispatch(&mut group, &named_key(NamedKey::End));
            assert_eq!(group.selected(), 2);
        }

        // Case `unavailable_toggle_group_rejects_pointer_touch_and_keyboard_input`.
        {
            let mut group = group_at(
                vec![Segment::new("Light"), Segment::new("Dark")],
                0,
                Rect::new(0.0, 0.0, 200.0, 44.0),
            )
            .on_view("appearance", |_| {});
            crate::widget::controls::refresh_control_action_availability(
                &mut group,
                &std::collections::HashSet::new(),
            );
            group.set_focus(true);
            assert!(!Widget::is_enabled(&group));

            dispatch(&mut group, &cursor_moved(150.0, 22.0));
            assert!(!dispatch_handled(&mut group, &mouse_press()));
            assert!(!dispatch_handled(
                &mut group,
                &touch(winit::event::TouchPhase::Started, Point::new(150.0, 22.0)),
            ));
            assert!(!dispatch_handled(
                &mut group,
                &named_key(NamedKey::ArrowRight),
            ));
            assert_eq!(group.selected(), 0);
        }
    }

    /// A focus change swaps the outer border colour, so the encoded stream
    /// differs between focused and unfocused renders.
    fn group_draw_data(focused: bool, theme: &Theme) -> Vec<u32> {
        let mut g = ToggleGroup::new(vec![Segment::new("a"), Segment::new("b")], 0);
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = g.build_layout(&mut taffy, theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, node, Point::ZERO);
        g.set_focus(focused);

        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        g.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
        scene.encoding().draw_data.clone()
    }

    #[test]
    fn segment_colours_follow_selection_focus_and_style() {
        // Case `focused_toggle_group_uses_the_focus_colour`.
        // Uses high contrast (yellow focus vs white border) so the two colours
        // are unambiguously different.
        {
            let theme = Theme::high_contrast();
            let unfocused = group_draw_data(false, &theme);
            let focused = group_draw_data(true, &theme);
            assert_ne!(
                unfocused, focused,
                "focusing the group must change the border colour in the encoded scene",
            );
        }

        // Case `the_chosen_segment_is_a_colourless_lift_with_readable_ink`.
        {
            use crate::a11y::contrast_ratio;
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let colors = &theme.tokens.colors;
                for (style, ground) in [
                    (SegmentedStyle::Segmented, colors.surfaces.raised),
                    (SegmentedStyle::Quiet, colors.surfaces.content),
                ] {
                    let (fill, ink) = segment_colors(&theme, style, true, false);
                    let fill = fill.expect("the chosen segment is lifted");
                    assert_eq!(
                        fill,
                        selection_tier_on(&theme, SelectionState::active(true), ground).fill,
                        "{} {style:?}: the thumb is the selection tier of its ground",
                        theme.name
                    );
                    let chroma = crate::color::Oklch::from_color(fill).c;
                    assert!(
                        chroma <= 0.02,
                        "{} {style:?}: the chosen segment carries no hue ({chroma})",
                        theme.name
                    );
                    assert!(contrast_ratio(ink, fill) >= ON_FILL_MIN_CONTRAST);
                    let (idle, idle_ink) = segment_colors(&theme, style, false, false);
                    assert_eq!(idle, None, "{}: an idle segment paints nothing", theme.name);
                    assert!(
                        contrast_ratio(idle_ink, ground)
                            < contrast_ratio(colors.text_primary, ground),
                        "{} {style:?}: an idle label is quieter than the chosen one",
                        theme.name
                    );
                    let (hover, hover_ink) = segment_colors(&theme, style, false, true);
                    assert!(hover.is_some(), "a hovered segment lifts halfway");
                    assert_eq!(hover_ink, idle_ink, "the label holds its colour on hover");
                }
                let (fill, ink) = segment_colors(&theme, SegmentedStyle::Tabs, true, false);
                assert_eq!(fill, None, "a tab is marked by its underline");
                assert!(contrast_ratio(ink, colors.surfaces.content) >= ON_FILL_MIN_CONTRAST);
            }
        }
    }

    #[test]
    fn accessibility_and_actions_expose_and_drive_the_selected_segment() {
        // Case `accessibility_reports_group_role_and_selected_segment_label`.
        {
            use crate::a11y::Role;
            let g = ToggleGroup::new(
                vec![
                    Segment::new("Light"),
                    Segment::new("Dark"),
                    Segment::new("Auto"),
                ],
                1,
            );
            let node = Widget::accessibility(&g).expect("toggle group is not presentational");
            assert_eq!(node.role, Role::Group);
            assert_eq!(node.name, None, "no intrinsic label on the group");
            assert_eq!(
                node.value.as_deref(),
                Some("Dark"),
                "value is the selected segment's label",
            );
            assert!(node.state.focusable);
            assert!(!node.state.disabled);
        }

        // Case `accessibility_value_tracks_selection_changes`.
        {
            // Nothing reads `captured` back in this test (only the accessibility
            // value is asserted), so the closure can just own it — no need for
            // the clone-and-keep-a-handle dance the other click test uses.
            let captured: Rc<RefCell<Option<usize>>> = Rc::new(RefCell::new(None));
            let mut g = group_at(
                vec![Segment::new("a"), Segment::new("b"), Segment::new("c")],
                0,
                Rect::new(0.0, 0.0, 300.0, 36.0),
            )
            .on_change(move |i| *captured.borrow_mut() = Some(i));

            assert_eq!(
                Widget::accessibility(&g).unwrap().value.as_deref(),
                Some("a")
            );
            let segments = Widget::accessibility(&g).unwrap().children;
            assert_eq!(
                segments.get(2).and_then(|segment| segment.bounds),
                g.segment_rects.get(2).map(|rect| (*rect).into()),
                "each segment is placed where it is drawn"
            );

            // Click into segment 2 → value follows the new selection.
            dispatch(&mut g, &cursor_moved(250.0, 18.0));
            dispatch(&mut g, &mouse_press());
            assert_eq!(g.selected(), 2);
            assert_eq!(
                Widget::accessibility(&g).unwrap().value.as_deref(),
                Some("c")
            );
        }

        // Case `synthetic_segment_activation_selects_and_fires_on_change`.
        {
            let changed = Rc::new(RefCell::new(Vec::new()));
            let captured = Rc::clone(&changed);
            let mut group = ToggleGroup::new(vec![Segment::new("Light"), Segment::new("Dark")], 0)
                .on_change(move |index| captured.borrow_mut().push(index));

            assert!(Widget::perform_semantic_action(
                &mut group,
                "segment:1",
                &crate::a11y::Action::Activate,
                &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert_eq!(group.selected(), 1);
            assert_eq!(&*changed.borrow(), &[1]);
        }

        // Case `on_view_publishes_and_executes_one_typed_resource_action`.
        {
            let changed = Rc::new(RefCell::new(Vec::new()));
            let captured = Rc::clone(&changed);
            let mut group = ToggleGroup::new(vec![Segment::new("Light"), Segment::new("Dark")], 0)
                .on_view("appearance", move |index| captured.borrow_mut().push(index));

            let resources = crate::resources::resources(&group, &AutoCommit::new());
            assert_eq!(resources.len(), 1);
            let resource = &resources[0];
            assert_eq!(resource.id.as_str(), "tilo.control:appearance");
            assert_eq!(resource.actions.len(), 1);
            let spec = &resource.actions[0];
            assert_eq!(spec.kind, crate::tilo_actions::OperationKind::View);
            assert_eq!(
                spec.contract.discovery,
                crate::tilo_actions::DiscoveryScope::Launchable
            );
            assert_eq!(spec.params()[0].name, "index");

            assert!(Widget::perform_semantic_action(
                &mut group,
                "appearance:segment:1",
                &crate::a11y::Action::Activate,
                &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert_eq!(
                group.selected(),
                0,
                "UI intent does not bypass the action host"
            );
            assert!(changed.borrow().is_empty());

            let invocation = crate::widget::controls::take_control_invocation(&mut group).unwrap();
            assert_eq!(
                invocation.address,
                crate::tilo_actions::ActionAddress::new("tilo.control:appearance", "select")
            );
            assert_eq!(invocation.arguments["index"], 1);
            let encoded = spec.build_json(&invocation.arguments).unwrap();
            assert_eq!(encoded.type_id, "tilo.ui.segmented.select");
            let output = Widget::start_operation(
                &mut group,
                &crate::widget::ResourceOperation::local(
                    crate::tilo_actions::InvocationId::new(),
                    crate::tilo_actions::OperationKind::View,
                    &encoded.type_id,
                    &encoded.payload,
                ),
                &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            )
            .map(|started| started.and_then(crate::widget::ResourceOperationStart::completed))
            .unwrap();
            assert!(output.is_some());
            assert_eq!(group.selected(), 1);
            assert_eq!(&*changed.borrow(), &[1]);
        }
    }

    #[test]
    fn strips_choose_nothing_many_or_icons_and_tabs_start_flush() {
        fn nothing_chosen_until_the_person_picks() {
            let mut g = group_at(
                vec![Segment::new("Low"), Segment::new("High")],
                NONE_CHOSEN,
                Rect::new(0.0, 0.0, 200.0, 32.0),
            );
            assert_eq!(g.selection(), None);
            assert_eq!(g.separator_count(), 1);
            let node = g.accessibility().expect("semantics");
            assert_eq!(node.value, None);
            g.set_focus(true);
            dispatch(&mut g, &named_key(NamedKey::ArrowRight));
            assert_eq!(g.selection(), Some(0), "the first arrow chooses the first");
            g.set_selection(None);
            assert_eq!(g.selected(), NONE_CHOSEN);
        }

        fn a_bound_selection_follows_its_state_both_ways() {
            let state = crate::LocalState::new(1usize);
            let mut g = ToggleGroup::new(vec![Segment::new("A"), Segment::new("B")], 0)
                .bind_selection(&state);
            assert_eq!(g.selected(), 1);
            g.rect = Rect::new(0.0, 0.0, 200.0, 32.0);
            g.segment_rects = partition(g.rect, 2);
            dispatch(&mut g, &cursor_moved(20.0, 16.0));
            dispatch(&mut g, &mouse_press());
            assert_eq!(state.get(), 0);
            state.set(1);
            g.sync(&AutoCommit::new(), &automerge::ROOT);
            assert_eq!(g.selected(), 1);
        }

        fn many_toggle_on_their_own_and_wrap() {
            let seen = Rc::new(RefCell::new(Vec::<Vec<bool>>::new()));
            let sink = seen.clone();
            let days = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
            let mut g = ToggleGroup::multiple(
                days.iter().map(|d| Segment::new(*d)).collect(),
                vec![true, false, false, false, false, false, false],
            )
            .on_chosen_change(move |chosen| sink.borrow_mut().push(chosen.to_vec()));
            let mut text = TextRenderer::new();
            g.measure_naturals(&mut text);
            g.apply_layout_external(Rect::new(0.0, 0.0, 160.0, 200.0));
            assert!(g.rows > 1, "seven toggles wrap in 160 px: {} rows", g.rows);
            let second = g.segment_rects[1];
            dispatch(&mut g, &cursor_moved(second.center().x, second.center().y));
            dispatch(&mut g, &mouse_press());
            assert_eq!(
                seen.borrow().last().map(|c| c[..2].to_vec()),
                Some(vec![true, true])
            );
            let node = g.accessibility().expect("semantics");
            assert_eq!(node.children[1].state.checked, Some(true));
        }

        fn icon_segments_are_named_and_square() {
            let segments = vec![
                Segment::icon(crate::icons::Icon::List, "List"),
                Segment::icon(crate::icons::Icon::LayoutGrid, "Grid"),
            ];
            let mut g = ToggleGroup::new(segments, 0);
            let mut text = TextRenderer::new();
            g.measure_naturals(&mut text);
            assert!((g.naturals[0].0 - g.metrics.height.ceil()).abs() < 1e-9);
            let node = g.accessibility().expect("semantics");
            assert_eq!(node.children[1].name.as_deref(), Some("Grid"));
        }

        fn a_disabled_strip_takes_no_input() {
            let mut g = group_at(
                vec![Segment::new("A"), Segment::new("B")],
                0,
                Rect::new(0.0, 0.0, 200.0, 32.0),
            )
            .with_enabled(false);
            dispatch(&mut g, &cursor_moved(150.0, 16.0));
            dispatch(&mut g, &mouse_press());
            assert_eq!(g.selected(), 0);
            assert!(!g.is_focusable());
        }

        fn tabs_start_on_the_leading_edge_and_scroll_when_long() {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let labels: Vec<Segment> = ["Overview", "Activity", "Settings", "Members", "Billing"]
                .iter()
                .map(|l| Segment::new(*l))
                .collect();
            let rects = SegmentedFace::partition_segments(
                Rect::new(10.0, 0.0, 600.0, 32.0),
                &labels,
                SegmentedStyle::Tabs,
                ControlSize::Regular,
                &mut text,
                &theme,
            );
            assert!((rects[0].x0 - 10.0).abs() < 1e-9);
            let metrics = StripMetrics::of(ControlSize::Regular, &theme);
            let (x0, _) = tab_label_span(&rects[0], true, "Overview", &metrics, &mut text);
            assert!(
                (x0 - 10.0).abs() < 1e-9,
                "the first label sits on the strip's edge"
            );

            let mut g = ToggleGroup::new(labels, 4).with_style(SegmentedStyle::Tabs);
            g.metrics = metrics;
            g.measure_naturals(&mut text);
            g.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 32.0));
            assert!(g.overflows());
            g.reveal(4);
            let last = g.segment_rects[4];
            assert!(
                last.x1 <= 200.0 + 1e-6 && last.x0 >= 0.0,
                "the chosen tab scrolls into view: {last:?}"
            );
        }

        fn a_caption_names_the_strip_and_sits_above_it() {
            let mut g =
                ToggleGroup::new(vec![Segment::new("A"), Segment::new("B")], 0).caption("Repeat");
            g.adopt_theme(&Theme::dark());
            g.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 60.0));
            assert!(g.rect.y0 > 0.0);
            assert_eq!(
                g.accessibility().and_then(|n| n.name).as_deref(),
                Some("Repeat")
            );
        }

        nothing_chosen_until_the_person_picks();
        a_bound_selection_follows_its_state_both_ways();
        many_toggle_on_their_own_and_wrap();
        icon_segments_are_named_and_square();
        a_disabled_strip_takes_no_input();
        tabs_start_on_the_leading_edge_and_scroll_when_long();
        a_caption_names_the_strip_and_sits_above_it();
    }
}
