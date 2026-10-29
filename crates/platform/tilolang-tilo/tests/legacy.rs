//! Records written before items were stored by their id, read as items of the flat form.
//!
//! The rooms in `tests/legacy/` (saved Automerge documents, as hex) were written by the code
//! of that time (each app's scenarios run in a room of its own and saved, before items were
//! flat): every item is nested in the collection it was inserted into. Opened now, each item is read by its id, in the collection
//! and place it was in, with what it holds; nothing of the record is rewritten; references
//! written as paths still name their items; and moves, edits and removals of those items work
//! as on any other, keeping every id.

use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, Value as AmValue};
use tilo_resource::collection::{collection_entries, in_stored_order, read_entry};
use tilolang_runtime::meta::FieldKind;
use tilolang_runtime::store::{FieldSpec, FieldValue, Store as _};
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_runtime::{InvokeOutcome, Principal};
use tilolang_tilo::schema::{MergeKind, TypeSchema, lookup};

use crate::fixtures::Replica;

macro_rules! app {
    ($name:literal) => {
        (
            include_str!(concat!("../../../../apps/tl/", $name, "/app.tl")),
            include_str!(concat!("../../../../apps/tl/", $name, "/app.ids")),
        )
    };
}

/// A package's source and ids lock.
type Package = (&'static str, &'static str);

/// Each fixture (the saved room, as hex), the app whose scenarios wrote it and every package it
/// needs.
fn fixtures() -> Vec<(&'static str, Vec<Package>)> {
    vec![
        (include_str!("legacy/todo.hex"), vec![app!("tasks")]),
        (include_str!("legacy/notes.hex"), vec![app!("notes")]),
        // Letters as it was when it wrote its room: it became Mail since, keeping its ids.
        (
            include_str!("legacy/letters.hex"),
            vec![(
                include_str!("legacy/letters/app.tl"),
                include_str!("legacy/letters/app.ids"),
            )],
        ),
        (include_str!("legacy/chat.hex"), vec![app!("chat")]),
        (include_str!("legacy/board.hex"), vec![app!("board")]),
        (
            include_str!("legacy/today.hex"),
            vec![app!("notes"), app!("tasks"), app!("chat"), app!("today")],
        ),
    ]
}

/// One nested item as the old layout holds it: the item holding its collection (`None`: the
/// record), the collection's field, its id and its map.
type Nested = (Option<String>, String, String, Option<ObjId>);

fn map_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(obj, key).ok()? {
        Some((AmValue::Object(ObjType::Map), id)) => Some(id),
        _ => None,
    }
}

/// Every item nested below the map `obj` (of `schema`, the item `holder` or the record), as the
/// old layout holds them: in field order, each collection in its stored order.
fn nested(
    doc: &AutoCommit,
    (obj, schema): (&ObjId, &TypeSchema),
    holder: Option<&str>,
    out: &mut Vec<Nested>,
) {
    for field in schema
        .fields
        .iter()
        .filter(|f| f.kind == MergeKind::Collection)
    {
        let Some(coll) = map_at(doc, obj, &field.id) else {
            continue;
        };
        let mut stored: Vec<_> = collection_entries(doc, &coll)
            .into_iter()
            .filter_map(|entry| read_entry(doc, entry))
            .collect();
        in_stored_order(&mut stored);
        for item in stored {
            let id = item.id.as_str().to_owned();
            let value = item.value.as_ref().and_then(|(value, at)| {
                matches!(value, AmValue::Object(ObjType::Map)).then(|| at.clone())
            });
            out.push((
                holder.map(str::to_owned),
                field.id.clone(),
                id.clone(),
                value.clone(),
            ));
            // An item of a collection of a union's types is of the type it was stored as.
            let item_schema = value
                .as_ref()
                .map(|value| type_of(doc, value))
                .and_then(|ty| lookup(&ty))
                .or_else(|| field.record.as_deref().and_then(lookup));
            if let (Some(value), Some(item_schema)) = (value, item_schema) {
                nested(doc, (&value, item_schema), Some(&id), out);
            }
        }
    }
}

