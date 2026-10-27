//! Bringing what earlier windows of an app kept into the app's one record.
//!
//! An app keeps one record per workspace: every window of it there opens
//! that record, across closing and reopening it and restarts. Apps used to
//! keep a record per window instead (`derived:default@tab:<window>`), so a
//! new window opened an empty record and what an earlier window held was
//! reachable only by restoring that exact window. When an app's workspace
//! record is first created, what every such window record of the app this
//! device holds there is copied into it, once.
//!
//! A copy, not a merge of the documents: each window record was created on
//! its own, under its own record's genesis, so its collections are other
//! objects than the workspace record's; merging the documents would keep
//! one window's collection and hide the others' as conflicts. The copy goes
//! through the documents' values instead:
//!
//! - a map key the workspace record lacks is copied whole;
//! - a value a window changed from what a new record starts with wins
//!   (later windows, in record id order, over earlier ones), and a value it
//!   left as it was changes nothing;
//! - a list gains the window's elements it does not hold yet: an element
//!   that names itself (a map with an `id`) joins the element of that id,
//!   and any other is added unless an equal one is there;
//! - text the workspace record holds none of is copied.
//!
//! Every id inside the values is kept, so references between them still
//! resolve. The window records are only read: they stay on disk as they
//! were, with their history, so nothing is lost if the copy has to be done
//! again by hand.

use automerge::{ObjId, ObjType, ROOT, ReadDoc, Value, transaction::Transactable as _};
use tilo_resource::Doc;

/// Copy what each of `windows` (window records of the app, read-only, in
/// record id order) holds into `record`, the app's workspace record just
/// created. Values `record` holds as a new record starts with are taken
/// from `defaults`, a copy of it from before the copy. A window record that
/// cannot be read as the app's resource is left out and logged.
pub(crate) fn adopt(
    record: &mut Doc,
    windows: &[(ui_core::record::RecordId, Doc)],
    migrate: &dyn Fn(&mut Doc) -> Result<(), String>,
) -> Result<(), String> {
    let defaults = record.clone();
    for (id, window) in windows {
        let mut window = window.clone();
        if let Err(error) = migrate(&mut window) {
            tracing::warn!(%id, %error, "not adopting a window record that does not read as the app's");
            continue;
        }
        merge_map(record, &ROOT, &window, &ROOT, Some((&defaults, &ROOT)))
            .map_err(|error| error.to_string())?;
        tracing::info!(%id, "adopted an earlier window's record into the workspace record");
    }
    Ok(())
}

type Defaults<'a> = Option<(&'a Doc, &'a ObjId)>;

/// The prefix of the keys the framework keeps at a record's root: its
/// identity (`tilo:record`), its storage version (`tilo:version`).
const FRAMEWORK_KEYS: &str = "tilo:";

fn merge_map(
    record: &mut Doc,
    into: &ObjId,
    window: &Doc,
    from: &ObjId,
    defaults: Defaults<'_>,
) -> Result<(), automerge::AutomergeError> {
    let keys: Vec<String> = window.keys(from).collect();
    for key in keys {
        // The record's own identity and storage version are the workspace
        // record's, never a window's.
        if *from == ROOT && key.starts_with(FRAMEWORK_KEYS) {
            continue;
        }
        let Some((value, source)) = window.get(from, key.as_str())? else {
            continue;
        };
        let default = defaults.and_then(|(doc, obj)| {
            doc.get(obj, key.as_str())
                .ok()
                .flatten()
                .map(|(value, id)| (doc, value.into_owned(), id))
        });
        match (record.get(into, key.as_str())?, value) {
            (None, value) => copy_into_map(record, into, &key, window, &value, &source)?,
            (Some((Value::Object(have), existing)), Value::Object(kind))
                if same_kind(have, kind) =>
            {
                let defaults = default
                    .as_ref()
                    .filter(|(_, value, _)| matches!(value, Value::Object(_)))
                    .map(|(doc, _, id)| (*doc, id));
                merge_object(record, &existing, kind, window, &source, defaults)?;
            }
            (Some(_), value) => {
                // A value the window changed from what a new record starts
                // with wins; one it left as it was changes nothing.
                let unchanged = default.as_ref().is_some_and(|(doc, default, id)| {
                    equal(doc, default, id, window, &value, &source)
                });
                if !unchanged {
                    copy_into_map(record, into, &key, window, &value, &source)?;
                }
            }
        }
    }
    Ok(())
}

