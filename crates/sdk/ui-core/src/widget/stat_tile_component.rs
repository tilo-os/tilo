//! [`StatTile`] and [`StatGroup`] as described components.

use crate::component::{Component, Described, Event};
use crate::widget::Tone;
use crate::widget::activation::Activation;

use super::{StatGroup, StatTile, Trend};

crate::component::choice!(Trend, "Trend", {
    Up => "up",
    Down => "down",
    Flat => "flat",
});

/// One number and what it measures, on the content ground: a dashboard's reading, a backup's
/// size. A tile reads top to bottom: its label, the value with its unit, a note, which way it
/// moved, and a sparkline of its recent readings. A tile that opens a detail is pressed.
#[derive(Component, Default)]
#[component(name = "StatTile", role = "Group", label = "label")]
pub struct StatTileProps {
    /// The tile's stable key in a group.
    #[component(key)]
    pub key: String,
    /// What the number measures, in sentence case: "Active users".
    #[component(required, positional)]
    pub label: String,
    /// The number, as it reads: "1,284", "99.9".
    #[component(required, positional)]
    pub value: String,
    /// Its unit, after the value: "GB", "ms".
    pub unit: Option<String>,
    /// A neutral qualifier read with the value: "of 2 TB", "estimated".
    pub note: Option<String>,
    /// How it changed, beside an arrow of its trend: "12% this week".
    pub change: Option<String>,
    /// Which way it moved: `up` unless said, when it has a change. Whether that is good is its
    /// tone, not the arrow.
    pub trend: Option<Trend>,
    /// The state of what it measures, as a ticker down its leading edge: a failed backup is
    /// danger, a healthy tunnel success.
    pub tone: Option<Tone>,
    /// The one hero number of a screen, set large.
    pub hero: bool,
    /// Its recent readings, oldest first, as a sparkline under it.
    pub samples: Vec<f64>,
    /// Whether it is the selected one, when it opens a detail.
    pub selected: bool,
    /// The tile was pressed: open its detail.
    #[component(event)]
    pub activate: Event,
}

impl StatTileProps {
    fn change(&self) -> Option<(String, Trend)> {
        self.change
            .clone()
            .map(|text| (text, self.trend.unwrap_or(Trend::Up)))
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "a sparkline's readings are drawn at f32"
    )]
    fn samples(&self) -> Option<Vec<f32>> {
        (!self.samples.is_empty()).then(|| self.samples.iter().map(|s| *s as f32).collect())
    }
}

impl Described for StatTile {
    type Props = StatTileProps;

    fn from_props(key: &str, props: StatTileProps) -> Self {
        let change = props.change();
        let samples = props.samples();
        let StatTileProps {
            key: _,
            label,
            value,
            unit,
            note,
            change: _,
            trend: _,
            tone,
            hero,
            samples: _,
            selected,
            activate,
        } = props;
        let mut tile = Self::new_owned(label, value);
        tile.unit = unit;
        tile.note = note;
        tile.change = change;
        tile.tone = tone;
        tile.hero = hero;
        tile.sparkline = samples;
        if !activate.is_none() {
            tile.activation = Activation::new(key);
            let label = tile.label.clone();
            tile.activation.set_event(activate, &label);
            tile = tile.selected(selected);
        }
        tile
    }

    /// A new reading (its value, note, change and samples) is taken in place.
    fn set_props(&mut self, props: StatTileProps) -> bool {
        let same_shape = props.label == self.label
            && props.unit == self.unit
            && props.tone == self.tone
            && props.hero == self.hero
            && props.samples.is_empty() == self.sparkline.is_none();
        if !same_shape {
            return false;
        }
        let change = props.change();
        if let Some(samples) = props.samples() {
            self.set_live(props.value, samples);
        } else {
            self.set_value(props.value);
        }
        self.set_note(props.note);
        self.set_change(change);
        self.set_selected(props.selected);
        true
    }
}

/// A set of stat tiles that reflows by width: all in one row when they fit, then half per row
/// (a 2 by 2 for four), then one per row. The tiles are separated by space, never boxed.
#[derive(Component, Default)]
#[component(name = "StatGroup", role = "Group")]
pub struct StatGroupProps {
    /// The tiles, in order.
    #[component(slot(default))]
    pub tiles: Vec<StatTileProps>,
}

crate::component::part_slot!(StatTileProps);

impl Described for StatGroup {
    type Props = StatGroupProps;

    fn from_props(_key: &str, props: StatGroupProps) -> Self {
        let tiles = props
            .tiles
            .into_iter()
            .map(|tile| {
                let key = tile.key.clone();
                Box::new(StatTile::from_props(&key, tile)) as crate::widget::Element
            })
            .collect();
        Self::new(tiles)
    }

    /// The same tiles take their new readings in place.
    fn set_props(&mut self, props: StatGroupProps) -> bool {
        if props.tiles.len() != self.tiles.len() {
            return false;
        }
        for (index, tile) in props.tiles.into_iter().enumerate() {
            let Some(held) = self.tile_mut(index) else {
                return false;
            };
            if !held.set_props(tile) {
                return false;
            }
        }
        true
    }
}