/// The records of the room: app, id and the record's map.
fn records(doc: &AutoCommit) -> Vec<(String, String, ObjId)> {
    let tl = map_at(doc, &automerge::ROOT, "tl").expect("the room holds tilolang records");
    let mut out = Vec::new();
    for app in doc.keys(&tl).collect::<Vec<_>>() {
        let Some(records) = map_at(doc, &tl, &app) else {
            continue;
        };
        for id in doc.keys(&records).collect::<Vec<_>>() {
            if let Some(obj) = map_at(doc, &records, &id) {
                out.push((app.clone(), id, obj));
            }
        }
    }
    out
}

fn type_of(doc: &AutoCommit, obj: &ObjId) -> String {
    match doc.get(obj, "$type").unwrap() {
        Some((AmValue::Scalar(scalar), _)) => scalar.as_str().unwrap().to_owned(),
        other => panic!("{other:?}"),
    }
}

fn listed(replica: &Replica, holder: &ResourceRef, field: &str) -> Vec<String> {
    let spec = FieldSpec {
        id: field.to_owned(),
        kind: FieldKind::Collection,
    };
    match replica.host.store().read(holder, &spec).unwrap() {
        FieldValue::Items(ids) => ids,
        other => panic!("{other:?}"),
    }
}

/// The saved room a fixture holds as hex.
fn saved(hex: &str) -> AutoCommit {
    let digits: Vec<u8> = hex.bytes().filter(u8::is_ascii_hexdigit).collect();
    let bytes: Vec<u8> = digits
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect();
    AutoCommit::load(&bytes).unwrap()
}

/// The room of a fixture, opened with its packages.
fn open(hex: &str, packages: &[(&str, &str)]) -> Replica {
    Replica::new("ada", saved(hex), packages).0
}

#[test]
fn every_item_of_an_older_record_reads_by_its_id_where_it_was_and_nothing_is_rewritten() {
    for (bytes, packages) in fixtures() {
        let mut replica = open(bytes, &packages);
        let heads = replica.doc.get_heads();
        let doc = replica.doc.clone();
        let mut items = 0usize;
        for (_, id, obj) in records(&doc) {
            let ty = type_of(&doc, &obj);
            let schema = lookup(&ty).unwrap_or_else(|| panic!("type {ty}"));
            let mut held = Vec::new();
            nested(&doc, (&obj, schema), None, &mut held);
            items += held.len();
            // Each collection lists the items nested in it, in their order.
            let mut collections: Vec<(Option<String>, String)> = held
                .iter()
                .map(|(holder, field, _, _)| (holder.clone(), field.clone()))
                .collect();
            collections.dedup();
            for (holder, field) in collections {
                let at = match &holder {
                    Some(item) => ResourceRef::root(&id, ty.clone()).item(item, String::new()),
                    None => ResourceRef::root(&id, ty.clone()),
                };
                let expected: Vec<String> = held
                    .iter()
                    .filter(|(h, f, _, _)| *h == holder && *f == field)
                    .map(|(_, _, item, _)| item.clone())
                    .collect();
                assert_eq!(
                    listed(&replica, &at, &field),
                    expected,
                    "{id} {holder:?} {field}"
                );
            }
            // Each item reads what its map holds, by its id.
            for (_, _, item, value) in &held {
                let Some(value) = value else {
                    continue;
                };
                let item_ty = type_of(&doc, value);
                let item_schema = lookup(&item_ty).unwrap();
                let at = ResourceRef::root(&id, ty.clone()).item(item, item_ty.clone());
                assert_eq!(replica.host.store().type_of(&at).unwrap(), item_ty);
                for field in item_schema
                    .fields
                    .iter()
                    .filter(|f| f.kind == MergeKind::Text)
                {
                    let stored: String =
                        tilo_resource::read_key(&doc, value, &field.id).unwrap_or_default();
                    assert_eq!(
                        replica.host.runtime().read(&at, &field.id).unwrap(),
                        Value::Text(stored),
                        "{id}/{item} {}",
                        field.id
                    );
                }
            }
        }
        assert!(items > 0 || packages.len() > 1, "the fixture holds items");
        // Reading changed nothing: the room holds the changes it held, and nothing is queued.
        assert_eq!(replica.doc.get_heads(), heads);
        assert!(replica.host.take_committed().is_empty());
    }
}

