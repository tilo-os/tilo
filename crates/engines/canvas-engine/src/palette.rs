//! Authored literal colours, shared by human swatches and agent inspection.
//! This reads editable paint, never samples image pixels.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::model::{Artwork, Mark};

/// One authored colour and how many matching marks use it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Colour {
    pub colour: String,
    pub marks: usize,
}

/// Canonical hex paint; retain alpha so choosing a swatch preserves it.
fn literal(value: &str) -> Option<String> {
    let digits = value.strip_prefix('#')?;
    if !matches!(digits.len(), 6 | 8) || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(value.to_ascii_lowercase())
}

/// Count each colour once per mark, even when fill and stroke match.
/// Erasers carry no paint; named pigments stay in their medium's palette.
pub fn used<'a>(marks: impl IntoIterator<Item = &'a Mark>) -> Vec<Colour> {
    let mut counts = BTreeMap::<String, usize>::new();
    for mark in marks {
        if mark.erases() {
            continue;
        }
        let gradient = mark.style.gradient.as_ref();
        let colours: BTreeSet<_> = mark
            .style
            .fill
            .iter()
            .chain(mark.style.stroke.iter())
            .chain(
                gradient
                    .into_iter()
                    .flat_map(|gradient| gradient.stops.iter().map(|stop| &stop.1)),
            )
            .filter_map(|value| literal(value))
            .collect();
        for colour in colours {
            let count = counts.entry(colour).or_default();
            *count = count.saturating_add(1);
        }
    }
    counts
        .into_iter()
        .map(|(colour, marks)| Colour { colour, marks })
        .collect()
}

/// Counts changing during painting do not rebuild the stable picker.
/// The ground is reusable too; empty artworks stay empty.
pub fn swatches(art: &Artwork) -> Vec<String> {
    if art.marks.is_empty() {
        return Vec::new();
    }
    let mut colours: BTreeSet<_> = used(&art.marks)
        .into_iter()
        .map(|colour| colour.colour)
        .collect();
    if colours.is_empty() {
        return Vec::new();
    }
    colours.extend(literal(&art.ground.colour));
    colours.into_iter().take(32).collect()
}
