//! The filter chips under a query view's field: one per filterable field
//! of the app's types, drawn the same way in every app.
//!
//! A toggle field (Liked) is one chip that is on or off. A value field
//! (Artist) offers the values most of the results share as chips; picking
//! one narrows to it and leaves it as the field's one chip, removable. A
//! chip typed as a token (`artist:ada`) shows as picked; the token in the
//! field is what takes it away.

use std::collections::HashMap;
use std::rc::Rc;

use ui_core::widget::{Chip as ChipWidget, Element, Row};

use super::engine::QueryResults;
use super::kind::{FilterKind, Item, KindDecl, filters};
use super::query::{Chip, QuerySpec};

/// One chip a query view offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChipOffer {
    pub chip: Chip,
    /// What it reads ("Liked", "Artist: Ada").
    pub label: String,
    /// The query has it.
    pub selected: bool,
    /// It can be taken away with its own control: picked, not typed.
    pub removable: bool,
}

/// The chips for `query` and what it found: every toggle field's chip, and
/// for each value field either the chip the query has or the `per_field`
/// values most of `results` share.
#[must_use]
pub fn chip_offers(
    kinds: &[KindDecl],
    query: &QuerySpec,
    results: &QueryResults,
    per_field: usize,
) -> Vec<ChipOffer> {
    let rows: Vec<&Item> = results
        .local
        .iter()
        .flat_map(|group| &group.items)
        .chain(
            results
                .remote
                .iter()
                .flat_map(|section| section.groups.iter().flat_map(|group| &group.items)),
        )
        .collect();
    let mut offers = Vec::new();
    for field in filters(kinds) {
        let held: Vec<&Chip> = query
            .chips
            .iter()
            .filter(|chip| chip.key == field.key)
            .collect();
        match field.filter {
            Some(FilterKind::Toggle) => {
                let typed = held.iter().any(|chip| chip.typed);
                offers.push(ChipOffer {
                    chip: Chip {
                        typed,
                        ..Chip::toggle(field.key)
                    },
                    label: field.label.clone(),
                    selected: !held.is_empty(),
                    removable: false,
                });
            }
            Some(FilterKind::Value) if !held.is_empty() => {
                for chip in held {
                    offers.push(ChipOffer {
                        chip: chip.clone(),
                        label: chip.label(Some(field)),
                        selected: true,
                        removable: !chip.typed,
                    });
                }
            }
            Some(FilterKind::Value) => {
                let mut counts: HashMap<&str, usize> = HashMap::new();
                let mut first_seen: Vec<&str> = Vec::new();
                for row in &rows {
                    for value in row.values(field.key) {
                        let count = counts.entry(value).or_insert(0);
                        if *count == 0 {
                            first_seen.push(value);
                        }
                        *count = count.saturating_add(1);
                    }
                }
                // Most shared first; among equals, the one met first.
                let mut values: Vec<(usize, usize, &str)> = first_seen
                    .iter()
                    .enumerate()
                    .map(|(at, value)| (counts.get(value).copied().unwrap_or(0), at, *value))
                    .collect();
                values.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
                for (_, _, value) in values.into_iter().take(per_field) {
                    let chip = Chip::value(field.key, value);
                    offers.push(ChipOffer {
                        label: chip.label(Some(field)),
                        chip,
                        selected: false,
                        removable: false,
                    });
                }
            }
            None => {}
        }
    }
    offers
}

/// The chips as the row under the field: a press on one hands `on_change`
/// the query with it picked or taken away.
#[must_use]
pub fn chip_bar(
    offers: &[ChipOffer],
    query: &QuerySpec,
    on_change: &Rc<dyn Fn(QuerySpec)>,
) -> Element {
    let mut row = Row::new()
        .with_gap(6.0)
        .with_align_items(ui_core::taffy::AlignItems::Center);
    for offer in offers {
        let next = query.toggled(&offer.chip);
        let mut chip = ChipWidget::new(offer.label.clone())
            .with_id(format!(
                "view-chip:{}:{}",
                offer.chip.key,
                offer.chip.value.as_deref().unwrap_or_default()
            ))
            .selected(offer.selected);
        // A typed chip is taken away by its token in the field, so pressing
        // it does nothing; every other chip flips or goes.
        if !(offer.selected && offer.chip.typed) {
            let change = Rc::clone(on_change);
            let flipped = next.clone();
            chip = chip.on_toggle(move |_| change(flipped.clone()));
        }
        if offer.removable {
            let change = Rc::clone(on_change);
            chip = chip.on_remove(move || change(next.clone()));
        }
        row = row.push(Box::new(chip));
    }
    Box::new(row)
}
