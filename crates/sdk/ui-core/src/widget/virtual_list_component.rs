//! [`VirtualList`] as a described component: a list of [`ListRow`]s, each built from its props
//! when it scrolls into view.

use crate::component::{Accepts, Component, Described, Event, SlotType, Source, Value};
use crate::selection::KeySelection;
use crate::widget::{ListRow, ListRowProps};

use super::{VecListModel, VirtualList};

/// The list a described [`VirtualList`] is: its rows' props by key.
pub type DescribedList = VirtualList<VecListModel<ListRowProps, String>>;

/// The height a row is reckoned at before it is laid out: a row with a secondary line.
const ROW_ESTIMATE: f32 = 56.0;

/// A long list of rows that costs only the rows on screen: each is built when it scrolls into
/// view. Selection follows the person's choice and is reported; the list is one focus stop.
#[derive(Component, Default)]
#[component(name = "VirtualList", role = "List", label = "name")]
pub struct VirtualListProps {
    /// What the list is, for assistive technology.
    pub name: Option<String>,
    /// Moving through the rows with the keyboard selects them, as a list whose selection shows
    /// beside it does.
    pub selection_follows_focus: bool,
    /// The rows, in order.
    #[component(slot(default))]
    pub rows: Vec<ListRowProps>,
    /// The selected rows, by key.
    #[component(state)]
    pub selection: Option<Vec<String>>,
    /// How far the list is scrolled, in logical pixels.
    #[component(state)]
    pub scroll: Option<f64>,
    /// A row was selected or deselected; carries its key.
    #[component(event(value = String))]
    pub select: Event,
    /// A row was activated (a click, Enter, an assistive technology); carries its key.
    #[component(event(value = String))]
    pub activate: Event,
}

/// The rows of a virtual list: [`ListRow`]s only, each built anew as it scrolls into view.
impl SlotType for Vec<ListRowProps> {
    const MANY: bool = true;

    fn accepts() -> Accepts {
        Accepts::Component(ListRowProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, slot: &str) -> Result<(), String> {
        let mut rows: Vec<ListRowProps> = Vec::new();
        source.components(slot, &mut |child| {
            let row = ListRowProps::from_source(child).map_err(|error| error.to_string())?;
            if row.trailing.is_some() {
                return Err(
                    "a row of a virtual list is built again each time it scrolls into view, so it takes no trailing control"
                        .to_owned(),
                );
            }
            if rows.iter().any(|other| other.key == row.key) {
                return Err(format!("has two rows of the key {}", row.key));
            }
            rows.push(row);
            Ok(())
        })?;
        *self = rows;
        Ok(())
    }
}

impl Described for DescribedList {
    type Props = VirtualListProps;

    fn from_props(key: &str, props: VirtualListProps) -> Self {
        let VirtualListProps {
            name,
            selection_follows_focus,
            rows,
            selection,
            scroll,
            select,
            activate,
        } = props;
        // `fill` refused rows of one key, so only rows made in Rust can collide; they keep the
        // first of each key.
        let model =
            VecListModel::new(rows, |row| row.key.clone()).unwrap_or_else(|_| VecListModel {
                items: Vec::new(),
                keys: Vec::new(),
                index: std::collections::HashMap::new(),
            });
        let mut list =
            Self::with_selection_delegate(model, ROW_ESTIMATE, |model, index, key, _| {
                let row = model
                    .item(index)
                    .and_then(ListRowProps::reusable)
                    .map_or_else(
                        || ListRow::new(key.clone(), ""),
                        |props| ListRow::from_props(key, props),
                    );
                Box::new(row)
            })
            .with_id(key)
            .with_semantic_key(Clone::clone);
        if let Some(name) = name {
            list = list.with_accessible_name(name);
        }
        if selection_follows_focus {
            list = list.selection_follows_focus();
        }
        if let Some(keys) = selection {
            let mut chosen = KeySelection::new();
            for key in keys {
                chosen.toggle(key);
            }
            list = list.with_selection(chosen);
        }
        if !select.is_none() {
            list = list.on_select(move |key, selected| {
                if selected {
                    select.fire(Some(Value::Text(key)));
                }
            });
        }
        if !activate.is_none() {
            list = list.on_activate(move |key| activate.fire(Some(Value::Text(key))));
        }
        if let Some(offset) = scroll {
            list.set_scroll_offset(offset);
        }
        list
    }
}
