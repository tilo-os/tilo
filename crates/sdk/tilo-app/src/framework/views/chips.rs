//! The filter chips under a query view's field: one per filterable field
//! of the app's types, drawn the same way in every app.
//!
//! A toggle field (Liked) is a chip that is on or off. A value field
//! (Artist, Year) is a chip that opens a list of the values the results
//! share, with how many share each, any number of which may be chosen; the
//! chip then reads its choice ("Artist: Aurora Skies") and offers to clear
//! it. A value typed as a token (`artist:ada`) is the same choice; the token
//! in the field is what takes it away.

use std::collections::HashMap;
use std::rc::Rc;

use ui_core::vello::kurbo::Rect;
use ui_core::widget::context_menu::{ContextMenu, MenuItem};
use ui_core::widget::{Chip as ChipWidget, Element, Row};

use super::engine::QueryResults;
use super::kind::{FilterKind, Item, KindDecl, filters};
use super::query::{Chip, QuerySpec};

/// How long a chip's label may grow with the values it names before it says
/// how many more there are.
const LABEL_VALUES: usize = 2;

/// One value a value filter offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FacetValue {
    pub value: String,
    /// How many of the results have it.
    pub count: usize,
    pub chosen: bool,
    /// It was typed as a token.
    pub typed: bool,
}

/// The chip of one filterable field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FacetOffer {
    pub key: String,
    /// The field's name ("Artist").
    pub name: String,
    pub kind: FilterKind,
    /// What the chip reads: its name, with its choice once it has one
    /// ("Artist: Aurora Skies").
    pub label: String,
    /// The filter narrows the results now.
    pub active: bool,
    /// Its choice can be cleared with the chip's own control: something of it
    /// was picked, not typed.
    pub clearable: bool,
    /// A value filter's values, the most shared first, chosen ones always
    /// among them.
    pub values: Vec<FacetValue>,
}

impl FacetOffer {
    /// The query with `value` of this filter chosen, or no longer chosen.
    #[must_use]
    pub fn toggled(&self, query: &QuerySpec, value: &str) -> QuerySpec {
        query.toggled(&Chip::value(self.key.clone(), value))
    }
}

/// The chips for `query` and what it found: one per filterable field of
/// `kinds`, in declaration order, each value counted among `results`.
#[must_use]
pub fn facet_offers(
    kinds: &[KindDecl],
    query: &QuerySpec,
    results: &QueryResults,
) -> Vec<FacetOffer> {
    let rows = result_rows(results);
    offers_counting(kinds, query, &|_| rows.clone())
}

/// Every row `results` shows, local first.
pub(crate) fn result_rows(results: &QueryResults) -> Vec<Item> {
    results
        .local
        .iter()
        .flat_map(|group| &group.items)
        .chain(
            results
                .remote
                .iter()
                .flat_map(|section| section.groups.iter().flat_map(|group| &group.items)),
        )
        .cloned()
        .collect()
}

/// The chips for `query`, each value filter's values counted among
/// `rows_for(key)`: what the query finds without that filter's own choice,
/// so choosing one artist still lists the others to add.
pub(crate) fn offers_counting(
    kinds: &[KindDecl],
    query: &QuerySpec,
    rows_for: &dyn Fn(&str) -> Vec<Item>,
) -> Vec<FacetOffer> {
    let mut offers = Vec::new();
    for field in filters(kinds) {
        let Some(kind) = field.filter else {
            continue;
        };
        let held: Vec<&Chip> = query
            .chips
            .iter()
            .filter(|chip| chip.key == field.key)
            .collect();
        let active = !held.is_empty();
        let clearable = held.iter().any(|chip| !chip.typed);
        let (label, values) = match kind {
            FilterKind::Toggle => (field.label.clone(), Vec::new()),
            FilterKind::Value => {
                let rows = rows_for(field.key);
                let rows: Vec<&Item> = rows.iter().collect();
                let values = facet_values(field.key, &rows, &held);
                let chosen: Vec<&str> = values
                    .iter()
                    .filter(|value| value.chosen)
                    .map(|value| value.value.as_str())
                    .collect();
                (choice_label(&field.label, &chosen), values)
            }
        };
        offers.push(FacetOffer {
            key: field.key.to_owned(),
            name: field.label.clone(),
            kind,
            label,
            active,
            clearable,
            values,
        });
    }
    offers
}