/// Invoke `name` of `app` with `args`, as `ada`, and commit it.
fn call(replica: &mut Replica, app: &str, name: &str, args: Vec<Value>) -> Value {
    let meta = replica.host.package(app).unwrap().meta;
    let id = meta
        .actions
        .iter()
        .find(|a| a.name == name)
        .unwrap()
        .id
        .clone();
    let outcome = replica
        .host
        .runtime()
        .invoke_action(&Principal::new("ada"), (id.as_str(), args))
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    replica.commit();
    match outcome {
        InvokeOutcome::Done(value) => value,
        other => panic!("{name}: {other:?}"),
    }
}

/// The index `replica` kept holds what deriving it anew from its record holds.
fn check_index(replica: &Replica, packages: &[(&str, &str)], app: &str) {
    let fresh = Replica::new("fresh", replica.doc.clone().fork(), packages).0;
    assert_eq!(
        replica.host.store().index_contents(app).unwrap(),
        fresh.host.store().index_contents(app).unwrap()
    );
}

/// The root of `app` in `replica`, and the type of its items in the collection `field`.
fn root(replica: &Replica, app: &str) -> ResourceRef {
    let root = replica.host.root_of(app).unwrap();
    assert!(
        replica.host.take_committed().is_empty(),
        "the root was there"
    );
    root
}

fn field(replica: &Replica, app: &str, ty: &str, name: &str) -> (String, String) {
    let meta = replica.host.package(app).unwrap().meta;
    let declared = meta.types.iter().find(|t| t.name == ty).unwrap();
    let field = declared.fields.iter().find(|f| f.name == name).unwrap();
    let item = lookup(&declared.id)
        .and_then(|schema| schema.field(&field.id))
        .and_then(|f| f.record.clone())
        .unwrap_or_default();
    (field.id.clone(), item)
}

#[test]
fn items_of_an_older_record_move_change_and_go_keeping_their_ids() {
    let packages = vec![app!("tasks")];
    let mut replica = open(include_str!("legacy/todo.hex"), &packages);
    let app = "so.tilo.lang.tasks";
    let list = root(&replica, app);
    let (items_field, item_ty) = field(&replica, app, "Tasks", "items");
    let before = listed(&replica, &list, &items_field);
    assert!(before.len() >= 3);
    let item = |id: &str| Value::resource(&list.item(id, item_ty.clone()));
    // The last item, moved after the first.
    let moving = before.last().unwrap().clone();
    call(
        &mut replica,
        app,
        "move",
        vec![Value::resource(&list), item(&moving), item(&before[0])],
    );
    let moved = listed(&replica, &list, &items_field);
    assert_eq!(moved[1], moving, "{moved:?}");
    assert_eq!(moved.len(), before.len());
    check_index(&replica, &packages, app);
    // Checked, and read back.
    call(
        &mut replica,
        app,
        "check",
        vec![item(&moving), Value::Bool(true)],
    );
    assert_eq!(
        replica
            .host
            .runtime()
            .read(&list.item(&moving, item_ty.clone()), "done")
            .unwrap(),
        Value::Bool(true)
    );
    // Removed, and only it.
    call(
        &mut replica,
        app,
        "remove",
        vec![Value::resource(&list), item(&before[0])],
    );
    let after = listed(&replica, &list, &items_field);
    assert_eq!(after.len(), before.len() - 1);
    assert!(!after.contains(&before[0]));
    // A new item goes in beside the older ones.
    call(
        &mut replica,
        app,
        "add",
        vec![Value::resource(&list), Value::text("new")],
    );
    let added = listed(&replica, &list, &items_field);
    assert_eq!(added.len(), before.len());
    assert_eq!(&added[..after.len()], after.as_slice());
    check_index(&replica, &packages, app);
}

