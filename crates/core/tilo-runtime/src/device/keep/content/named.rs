//! The content a branch of a workspace takes from its source: every content
//! address the source's records name, in any version of them, with what that
//! content is made of ([`super::branch`]).

use std::collections::BTreeSet;

use automerge::{AutoCommit, ObjId, ObjType, ReadDoc as _, ScalarValue, ValueRef};
use tilo_fs::content::Manifest;

/// The length of a content address: BLAKE3, in hex.
const ADDRESS_LEN: usize = 64;

/// A record by its app and id.
type Route = (String, String);

/// Every content address `documents` (records by app and id, each saved
/// whole) name, in any version of them, with what that content is made of:
/// the chunks of each file manifest `read` finds among them. What a branch
/// holding these records takes of its source's content.
///
/// # Errors
///
/// A record that does not load, named with why.
pub(crate) fn named_by(
    documents: &[(Route, Vec<u8>)],
    read: &impl Fn(&str) -> Option<Vec<u8>>,
) -> Result<BTreeSet<String>, String> {
    named_by_matching(documents, &|_| true, read)
}

/// The content named by the selected apps, with the content their manifests name.
pub(super) fn named_by_matching(
    documents: &[(Route, Vec<u8>)],
    include: &impl Fn(&Route) -> bool,
    read: &impl Fn(&str) -> Option<Vec<u8>>,
) -> Result<BTreeSet<String>, String> {
    let mut live = BTreeSet::new();
    for ((app, id), document) in documents.iter().filter(|(route, _)| include(route)) {
        named_in(document, &mut live).map_err(|why| format!("{id} of {app}: {why}"))?;
    }
    made_of(read, &mut live);
    Ok(live)
}

/// Add every content address `record` (a saved Automerge document) names to
/// `live`: in any value its history ever held, and in the text it holds now.
///
/// # Errors
///
/// Why it does not load.
fn named_in(record: &[u8], live: &mut BTreeSet<String>) -> Result<(), String> {
    let mut doc = AutoCommit::load(record).map_err(|error| error.to_string())?;
    for change in doc.get_changes(&[]) {
        // Text typed in one change lies in one run of its values.
        addresses_in(change.raw_bytes(), live);
        for op in change.decode().operations {
            match op.action {
                automerge::legacy::OpType::Put(ScalarValue::Str(text)) => {
                    addresses_in(text.as_bytes(), live);
                }
                automerge::legacy::OpType::Put(ScalarValue::Bytes(bytes)) => {
                    if bytes.len() == ADDRESS_LEN / 2 {
                        live.insert(hex::encode(&bytes));
                    }
                    addresses_in(&bytes, live);
                }
                _ => {}
            }
        }
    }
    // Text is written a character at a time: an address in it is whole only
    // in the text it makes.
    texts_in(&doc, &automerge::ROOT, live);
    Ok(())
}

/// Add the addresses in the text objects under `obj` of `doc` to `live`.
fn texts_in(doc: &AutoCommit, obj: &ObjId, live: &mut BTreeSet<String>) {
    let children: Vec<(ObjType, ObjId)> = match doc.object_type(obj) {
        Ok(ObjType::Map | ObjType::Table) => doc
            .map_range(obj, ..)
            .filter_map(|item| match item.value {
                ValueRef::Object(kind) => Some((kind, item.id())),
                ValueRef::Scalar(_) => None,
            })
            .collect(),
        Ok(ObjType::List) => doc
            .list_range(obj, ..)
            .filter_map(|item| match item.value {
                ValueRef::Object(kind) => Some((kind, item.id())),
                ValueRef::Scalar(_) => None,
            })
            .collect(),
        Ok(ObjType::Text) => {
            if let Ok(text) = doc.text(obj) {
                addresses_in(text.as_bytes(), live);
            }
            return;
        }
        Err(_) => return,
    };
    for (_, child) in children {
        texts_in(doc, &child, live);
    }
}

/// Add each run of exactly [`ADDRESS_LEN`] hex digits in `bytes` to `live`,
/// in lower case; a longer run adds each address it could hold.
pub(crate) fn addresses_in(bytes: &[u8], live: &mut BTreeSet<String>) {
    let mut start = 0;
    while start < bytes.len() {
        let run = bytes
            .get(start..)
            .unwrap_or_default()
            .iter()
            .take_while(|byte| byte.is_ascii_hexdigit())
            .count();
        if run >= ADDRESS_LEN {
            for at in start..=start.saturating_add(run).saturating_sub(ADDRESS_LEN) {
                if let Some(address) = bytes.get(at..at.saturating_add(ADDRESS_LEN)) {
                    live.insert(String::from_utf8_lossy(address).to_ascii_lowercase());
                }
            }
        }
        start = start.saturating_add(run).saturating_add(1);
    }
}

/// Add to `live` what the content it names is made of: the chunks each file
/// manifest `read` finds among it lists.
fn made_of(read: &impl Fn(&str) -> Option<Vec<u8>>, live: &mut BTreeSet<String>) {
    let chunks: Vec<String> = live
        .iter()
        .filter_map(|hash| read(hash))
        .filter_map(|bytes| Manifest::decode(&bytes).ok())
        .flat_map(|manifest| {
            manifest
                .chunks()
                .iter()
                .map(|chunk| hex::encode(chunk.hash))
                .collect::<Vec<_>>()
        })
        .collect();
    live.extend(chunks);
}

#[cfg(test)]
mod tests {
    use automerge::transaction::Transactable as _;
    use automerge::{AutoCommit, ObjType, ROOT};

    use super::*;

    tilo_test_env::scenario! {
        fn a_record_names_only_the_whole_addresses_it_holds;

        phase a_record_names_the_content_any_version_of_it_held;
        {
            let first = "a1".repeat(32);
            let second = "B2".repeat(32);
            let typed = "c3".repeat(32);
            let raw = [7_u8; 32];
            let mut doc = AutoCommit::new();
            doc.put(ROOT, "file", first.as_str()).unwrap();
            doc.commit();
            doc.put(ROOT, "file", format!("tilo.content:{second}"))
                .unwrap();
            doc.put(ROOT, "thumbnail", raw.to_vec()).unwrap();
            let note = doc.put_object(ROOT, "note", ObjType::Text).unwrap();
            doc.commit();
            for (at, character) in typed.chars().enumerate() {
                doc.splice_text(&note, at, 0, &character.to_string())
                    .unwrap();
                doc.commit();
            }
            let mut live = BTreeSet::new();
            named_in(&doc.save(), &mut live).unwrap();
            for address in [first, second.to_ascii_lowercase(), typed, hex::encode(raw)] {
                assert!(live.contains(&address), "{address} is named: {live:?}");
            }
        }

        phase only_whole_addresses_are_names;
        {
            let mut live = BTreeSet::new();
            addresses_in(
                format!("{} and {}", "d4".repeat(31), "e5".repeat(32)).as_bytes(),
                &mut live,
            );
            assert_eq!(live, BTreeSet::from(["e5".repeat(32)]));
        }

        phase a_record_that_does_not_load_names_nothing_and_says_so;
        {
            assert!(named_in(b"not a document", &mut BTreeSet::new()).is_err());
        }
    }
}
