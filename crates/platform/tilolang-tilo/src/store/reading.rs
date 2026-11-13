//! How a record's documents are read: the resources it holds, the fields a change reached, and
//! an index built from it whole.

use super::*;
use crate::chunk::{Chunks, Stamps};

/// Every resource of `doc`, with its app and type id. Every copy of the levels above the
/// resources is read ([`crate::containers::reach`]): a record a replica wrote before it met the
/// others is in the map the document does not keep, and it is the record all the same.
pub(super) fn scan(doc: &Doc) -> BTreeMap<DocId, (AppId, TypeId)> {
    let mut out = BTreeMap::new();
    for records in crate::containers::maps(doc, &automerge::ROOT, RECORDS_KEY) {
        for stored in doc.keys(&records) {
            // Records stored under an id their app had before (`previously:`) are that app's.
            let app = crate::schema::successor(&stored);
            let own = app == stored;
            for app_obj in crate::containers::maps(doc, &records, &stored) {
                for id in doc.keys(&app_obj) {
                    let Ok(Some((AmValue::Object(ObjType::Map), obj))) =
                        doc.get(&app_obj, id.as_str())
                    else {
                        continue;
                    };
                    if let Ok(Some((AmValue::Scalar(scalar), _))) = doc.get(&obj, TYPE_KEY)
                        && let ScalarValue::Str(ty) = scalar.as_ref()
                        && (own || !out.contains_key(&id))
                    {
                        out.insert(id, (app.clone(), ty.to_string()));
                    }
                }
            }
        }
    }
    out
}

pub(super) fn json_of_scalar(scalar: &Scalar) -> Json {
    match scalar {
        Scalar::Bool(flag) => Json::Bool(*flag),
        Scalar::Int(value) => Json::from(*value),
        Scalar::Float(value) => {
            serde_json::Number::from_f64(*value).map_or(Json::Null, Json::Number)
        }
        Scalar::Text(text) => Json::String(text.clone()),
        Scalar::Json(value) => value.clone(),
    }
}

/// The path from a record's map to the field `field` of a resource of it, whose own map the
/// path `prefix` reaches ([`Inner::prefix_of`]).
pub(super) fn path_to(prefix: &[Segment], field: &str) -> Vec<Segment> {
    let mut path = prefix.to_vec();
    path.push(Segment::Field(field.to_owned()));
    path
}

/// Have the mirror's writes carry the actor of a change the mirror already holds.
///
/// Under an actor of its own, each of the mirror's transactions would add that actor to the
/// document and its rollback remove it again, and Automerge renumbers every op's actor column
/// for each: two passes over the whole document per keystroke. The mirror never commits (its
/// writes are taken back before any change of the record is applied, see
/// [`TiloStore::sync`]), so its ops under a borrowed actor never meet that actor's next
/// real change.
pub(super) fn write_as_a_known_actor(mirror: &mut Doc) {
    let actor = mirror
        .committed_document()
        .get_heads()
        .first()
        .and_then(|head| mirror.committed_document().get_change_meta_by_hash(head))
        .map(|meta| meta.actor.into_owned());
    if let Some(actor) = actor {
        mirror.set_actor(actor);
    }
}

/// A copy of `record`'s committed changes, to write into.
pub(super) fn mirror_of(record: &Doc) -> Doc {
    let mut mirror = record.clone();
    // Only the record's committed changes are the mirror's: an open transaction of the
    // record arrives as a change once the framework commits it.
    mirror.rollback();
    write_as_a_known_actor(&mut mirror);
    mirror
}

/// The fields of `resource` its versions changed, as those who read them name them
/// ([`Inner::provisional_versions`] writes them into the index): the version that made it and
/// the one that last changed it ([`tilolang_runtime::store::Reflected::CREATED`],
/// [`tilolang_runtime::store::Reflected::UPDATED`]), when each field `end` changed last was
/// changed (`changed(r, .field)`), and its versions, which marks and history read.
pub(super) fn version_fields(resource: &str, end: &idx::reflect::Ends) -> Vec<FieldKey> {
    let mut fields = vec![
        FieldKey {
            resource: resource.to_owned(),
            field: VERSIONS.to_owned(),
        },
        FieldKey {
            resource: resource.to_owned(),
            field: idx::reflect::CREATED.to_owned(),
        },
        FieldKey {
            resource: resource.to_owned(),
            field: idx::reflect::UPDATED.to_owned(),
        },
    ];
    for field in end.fields.keys() {
        fields.push(FieldKey {
            resource: resource.to_owned(),
            field: idx::reflect::changed_field(field),
        });
    }
    fields
}