#[test]
fn a_message_posted_to_an_older_conversation_follows_its_messages() {
    let packages = vec![app!("chat")];
    let mut replica = open(include_str!("legacy/chat.hex"), &packages);
    let app = "so.tilo.lang.chat";
    let all = root(&replica, app);
    // The Chat of channels reads an older conversation as a text channel.
    let (conversations, _) = field(&replica, app, "Channels", "items");
    let (messages, _) = field(&replica, app, "TextChannel", "messages");
    let conversation_ty = replica
        .host
        .package(app)
        .unwrap()
        .meta
        .types
        .iter()
        .find(|t| t.name == "TextChannel")
        .unwrap()
        .id
        .clone();
    let first = listed(&replica, &all, &conversations)[0].clone();
    let conversation = all.item(&first, conversation_ty);
    let before = listed(&replica, &conversation, &messages);
    assert!(!before.is_empty());
    call(
        &mut replica,
        app,
        "send",
        vec![
            Value::resource(&conversation),
            Value::Rich(tilolang_runtime::rich::RichValue::from_text("still here")),
            Value::none(),
        ],
    );
    let after = listed(&replica, &conversation, &messages);
    assert_eq!(&after[..before.len()], before.as_slice());
    assert_eq!(after.len(), before.len() + 1);
    check_index(&replica, &packages, app);
}

#[test]
fn an_older_boards_cards_move_into_their_columns_by_its_migration_keeping_their_ids() {
    let packages = vec![app!("board")];
    let mut replica = open(include_str!("legacy/board.hex"), &packages);
    let app = "so.tilo.lang.board";
    let board = root(&replica, app);
    let (cards_field, card_ty) = field(&replica, app, "Board", "cards");
    let (columns_field, column_ty) = field(&replica, app, "Board", "columns");
    let (lane_field, _) = field(&replica, app, "Column", "cards");
    let stored = |replica: &Replica| -> Vec<(String, String)> {
        records(&replica.doc)
            .into_iter()
            .map(|(app, id, _)| (app, id))
            .collect()
    };
    let records_before = stored(&replica);
    let before = listed(&replica, &board, &cards_field);
    assert_eq!(before.len(), 9, "the release board's cards");
    let titles = |replica: &Replica, cards: &[String]| -> Vec<Value> {
        cards
            .iter()
            .map(|card| {
                replica
                    .host
                    .runtime()
                    .read(&board.item(card, card_ty.clone()), "title")
                    .unwrap()
            })
            .collect()
    };
    let titles_before = titles(&replica, &before);
    let columns = listed(&replica, &board, &columns_field);
    assert_eq!(columns.len(), 3);
    let lanes = |replica: &Replica| -> Vec<Vec<String>> {
        columns
            .iter()
            .map(|col| listed(replica, &board.item(col, column_ty.clone()), &lane_field))
            .collect()
    };
    assert!(lanes(&replica).iter().all(Vec::is_empty));

    // Opened by the version that keeps cards in their columns, the board is migrated once: one
    // change, the migration's own action.
    assert_eq!(replica.host.migrate(), 1);
    let committed = replica.host.take_committed();
    assert_eq!(committed.len(), 1);
    let meta = replica.host.package(app).unwrap().meta;
    let place_cards = meta
        .actions
        .iter()
        .find(|a| a.name == "place_cards")
        .unwrap();
    assert_eq!(committed[0].action.action_id, place_cards.id);
    // It is the package version's change, not the viewer's: history names Board updating.
    let authorship = &committed[0].action.authorship;
    assert_eq!(
        authorship.invoker,
        tilolang_tilo::authority::migration_principal(app, &meta.module_hash).0
    );
    assert!(
        tilolang_tilo::names::unnamed(&authorship.invoker)
            .starts_with("Board, updating to version "),
        "{}",
        tilolang_tilo::names::unnamed(&authorship.invoker)
    );
    for done in &committed {
        replica.apply(&done.action);
    }

    // Each card is in its column, in the order the board showed them, under the id it had.
    let after = lanes(&replica);
    assert_eq!(
        after.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![4, 2, 3],
        "{after:?}"
    );
    assert_eq!(after.concat(), before);
    assert_eq!(titles(&replica, &after.concat()), titles_before);
    assert!(listed(&replica, &board, &cards_field).is_empty());
    // Nothing was deleted: every record is where it was, and each card still holds the column it
    // was in, which the new version no longer reads.
    assert_eq!(stored(&replica), records_before);
    let column = replica
        .host
        .runtime()
        .read(&board.item(&before[8], card_ty.clone()), "column");
    assert!(matches!(column, Ok(Value::Ref(_))), "{column:?}");
    check_index(&replica, &packages, app);

    // It ran once: neither this replica nor a peer opening the room afterwards runs it again.
    assert_eq!(replica.host.migrate(), 0);
    assert!(replica.host.take_committed().is_empty());
    let peer = Replica::new("peer", replica.doc.clone().fork(), &packages).0;
    assert_eq!(peer.host.migrate(), 0);
    assert_eq!(lanes(&peer), after);

    // The cards work in their columns as any other: one moved to the top of another column.
    let card = |id: &str| Value::resource(&board.item(id, card_ty.clone()));
    let lane = |id: &str| Value::resource(&board.item(id, column_ty.clone()));
    call(
        &mut replica,
        app,
        "move_before",
        vec![lane(&columns[0]), card(&after[2][0]), card(&after[0][0])],
    );
    let moved = lanes(&replica);
    assert_eq!(moved[0][0], after[2][0]);
    assert_eq!(moved[2].len(), 2);
    check_index(&replica, &packages, app);
}

