//! [`ToggleGroup`] and [`ViewTabs`] as described components.

use crate::component::{Component, Described, Event, Value};
use crate::icons::Icon;
use crate::widget::ControlSize;

use super::{Segment, ToggleGroup};
use crate::widget::ViewTabs;

crate::component::part_slot!(SegmentProps);

/// A small set of choices, one of which is made: an alignment, a density, a unit. The chosen one is
/// a thumb lifted off the track.
#[derive(Component)]
#[component(name = "ToggleGroup", role = "Group", label = "caption")]
pub struct ToggleGroupProps {
    /// The choices, in order.
    #[component(slot(default))]
    pub options: Vec<SegmentProps>,
    /// Which choice is made, counted from 0: none until the person makes one.
    pub selected: Option<i64>,
    /// A label over the strip naming what it chooses between.
    pub caption: Option<String>,
    /// Its spoken name, for a strip its caption does not name.
    pub label: Option<String>,
    /// How large its segments are: `regular` unless said.
    pub size: ControlSize,
    /// Share the row's width equally instead of fitting each label.
    pub fill: bool,
    /// Whether it can be chosen from now.
    pub enabled: bool,
    /// A choice was made: carries the index of the chosen option, counted from 0.
    #[component(event(value = i64))]
    pub change: Event,
}

/// A choosable strip is enabled until something says otherwise, like the segments in it.
impl Default for ToggleGroupProps {
    fn default() -> Self {
        Self {
            options: Vec::new(),
            selected: None,
            caption: None,
            label: None,
            size: ControlSize::default(),
            fill: false,
            enabled: true,
            change: Event::default(),
        }
    }
}

/// One choice of a [`ToggleGroup`] or a [`ViewTabs`]: what it says, and how it shows.
#[derive(Clone, Component)]
#[component(name = "Segment", role = "RadioButton", label = "label")]
pub struct SegmentProps {
    /// What it says.
    #[component(required, positional)]
    pub label: String,
    /// What it shows when its full label does not fit its share: "Mon" for "Monday".
    pub short: Option<String>,
    /// A glyph: before the label on a tab ([`ViewTabs`]), and instead of it on
    /// any other strip, named by the label either way.
    pub icon: Option<Icon>,
    /// Its keyboard shortcut, named in its tooltip: "B", "Shift+R".
    pub hint: Option<String>,
    /// Whether it can be chosen now.
    pub enabled: bool,
}

impl Default for SegmentProps {
    fn default() -> Self {
        Self {
            label: String::new(),
            short: None,
            icon: None,
            hint: None,
            enabled: true,
        }
    }
}

impl From<SegmentProps> for Segment {
    fn from(props: SegmentProps) -> Self {
        let mut segment = match props.icon {
            Some(icon) => Segment::icon(icon, props.label),
            None => Segment::new(props.label),
        };
        if let Some(short) = props.short {
            segment = segment.short(short);
        }
        if let Some(hint) = props.hint {
            segment = segment.with_hint(hint);
        }
        segment.with_enabled(props.enabled)
    }
}

/// Tabs that switch what a window's content shows: a request's Params, Headers and Body, a
/// container's Logs and Stats. Assistive technology meets them as a tab list whose chosen tab
/// is selected, not checked.
#[derive(Component)]
#[component(name = "ViewTabs", role = "TabBar", label = "label")]
pub struct ViewTabsProps {
    /// The tabs, in order.
    #[component(slot(default))]
    pub tabs: Vec<SegmentProps>,
    /// Which tab shows, counted from 0.
    pub selected: Option<i64>,
    /// Its spoken name for assistive technology.
    pub label: Option<String>,
    /// How large the tabs are: `regular` unless said.
    pub size: ControlSize,
    /// Share the row's width equally instead of fitting each label.
    pub fill: bool,
    /// Whether it can be chosen from now.
    pub enabled: bool,
    /// Another tab was chosen: carries its index, counted from 0.
    #[component(event(value = i64))]
    pub change: Event,
}

/// Tabs are enabled until something says otherwise: a bar draws them to be chosen from.
impl Default for ViewTabsProps {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            selected: None,
            label: None,
            size: ControlSize::default(),
            fill: false,
            enabled: true,
            change: Event::default(),
        }
    }
}

/// `index` as a choice among `count` options: the last one for an index past the end.
fn place(index: i64, count: usize) -> usize {
    usize::try_from(index.max(0))
        .unwrap_or_default()
        .min(count.saturating_sub(1))
}

/// A chosen index as the number an event carries.
fn carried(index: usize) -> Value {
    Value::Int(i64::try_from(index).unwrap_or(i64::MAX))
}

impl Described for ToggleGroup {
    type Props = ToggleGroupProps;

    fn from_props(key: &str, props: ToggleGroupProps) -> Self {
        let ToggleGroupProps {
            options,
            selected,
            caption,
            label,
            size,
            fill,
            enabled,
            change,
        } = props;
        let segments: Vec<Segment> = options.into_iter().map(Segment::from).collect();
        let chosen = selected.map(|index| place(index, segments.len()));
        let mut group = match chosen {
            Some(index) => Self::new(segments, index),
            None => Self::unchosen(segments),
        };
        if let Some(caption) = caption {
            group = group.caption(caption);
        }
        if let Some(label) = label {
            group = group.with_accessible_name(label);
        }
        if size != ControlSize::default() {
            group = group.control_size(size);
        }
        if fill {
            group = group.fill();
        }
        if !change.is_none() {
            let reported = change;
            group = group.on_change(move |index| reported.fire(Some(carried(index))));
        }
        group.with_enabled(enabled).with_id(key)
    }

    /// What is chosen changes in place; a strip of other choices is built again.
    fn set_props(&mut self, props: ToggleGroupProps) -> bool {
        if props.options.len() != self.count() {
            return false;
        }
        self.set_enabled(props.enabled);
        match props.selected {
            Some(index) => self.set_selected(place(index, self.count())),
            None => self.set_selected(crate::widget::toggle_group::NONE_CHOSEN),
        }
        true
    }
}

impl Described for ViewTabs {
    type Props = ViewTabsProps;

    fn from_props(key: &str, props: ViewTabsProps) -> Self {
        let ViewTabsProps {
            tabs,
            selected,
            label,
            size,
            fill,
            enabled,
            change,
        } = props;
        let segments: Vec<Segment> = tabs.into_iter().map(Segment::from).collect();
        let chosen = selected.map_or(0, |index| place(index, segments.len()));
        let mut tabs = Self::new(segments, chosen);
        if let Some(label) = label {
            tabs = tabs.with_accessible_name(label);
        }
        tabs = tabs.control_size(size);
        if fill {
            tabs = tabs.fill();
        }
        if !change.is_none() {
            let reported = change;
            tabs = tabs.on_change(move |index| reported.fire(Some(carried(index))));
        }
        tabs.with_enabled(enabled).with_id(key)
    }

    /// The tab showing changes in place; other tabs are built again.
    fn set_props(&mut self, props: ViewTabsProps) -> bool {
        if props.tabs.len() != self.count() {
            return false;
        }
        self.set_selected(props.selected.map_or(0, |index| place(index, self.count())));
        true
    }
}