/// The fields of the versions `ends` the record's changes made that read differently from the
/// ones `told`, which the store's own writes of those changes told when their actions committed
/// ([`Inner::provisional_versions`]): the versions themselves, which name the change that made
/// each now, and each end of a resource a different invocation made — the time a field was last
/// changed that the record says changed and the provisional versions did not name, say. The
/// rest reads as told: the record's versions carry the invocation's own time.
pub(super) fn told_differently(ends: &Ends, told: &Ends) -> BTreeMap<DocId, Vec<FieldKey>> {
    let mut docs = BTreeMap::new();
    for (resource, end) in ends {
        let key = |field: String| FieldKey {
            resource: resource.clone(),
            field,
        };
        let keys = match told.get(resource) {
            Some(was) if was.same_invocation(end) && was.first.same_invocation(&end.first) => {
                let mut keys = vec![key(VERSIONS.to_owned())];
                keys.extend(
                    end.fields
                        .iter()
                        .filter(|(field, stamp)| {
                            !was.fields
                                .get(*field)
                                .is_some_and(|held| held.same_invocation(stamp))
                        })
                        .map(|(field, _)| key(idx::reflect::changed_field(field))),
                );
                keys
            }
            _ => version_fields(resource, end),
        };
        note_derived(&mut docs, doc_of(resource), keys);
    }
    docs
}

/// Name each resource in `ends` as changed in its versions ([`version_fields`]): the record's
/// changes made them, and a change reaches the views of a resource even when the values it
/// writes read as the ones the store already held, as they do when the steps were written
/// locally first. A document the change reached whole names no field and is left as it is:
/// every view of it reads the document anew.
pub(super) fn name_version_changes(ends: &Ends, docs: &mut BTreeMap<DocId, Vec<FieldKey>>) {
    for (resource, end) in ends {
        note_derived(docs, doc_of(resource), version_fields(resource, end));
    }
}

/// Note what a change named in `doc`: the fields that changed there, which sets aside the
/// listings of it reading one. A document the change reached as a whole keeps the empty list
/// that says so: narrower fields would leave the listings they do not name on the old index.
pub(super) fn note_derived(
    docs: &mut BTreeMap<DocId, Vec<FieldKey>>,
    doc: &str,
    keys: Vec<FieldKey>,
) {
    match docs.get_mut(doc) {
        Some(fields) if fields.is_empty() => {}
        Some(fields) => fields.extend(keys),
        None => {
            docs.insert(doc.to_owned(), keys);
        }
    }
}

/// The collections whose items joined, left or moved, by the document that holds them: what a
/// reader of item text reads the steps of, or everything when there are none
/// ([`tilolang_runtime::store::ChangeSummary::unsaid`]).
pub(super) fn collections_by_doc<'a>(
    keys: impl IntoIterator<Item = &'a FieldKey>,
) -> BTreeMap<DocId, Vec<FieldKey>> {
    let mut collections: BTreeMap<DocId, Vec<FieldKey>> = BTreeMap::new();
    for key in keys {
        collections
            .entry(key.doc().to_owned())
            .or_default()
            .push(key.clone());
    }
    collections
}

/// Every record of `app` in `docs`, reached as a whole, with those of `before` that are gone:
/// what derives a package's index anew where it stands.
pub(super) fn everything(
    before: &BTreeMap<DocId, (AppId, TypeId)>,
    now: &BTreeMap<DocId, (AppId, TypeId)>,
) -> Reached {
    let mut reached = Reached::default();
    for doc in before.keys().chain(now.keys()) {
        reached.docs.insert(doc.clone(), Vec::new());
    }
    reached
}

/// Build the index of `app` from the documents `chunks` whole, stamped `heads`.
pub(super) fn build(
    index: &Index,
    chunks: &Chunks,
    docs: &BTreeMap<DocId, (AppId, TypeId)>,
    app: &str,
    heads: &Stamps,
    schemas: &Arc<crate::schema::Schemas>,
    clock: crate::index::Clock,
) -> Result<DocReads, IndexError> {
    // This build may run on a thread of its own, where no store installed its table: the
    // record's types resolve by the store's package versions, never by another store's.
    let _scope = schemas.enter();
    let mut ends = Ends::new();
    // The items that left their collections, which a list shows struck: read off the whole log
    // here, where the index is built off the frame, so a list answers them from the index.
    let mut versions = super::versions::Versions::default();
    let mut removed = crate::index::derive::Removed::new();
    for (_, doc) in chunks.docs() {
        let changes = doc.committed_document().get_changes(&[]);
        // What wrote a resource: a run's tail in the manifest is written once an item, and a
        // change that writes nothing else names no resource and makes no version of one.
        let naming = crate::reads::naming(&changes, doc);
        ends.extend(idx::reflect::ends_of(naming.iter().copied(), doc, docs));
        removed = super::versions::note_versions(&mut versions, naming, doc, docs);
    }
    let nested = Nested::default();
    index.write(|writer| {
        let mut derive = Derive::new(chunks, docs, &nested, clock);
        derive.all_with_versions(writer, app, &ends, &removed)?;
        writer.set_heads(heads)?;
        Ok(derive.reads)
    })
}