#[test]
fn a_board_the_new_version_made_is_not_migrated() {
    let packages = vec![app!("board")];
    let mut replica = Replica::new("ada", AutoCommit::new(), &packages).0;
    let app = "so.tilo.lang.board";
    let board = replica.host.root_of(app).unwrap();
    replica.commit();
    call(
        &mut replica,
        app,
        "new_column",
        vec![Value::resource(&board)],
    );
    assert_eq!(replica.host.migrate(), 0);
    assert!(replica.host.take_committed().is_empty());
}

#[test]
fn an_older_entry_that_does_not_read_is_set_aside_and_kept() {
    let packages = vec![app!("chat")];
    let bytes = include_str!("legacy/chat.hex");
    // The package's types are known once a host loaded it.
    drop(open(bytes, &packages));
    let mut doc = saved(bytes);
    // A message id held twice: copied into another conversation of the record, as no code
    // writes it but a damaged or hand-made record may hold it.
    let (_, id, obj) = records(&doc)
        .into_iter()
        .find(|(app, _, _)| app == "so.tilo.lang.chat")
        .unwrap();
    let schema = lookup(&type_of(&doc, &obj)).unwrap();
    let mut held = Vec::new();
    nested(&doc, (&obj, schema), None, &mut held);
    let conversations: Vec<&Nested> = held.iter().filter(|(h, _, _, _)| h.is_none()).collect();
    assert!(conversations.len() >= 2);
    let (_, _, first, Some(first_map)) = conversations[0] else {
        panic!("a conversation's map");
    };
    let (_, _, second, Some(second_map)) = conversations[1] else {
        panic!("a conversation's map");
    };
    let (_, messages_field, message, _) = held
        .iter()
        .find(|(h, _, _, _)| h.as_deref() == Some(first.as_str()))
        .unwrap()
        .clone();
    let _ = first_map;
    let coll = map_at(&doc, second_map, &messages_field).unwrap_or_else(|| {
        automerge::transaction::Transactable::put_object(
            &mut doc,
            second_map,
            messages_field.as_str(),
            ObjType::Map,
        )
        .unwrap()
    });
    let entry = automerge::transaction::Transactable::put_object(
        &mut doc,
        &coll,
        message.as_str(),
        ObjType::Map,
    )
    .unwrap();
    automerge::transaction::Transactable::put(&mut doc, &entry, "k", "zz").unwrap();
    doc.commit();
    let replica = Replica::new("ada", doc, &packages).0;
    let all = root(&replica, "so.tilo.lang.chat");
    let conversation_ty = lookup(&type_of(&replica.doc, second_map))
        .unwrap()
        .id
        .clone();
    let of = |conversation: &str| {
        listed(
            &replica,
            &all.item(conversation, conversation_ty.clone()),
            &messages_field,
        )
    };
    assert!(of(first).contains(&message), "the first met is read");
    assert!(
        !of(second).contains(&message),
        "the second is set aside: {:?}",
        of(second)
    );
    // And kept where it is.
    let coll = map_at(&replica.doc, second_map, &messages_field).unwrap();
    assert!(replica.doc.get(&coll, message.as_str()).unwrap().is_some());
    let _ = id;
}