fn merge_object(
    record: &mut Doc,
    into: &ObjId,
    kind: ObjType,
    window: &Doc,
    from: &ObjId,
    defaults: Defaults<'_>,
) -> Result<(), automerge::AutomergeError> {
    match kind {
        ObjType::Map | ObjType::Table => merge_map(record, into, window, from, defaults),
        ObjType::List => merge_list(record, into, window, from),
        ObjType::Text => {
            if record.text(into)?.is_empty() {
                let text = window.text(from)?;
                record.splice_text(into, 0, 0, &text)?;
            }
            Ok(())
        }
    }
}

fn merge_list(
    record: &mut Doc,
    into: &ObjId,
    window: &Doc,
    from: &ObjId,
) -> Result<(), automerge::AutomergeError> {
    for index in 0..window.length(from) {
        let Some((value, source)) = window.get(from, index)? else {
            continue;
        };
        let length = record.length(into);
        // An element that names itself joins the element of that id.
        if let Some(id) = element_id(window, &value, &source) {
            let joined = (0..length).find_map(|at| {
                let (have, existing) = record.get(into, at).ok().flatten()?;
                (element_id(record, &have, &existing).as_deref() == Some(id.as_str()))
                    .then_some(existing)
            });
            if let Some(existing) = joined {
                merge_map(record, &existing, window, &source, None)?;
                continue;
            }
        }
        let present = (0..length).any(|at| {
            record
                .get(into, at)
                .ok()
                .flatten()
                .is_some_and(|(have, existing)| {
                    equal(record, &have, &existing, window, &value, &source)
                })
        });
        if !present {
            insert_into_list(record, into, length, window, &value, &source)?;
        }
    }
    Ok(())
}

/// The `id` a list element that is a map gives itself.
fn element_id(doc: &Doc, value: &Value<'_>, obj: &ObjId) -> Option<String> {
    if !matches!(value, Value::Object(ObjType::Map | ObjType::Table)) {
        return None;
    }
    match doc.get(obj, "id").ok().flatten()? {
        (Value::Scalar(scalar), _) => Some(scalar.to_string()),
        _ => None,
    }
}

fn same_kind(have: ObjType, kind: ObjType) -> bool {
    have == kind
        || matches!(
            (have, kind),
            (ObjType::Map, ObjType::Table) | (ObjType::Table, ObjType::Map)
        )
}

/// Whether two values hold the same, in their documents.
fn equal(
    left_doc: &Doc,
    left: &Value<'_>,
    left_id: &ObjId,
    right_doc: &Doc,
    right: &Value<'_>,
    right_id: &ObjId,
) -> bool {
    hydrate(left_doc, left, left_id) == hydrate(right_doc, right, right_id)
}

fn hydrate(doc: &Doc, value: &Value<'_>, id: &ObjId) -> serde_json::Value {
    match value {
        Value::Scalar(scalar) => serde_json::Value::String(format!("{scalar:?}")),
        Value::Object(ObjType::Text) => serde_json::Value::String(doc.text(id).unwrap_or_default()),
        Value::Object(ObjType::List) => serde_json::Value::Array(
            (0..doc.length(id))
                .filter_map(|at| doc.get(id, at).ok().flatten())
                .map(|(value, child)| hydrate(doc, &value, &child))
                .collect(),
        ),
        Value::Object(ObjType::Map | ObjType::Table) => serde_json::Value::Object(
            doc.keys(id)
                .filter_map(|key| {
                    let (value, child) = doc.get(id, key.as_str()).ok().flatten()?;
                    Some((key, hydrate(doc, &value, &child)))
                })
                .collect(),
        ),
    }
}

fn copy_into_map(
    record: &mut Doc,
    into: &ObjId,
    key: &str,
    window: &Doc,
    value: &Value<'_>,
    source: &ObjId,
) -> Result<(), automerge::AutomergeError> {
    match value {
        Value::Scalar(scalar) => record.put(into, key, scalar.as_ref().clone()),
        Value::Object(kind) => {
            let made = record.put_object(into, key, *kind)?;
            fill(record, &made, *kind, window, source)
        }
    }
}

fn insert_into_list(
    record: &mut Doc,
    into: &ObjId,
    at: usize,
    window: &Doc,
    value: &Value<'_>,
    source: &ObjId,
) -> Result<(), automerge::AutomergeError> {
    match value {
        Value::Scalar(scalar) => record.insert(into, at, scalar.as_ref().clone()),
        Value::Object(kind) => {
            let made = record.insert_object(into, at, *kind)?;
            fill(record, &made, *kind, window, source)
        }
    }
}

