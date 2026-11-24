//! [`DiffGrid`] as a described component: two texts aligned line by line, each with its own line
//! numbers, the changed rows washed in their tone and a strip showing where the changes stand.

use crate::component::{Component, Described, Event};

use super::super::code_editor::Language;
use super::{DiffGrid, DiffSide};

crate::component::choice!(DiffSide, "DiffSide", {
    Both => "both",
    Left => "left",
    Right => "right",
});

/// Two texts aligned line by line, as a comparison reads: each side's own line numbers, the
/// changed rows washed, and a strip down the side where the changes stand.
#[derive(Component, Default)]
#[component(name = "DiffGrid", role = "Table", label = "label")]
pub struct DiffGridProps {
    /// The left side's lines, one per aligned row: an empty line where it has none.
    #[component(required, positional)]
    pub left: Vec<String>,
    /// The right side's lines, one per aligned row: an empty line where it has none.
    pub right: Vec<String>,
    /// One character per aligned row: `" "` where both sides hold the same line, `"a"` a line
    /// only the right has, `"r"` a line only the left has, `"m"` a line both have and differ.
    pub kinds: String,
    /// The first row of the change the window is at: the grid brings it into view when it
    /// changes.
    pub at: Option<i64>,
    /// Which side shows: both side by side, or one of them alone where the window is narrow.
    pub side: DiffSide,
    /// The language the lines are coloured as (the language's own name, `rust`, `json`); plain
    /// text unless said.
    pub language: Option<Language>,
    /// What the grid shows, as assistive technology announces it: "a.rs and b.rs, 3 changes".
    pub label: Option<String>,
    /// A row was pressed: carries its place among the aligned rows.
    #[component(event(value = i64))]
    pub press: Event,
    /// The wheel or the scroll bar asked to show other rows: carries the rows the first one
    /// moves by, down for a positive number.
    #[component(event(value = i64))]
    pub wheel: Event,
}

impl Described for DiffGrid {
    type Props = DiffGridProps;

    fn from_props(_key: &str, props: DiffGridProps) -> Self {
        let mut grid = Self::default();
        grid.set_props(props);
        grid
    }

    fn set_props(&mut self, props: DiffGridProps) -> bool {
        let text_changed =
            props.left != self.left || props.right != self.right || props.language != self.language;
        let at_changed = props.at != self.at;
        let numbering_changed = props.kinds != self.kinds || text_changed;
        let changed = text_changed
            || at_changed
            || numbering_changed
            || props.side != self.side
            || props.label != self.label;
        self.left = props.left;
        self.right = props.right;
        self.kinds = props.kinds;
        self.at = props.at;
        self.side = props.side;
        self.language = props.language;
        self.label = props.label;
        if text_changed {
            self.first = 0;
            self.highlight_rows();
        }
        if numbering_changed {
            self.number_rows();
        }
        if at_changed && let Some(at) = props.at.and_then(|at| usize::try_from(at).ok()) {
            self.reveal(at);
        }
        let press = props.press;
        self.on_press = (!press.is_none()).then(|| {
            Box::new(move |row: usize| {
                press.fire(Some(crate::component::Value::Int(
                    i64::try_from(row).unwrap_or(i64::MAX),
                )));
            }) as Box<dyn FnMut(usize)>
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