/// The values of the field `key` among `rows`, the most shared first and,
/// among equals, the one met first; every chosen value is among them.
fn facet_values(key: &str, rows: &[&Item], held: &[&Chip]) -> Vec<FacetValue> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    for row in rows {
        for value in row.values(key) {
            let count = counts.entry(value).or_insert(0);
            if *count == 0 {
                order.push(value);
            }
            *count = count.saturating_add(1);
        }
    }
    let fold = |text: &str| tilo_search::fold(text.trim());
    let chosen_of = |value: &str| {
        held.iter().find(|chip| {
            chip.value
                .as_deref()
                .is_some_and(|held| fold(held) == fold(value))
        })
    };
    let mut values: Vec<(usize, usize, FacetValue)> = order
        .iter()
        .enumerate()
        .map(|(at, value)| {
            let chip = chosen_of(value);
            (
                counts.get(value).copied().unwrap_or(0),
                at,
                FacetValue {
                    value: (*value).to_owned(),
                    count: counts.get(value).copied().unwrap_or(0),
                    chosen: chip.is_some(),
                    typed: chip.is_some_and(|chip| chip.typed),
                },
            )
        })
        .collect();
    values.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut out: Vec<FacetValue> = values.into_iter().map(|(_, _, value)| value).collect();
    // A chosen value nothing shown has any more still reads as chosen.
    for chip in held {
        if let Some(value) = &chip.value
            && !out.iter().any(|known| fold(&known.value) == fold(value))
        {
            out.insert(
                0,
                FacetValue {
                    value: value.clone(),
                    count: 0,
                    chosen: true,
                    typed: chip.typed,
                },
            );
        }
    }
    out
}

/// "Artist", "Artist: Ada", "Artist: Ada, Grace", "Artist: Ada and 2 more".
fn choice_label(name: &str, chosen: &[&str]) -> String {
    match chosen {
        [] => name.to_owned(),
        few if few.len() <= LABEL_VALUES => format!("{name}: {}", few.join(", ")),
        [first, rest @ ..] => format!("{name}: {first} and {} more", rest.len()),
    }
}

/// What a press on a chip asks of the app: a value filter's chip opens its
/// list of values, the app showing it where it suits (`key`).
pub type OpenFacet = Rc<dyn Fn(&str)>;

/// The chips as a row: a toggle flips, a value filter's chip opens its
/// values (`open`), and an active chip's clear control hands `on_change`
/// the query without its choice.
#[must_use]
pub fn facet_bar(
    offers: &[FacetOffer],
    query: &QuerySpec,
    on_change: &Rc<dyn Fn(QuerySpec)>,
    open: &OpenFacet,
) -> Element {
    let mut row = Row::new()
        .with_gap(6.0)
        .with_align_items(ui_core::taffy::AlignItems::Center);
    for offer in offers {
        let mut chip =
            ChipWidget::new(offer.label.clone()).with_id(format!("view-chip:{}", offer.key));
        match offer.kind {
            FilterKind::Toggle => {
                chip = chip.selected(offer.active);
                // A typed toggle is taken away by its token in the field.
                if !offer.active || offer.clearable {
                    let change = Rc::clone(on_change);
                    let next = query.toggled(&Chip::toggle(offer.key.clone()));
                    chip = chip.on_toggle(move |_| change(next.clone()));
                }
            }
            FilterKind::Value => {
                let open = Rc::clone(open);
                let key = offer.key.clone();
                chip = chip
                    .menu()
                    .current(offer.active)
                    .on_click(move || open(&key));
                if offer.clearable {
                    let change = Rc::clone(on_change);
                    let next = query.cleared(&offer.key);
                    chip = chip.on_remove(move || change(next.clone()));
                }
            }
        }
        row = row.push(Box::new(chip));
    }
    Box::new(row)
}

/// The values of one value filter, opened over its chip: each with how many
/// results share it and a check on those chosen, since any number may be.
/// Choosing one toggles it in the query; the menu closes, and opening it
/// again chooses another. An app that paints its own surface owns one,
/// draws it over its page and routes events to it while it is open.
#[derive(Default)]
pub struct FacetMenu {
    menu: ContextMenu,
    /// What choosing each row asks, in row order.
    choices: Vec<QuerySpec>,
}

impl FacetMenu {
    /// Open `offer`'s values for `query` beside `anchor` (its chip), kept
    /// inside `viewport`.
    pub fn open(&mut self, offer: &FacetOffer, query: &QuerySpec, anchor: Rect, viewport: Rect) {
        let mut menu = ContextMenu::new();
        self.choices.clear();
        for value in &offer.values {
            let mut item = MenuItem::selection(value.value.clone())
                .with_checked(value.chosen)
                // A typed token is taken away by editing the field.
                .with_enabled(!value.typed);
            if value.count > 0 {
                item = item.with_detail(value.count.to_string());
            }
            menu.push(item);
            self.choices.push(offer.toggled(query, &value.value));
        }
        menu.open_anchored(anchor, viewport);
        self.menu = menu;
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    /// The menu, to draw and to route events to.
    pub fn menu_mut(&mut self) -> &mut ContextMenu {
        &mut self.menu
    }

    /// The query a chosen value asks for, once one was chosen.
    pub fn take_choice(&mut self) -> Option<QuerySpec> {
        let chosen = self.menu.take_selection()?;
        self.choices.get(chosen).cloned()
    }
}