/// Fill the new object `made` with what `source` holds.
fn fill(
    record: &mut Doc,
    made: &ObjId,
    kind: ObjType,
    window: &Doc,
    source: &ObjId,
) -> Result<(), automerge::AutomergeError> {
    match kind {
        ObjType::Text => {
            let text = window.text(source)?;
            record.splice_text(made, 0, 0, &text)
        }
        ObjType::List => {
            for at in 0..window.length(source) {
                if let Some((value, child)) = window.get(source, at)? {
                    insert_into_list(record, made, at, window, &value, &child)?;
                }
            }
            Ok(())
        }
        ObjType::Map | ObjType::Table => {
            let keys: Vec<String> = window.keys(source).collect();
            for key in keys {
                if let Some((value, child)) = window.get(source, key.as_str())? {
                    copy_into_map(record, made, &key, window, &value, &child)?;
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window_with_feeds(feeds: &[(&str, &str)], sort: &str) -> Doc {
        let mut doc = Doc::new();
        let list = doc.put_object(ROOT, "feeds", ObjType::List).unwrap();
        for (at, (id, url)) in feeds.iter().enumerate() {
            let feed = doc.insert_object(&list, at, ObjType::Map).unwrap();
            doc.put(&feed, "id", *id).unwrap();
            doc.put(&feed, "url", *url).unwrap();
        }
        doc.put(ROOT, "sort", sort).unwrap();
        doc.put_object(ROOT, "title", ObjType::Text).unwrap();
        doc
    }

    fn stamped_window(feeds: &[(&str, &str)], sort: &str) -> Doc {
        let mut doc = window_with_feeds(feeds, sort);
        let meta = doc.put_object(ROOT, "tilo:record", ObjType::Map).unwrap();
        doc.put(&meta, "id", "derived:default@tab:1").unwrap();
        doc
    }

    fn feeds(doc: &Doc) -> Vec<String> {
        let (_, list) = doc.get(ROOT, "feeds").unwrap().unwrap();
        (0..doc.length(&list))
            .map(|at| {
                let (_, feed) = doc.get(&list, at).unwrap().unwrap();
                let (url, _) = doc.get(&feed, "url").unwrap().unwrap();
                url.to_string()
            })
            .collect()
    }

    /// Two windows' records, each created on its own with a feed of its
    /// own, both reach the one record, ids kept, and a setting a window
    /// changed wins over what a new record starts with.
    #[test]
    fn two_orphaned_window_records_merge_into_one_without_loss() {
        let mut record = window_with_feeds(&[], "newest");
        let windows = vec![
            (
                ui_core::record::RecordId::window_default(1),
                window_with_feeds(&[("a", "https://a.example/feed")], "newest"),
            ),
            (
                ui_core::record::RecordId::window_default(2),
                stamped_window(
                    &[
                        ("a", "https://a.example/feed"),
                        ("b", "https://b.example/feed"),
                    ],
                    "oldest",
                ),
            ),
            (
                ui_core::record::RecordId::window_default(3),
                window_with_feeds(&[], "newest"),
            ),
        ];
        adopt(&mut record, &windows, &|_| Ok(())).unwrap();
        let urls = feeds(&record);
        assert_eq!(urls.len(), 2, "each feed once: {urls:?}");
        assert!(urls.iter().any(|url| url.contains("a.example")));
        assert!(urls.iter().any(|url| url.contains("b.example")));
        let (sort, _) = record.get(ROOT, "sort").unwrap().unwrap();
        assert_eq!(
            sort.to_string(),
            "\"oldest\"",
            "a setting a window changed survives a later window that left it alone"
        );
        assert_eq!(feeds(&windows[1].1).len(), 2);
        assert!(
            record.get(ROOT, "tilo:record").unwrap().is_none(),
            "a window's identity is never the workspace record's"
        );
    }

    #[test]
    fn a_window_record_that_does_not_read_is_left_out() {
        let mut record = window_with_feeds(&[], "newest");
        let windows = vec![(
            ui_core::record::RecordId::window_default(1),
            window_with_feeds(&[("a", "https://a.example/feed")], "newest"),
        )];
        adopt(&mut record, &windows, &|_| Err("not this app's".into())).unwrap();
        assert!(feeds(&record).is_empty());
    }
}
