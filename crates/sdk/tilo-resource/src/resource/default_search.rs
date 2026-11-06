//! The default search entries a record publishes: one per item with a
//! `#[model(label)]`, or all its text in one entry.
//!
//! [`default_search`] is what a `#[resource]` without its own
//! `search = path` publishes, so every record is found by what it holds
//! without any search code. The app's own search function extends it by
//! calling it and adding what only the app knows (a service's remote
//! results, a scope).

/// The default search entries of a record: one per item with a
/// `#[model(label)]`, anchored at its item resource, or else all of its text
/// in one entry, found by its text. Items of a record holding several
/// collections are listed under each collection's title (its doc comment's
/// first line: "Folders", "Articles"), so the kinds read apart. An item is
/// found by all of its text, which is no preview of it: the palette shows
/// none for it. A `#[resource(search = path)]` function extends it by calling
/// it.
#[must_use]
pub fn default_search<R: crate::Resource + crate::Fields>(
    label: &str,
    record: &R,
) -> Vec<crate::SearchEntry> {
    let items = crate::Fields::item_search(record);
    let mut text = Vec::new();
    if items.is_empty() {
        record.collect_text(&mut text);
    }
    let titles: &[crate::shape::FieldShape] = match <R as crate::Model>::SHAPE {
        crate::Shape::Record(fields) => fields,
        _ => &[],
    };
    default_search_of(R::KIND, label, items, &text, titles)
}

/// [`default_search`] of a record of the kind `kind` holding `items`, or
/// else the text `text`; `fields` names its collections.
fn default_search_of(
    kind: &str,
    label: &str,
    items: Vec<crate::items::ItemText>,
    text: &[String],
    fields: &[crate::shape::FieldShape],
) -> Vec<crate::SearchEntry> {
    if !items.is_empty() {
        let several = items
            .iter()
            .any(|item| items.first().is_some_and(|first| first.field != item.field));
        let title_of = |field: &str| {
            fields
                .iter()
                .find(|shape| shape.name == field)
                .map(|shape| shape.title)
        };
        let mut entries = Vec::with_capacity(items.len());
        for item in items {
            let anchor = crate::items::item_resource_id_in(kind, item.field, &item.id);
            let mut entry = crate::SearchEntry::new(item.label, String::new()).anchored(anchor);
            entry.fields.push((item.text, crate::SearchWeight::Body));
            if several && let Some(title) = title_of(item.field) {
                entry = entry.grouped(title);
            }
            entries.push(entry);
        }
        return entries;
    }
    if text.is_empty() {
        return Vec::new();
    }
    vec![crate::SearchEntry {
        anchor: None,
        title: text.first().cloned().unwrap_or_else(|| label.to_owned()),
        fields: vec![(text.join("\n"), crate::SearchWeight::Body)],
        ..Default::default()
    }]
}
