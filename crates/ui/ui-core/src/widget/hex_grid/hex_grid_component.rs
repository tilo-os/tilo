//! [`HexGrid`] as a described component: a file's bytes, as a grid of hexadecimal pairs and
//! the same bytes as text, with the cursor, the selection and a search's matches on it.

use crate::component::{Component, Described, Event};

use super::{HexColumn, HexGrid};

crate::component::choice!(HexColumn, "HexColumn", {
    Hex => "hex",
    Ascii => "ascii",
});

/// Bytes as a dense grid: an offset down the side, hexadecimal pairs and the same bytes as text
/// across, with the cursor, the selection and a search's matches drawn over them.
#[derive(Component)]
#[component(name = "HexGrid", role = "Table", label = "label")]
pub struct HexGridProps {
    /// The rows shown, each its bytes as hexadecimal pairs without spaces (`"00ff2a"`).
    #[component(required, positional)]
    pub rows: Vec<String>,
    /// For each row, a `"0"` or `"1"` for each byte: whether it differs from what the file holds
    /// where it was read.
    pub changed: Vec<String>,
    /// The rows the file has.
    pub total: i64,
    /// The row `rows` starts at, counted from 0.
    pub first: i64,
    /// The bytes a row holds: 8 to 64, or 0 for as many as the width holds.
    pub columns: i64,
    /// The byte the cursor is on.
    pub cursor: Option<i64>,
    /// The other end of the selection: where it is drawn from the cursor.
    pub anchor: Option<i64>,
    /// The column the keys edit.
    pub column: HexColumn,
    /// The bytes a search found, each as `"start:end"`, as one of the grid's own bytes.
    pub matches: Vec<String>,
    /// The match the cursor is on, by its place in `matches`.
    pub at_match: Option<i64>,
    /// What the grid shows, as assistive technology announces it: "bytes.bin, 4096 bytes".
    pub label: Option<String>,
    /// A byte was pressed: carries its offset, the column pressed, and `"shift"` when Shift held,
    /// `["31", "hex", "shift"]`.
    #[component(event(value = Vec<String>))]
    pub press: Event,
    /// A key was pressed while the grid had the keyboard: carries it as a person's keyboard
    /// names it (`"a"`, `"ArrowLeft"`, `"Ctrl+S"`).
    #[component(event(value = String))]
    pub key: Event,
    /// The wheel or the scroll bar asked to show other rows: carries the rows the first one
    /// moves by, down for a positive number.
    #[component(event(value = i64))]
    pub wheel: Event,
    /// Whether a press on a byte runs now.
    pub enabled: bool,
    /// Why it cannot be pressed now, where the label does not say (an action whose requirement
    /// does not hold): spoken as its description beside its name.
    pub description: Option<String>,
}

/// A grid takes a press until something says otherwise.
impl Default for HexGridProps {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            changed: Vec::new(),
            total: 0,
            first: 0,
            columns: 0,
            cursor: None,
            anchor: None,
            column: HexColumn::default(),
            matches: Vec::new(),
            at_match: None,
            label: None,
            press: Event::None,
            key: Event::None,
            wheel: Event::None,
            enabled: true,
            description: None,
        }
    }
}

impl Described for HexGrid {
    type Props = HexGridProps;

    fn from_props(_key: &str, props: HexGridProps) -> Self {
        let mut grid = Self::default();
        grid.set_props(props);
        grid
    }

    fn set_props(&mut self, props: HexGridProps) -> bool {
        let matches: Vec<(usize, usize)> = props
            .matches
            .iter()
            .filter_map(|span| parse_span(span))
            .collect();
        let mut changed = false;
        if props.rows != self.rows
            || props.changed != self.changed
            || props.total != self.total
            || props.first != self.first
            || props.columns != self.columns
            || props.cursor != self.cursor
            || props.anchor != self.anchor
            || props.column != self.column
            || matches != self.matches
            || props.at_match != self.at_match
            || props.label != self.label
            || props.enabled != self.enabled
            || props.description != self.description
        {
            changed = true;
        }
        self.rows = props.rows;
        self.changed = props.changed;
        self.total = props.total;
        self.first = props.first;
        self.columns = props.columns;
        self.cursor = props.cursor;
        self.anchor = props.anchor;
        self.column = props.column;
        self.matches = matches;
        self.at_match = props.at_match;
        self.label = props.label;
        self.enabled = props.enabled;
        self.description = props.description;
        let press = props.press;
        self.on_press = (props.enabled && !press.is_none()).then(|| {
            Box::new(move |at: usize, column: HexColumn, extend: bool| {
                press.fire(Some(crate::component::Value::List(vec![
                    crate::component::Value::Text(at.to_string()),
                    crate::component::Value::Text(column.name().to_owned()),
                    crate::component::Value::Text(if extend { "shift" } else { "" }.to_owned()),
                ])));
            }) as Box<dyn FnMut(usize, HexColumn, bool)>
        });
        let key = props.key;
        self.on_key = (!key.is_none()).then(|| {
            Box::new(move |pressed: String| {
                key.fire(Some(crate::component::Value::Text(pressed)));
            }) as Box<dyn FnMut(String)>
        });
        let wheel = props.wheel;
        self.on_wheel = (!wheel.is_none()).then(|| {
            Box::new(move |rows: i64| {
                wheel.fire(Some(crate::component::Value::Int(rows)));
            }) as Box<dyn FnMut(i64)>
        });
        if changed {
            self.dirty = true;
        }
        changed
    }
}

/// `"start:end"` as the span it writes.
fn parse_span(span: &str) -> Option<(usize, usize)> {
    let (start, end) = span.split_once(':')?;
    Some((start.parse().ok()?, end.parse().ok()?))
}