/// Tasks, published as `so.tilo.lang.tasks`, declaring it was `so.tilo.lang.todo` before: the
/// id Todo's records were written under.
fn todo_as_tasks() -> (String, &'static str) {
    let (source, ids) = app!("tasks");
    let header = source.lines().next().unwrap();
    assert!(
        header.starts_with("app so.tilo.lang.tasks ")
            && header.contains("previously: so.tilo.lang.todo"),
        "{header}"
    );
    (source.to_owned(), ids)
}

#[test]
fn a_renamed_app_opens_the_records_of_its_earlier_id_where_they_are() {
    let (source, ids) = todo_as_tasks();
    let packages = vec![(source.as_str(), ids)];
    let mut replica = open(include_str!("legacy/todo.hex"), &packages);
    let heads = replica.doc.get_heads();
    let app = "so.tilo.lang.tasks";
    let stored = |replica: &Replica| -> Vec<(String, String)> {
        records(&replica.doc)
            .into_iter()
            .map(|(app, id, _)| (app, id))
            .collect()
    };
    let before = stored(&replica);
    assert!(
        before.iter().all(|(held, _)| held == "so.tilo.lang.todo"),
        "{before:?}"
    );
    // The root is the one Todo made, and its items read as before, by their ids.
    let list = root(&replica, app);
    assert!(before.iter().any(|(_, id)| *id == list.doc), "{list:?}");
    let (items_field, item_ty) = field(&replica, app, "Tasks", "items");
    let items = listed(&replica, &list, &items_field);
    assert!(items.len() >= 3, "{items:?}");
    // Opening changed nothing in the record.
    assert_eq!(replica.doc.get_heads(), heads);
    assert!(replica.host.take_committed().is_empty());
    // Actions of the renamed app change them where they are, keeping every id.
    let item = |id: &str| Value::resource(&list.item(id, item_ty.clone()));
    call(
        &mut replica,
        app,
        "check",
        vec![item(&items[0]), Value::Bool(true)],
    );
    assert_eq!(
        replica
            .host
            .runtime()
            .read(&list.item(&items[0], item_ty.clone()), "done")
            .unwrap(),
        Value::Bool(true)
    );
    call(
        &mut replica,
        app,
        "move",
        vec![Value::resource(&list), item(&items[2]), item(&items[0])],
    );
    assert_eq!(listed(&replica, &list, &items_field)[1], items[2]);
    call(
        &mut replica,
        app,
        "add",
        vec![Value::resource(&list), Value::text("after the rename")],
    );
    assert_eq!(listed(&replica, &list, &items_field).len(), items.len() + 1);
    // Every record is where it was, under the id it had.
    assert_eq!(stored(&replica), before);
    check_index(&replica, &packages, app);
    // A peer that opens the room afterwards reads the same.
    let fresh = Replica::new("fresh", replica.doc.clone().fork(), &packages).0;
    assert_eq!(
        listed(&fresh, &root(&fresh, app), &items_field),
        listed(&replica, &list, &items_field)
    );
}

#[test]
fn mail_leaves_the_records_letters_wrote_where_they_are() {
    let packages = vec![app!("mail")];
    let mut replica = open(include_str!("legacy/letters.hex"), &packages);
    let heads = replica.doc.get_heads();
    let stored = |replica: &Replica| -> Vec<(String, String)> {
        records(&replica.doc)
            .into_iter()
            .map(|(app, id, _)| (app, id))
            .collect()
    };
    let before = stored(&replica);
    assert!(
        !before.is_empty()
            && before
                .iter()
                .all(|(held, _)| held == "so.tilo.lang.letters"),
        "{before:?}"
    );
    // Mail never was Letters: Letters' records are not Mail's, and opening the room beside them
    // changes nothing of them.
    let mail = replica
        .host
        .store()
        .index_contents("so.tilo.lang.mail")
        .expect("Mail's index");
    for (_, id) in &before {
        assert!(!mail.docs.contains_key(id), "{id} is not Mail's");
    }
    assert_eq!(replica.doc.get_heads(), heads);
    assert!(replica.host.take_committed().is_empty());
    assert_eq!(stored(&replica), before);
}
