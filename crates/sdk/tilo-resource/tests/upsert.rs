//! Items addressed by what they hold (`upsert-item`) and places named by a
//! neighbour (`at`), in every kind of collection.
#![allow(clippy::unwrap_used, reason = "test assertions")]

use serde_json::json;
use tilo_resource::{
    At, Collection, Edit, EditOp, Id, Identified, Invoke, Keyed, Model, Replica, resource,
};

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Guest {
    #[model(label)]
    pub name: String,
    pub team: String,
    pub seats: i64,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Badge {
    pub id: String,
    pub holder: String,
    pub level: i64,
}

impl Identified for Badge {
    fn id(&self) -> Id {
        Id::from_raw(self.id.clone())
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Party {
    pub guests: Collection<Guest>,
    pub badges: Keyed<Badge>,
}

#[resource(kind = "test.party", label = "Party")]
impl Party {}

fn upsert(matching: &serde_json::Value, value: serde_json::Value) -> Edit<Party> {
    Party::paths()
        .guests
        .upsert_by(matching.as_object().unwrap().clone(), value)
}

fn apply(replica: &mut Replica<Party>, edit: &Edit<Party>) -> Result<(), String> {
    replica
        .apply(Invoke::edit(edit).map_err(|error| error.to_string())?)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn guests(replica: &Replica<Party>) -> Vec<(String, String, i64)> {
    replica
        .state()
        .unwrap()
        .guests
        .values()
        .map(|guest| (guest.name.clone(), guest.team.clone(), guest.seats))
        .collect()
}

fn guest(name: &str, team: &str, seats: i64) -> (String, String, i64) {
    (name.to_owned(), team.to_owned(), seats)
}

#[test]
fn an_upsert_updates_the_one_item_that_matches_else_appends() {
    let mut party = Replica::<Party>::new();
    apply(
        &mut party,
        &upsert(&json!({"name": "Ana"}), json!({"team": "Ops", "seats": 2})),
    )
    .unwrap();
    apply(
        &mut party,
        &upsert(&json!({"name": "Ben"}), json!({"team": "Web", "seats": 1})),
    )
    .unwrap();
    assert_eq!(
        guests(&party),
        [guest("Ana", "Ops", 2), guest("Ben", "Web", 1)]
    );
    // Matched folded for case and spacing, numbers by value; fields left
    // out keep what they hold.
    apply(
        &mut party,
        &upsert(&json!({"name": " ana "}), json!({"seats": 3})),
    )
    .unwrap();
    apply(
        &mut party,
        &upsert(&json!({"seats": "1.0"}), json!({"team": "Design"})),
    )
    .unwrap();
    assert_eq!(
        guests(&party),
        [guest("Ana", "Ops", 3), guest("Ben", "Design", 1)]
    );
}

#[test]
fn an_upsert_run_again_changes_nothing() {
    let mut party = Replica::<Party>::new();
    let edit = upsert(&json!({"name": "Ana"}), json!({"team": "Ops", "seats": 2}));
    apply(&mut party, &edit).unwrap();
    let heads = party.doc_mut().get_heads();
    let before = guests(&party);
    apply(&mut party, &edit).unwrap();
    assert_eq!(guests(&party), before);
    let changed = party.doc_mut().get_changes(&heads);
    let ops: usize = changed.iter().map(automerge::Change::len).sum();
    assert_eq!(ops, 0, "a second identical upsert writes no field");
}

#[test]
fn an_upsert_matching_two_items_is_refused_naming_them() {
    let mut party = Replica::<Party>::new();
    apply(
        &mut party,
        &upsert(&json!({"name": "Ana"}), json!({"team": "Ops"})),
    )
    .unwrap();
    apply(
        &mut party,
        &upsert(&json!({"name": "Ben"}), json!({"team": "Ops"})),
    )
    .unwrap();
    let refused = apply(
        &mut party,
        &upsert(&json!({"team": "ops"}), json!({"seats": 9})),
    )
    .unwrap_err();
    assert!(refused.contains("fits items"), "{refused}");
    assert!(guests(&party).iter().all(|(_, _, seats)| *seats == 0));
}

#[test]
fn upserts_of_different_keys_from_two_replicas_both_land() {
    let mut party = Replica::<Party>::new();
    apply(
        &mut party,
        &upsert(&json!({"name": "Ana"}), json!({"seats": 1})),
    )
    .unwrap();
    let mut other = party.fork();
    apply(
        &mut party,
        &upsert(&json!({"name": "Ben"}), json!({"seats": 2})),
    )
    .unwrap();
    apply(
        &mut other,
        &upsert(&json!({"name": "Cy"}), json!({"seats": 3})),
    )
    .unwrap();
    apply(
        &mut other,
        &upsert(&json!({"name": "Ana"}), json!({"team": "Ops"})),
    )
    .unwrap();
    party.sync_with(&mut other).unwrap();
    let mut merged = guests(&party);
    merged.sort();
    assert_eq!(
        merged,
        [
            guest("Ana", "Ops", 1),
            guest("Ben", "", 2),
            guest("Cy", "", 3)
        ]
    );
    assert_eq!(guests(&party), guests(&other));
}

#[test]
fn one_key_upserted_on_two_replicas_at_once_is_one_item() {
    let mut party = Replica::<Party>::new();
    let mut other = party.fork();
    apply(
        &mut party,
        &upsert(&json!({"name": "Dee"}), json!({"seats": 1})),
    )
    .unwrap();
    apply(
        &mut other,
        &upsert(&json!({"name": "dee"}), json!({"seats": 1})),
    )
    .unwrap();
    party.sync_with(&mut other).unwrap();
    assert_eq!(guests(&party).len(), 1);
}

#[test]
fn a_keyed_collection_upserts_by_its_fields_too() {
    let mut party = Replica::<Party>::new();
    let badges = Party::paths().badges;
    let edit = |matching: serde_json::Value, value: serde_json::Value| {
        badges.edit(EditOp::UpsertItem {
            matching: matching.as_object().unwrap().clone(),
            value,
            id: None,
        })
    };
    apply(
        &mut party,
        &edit(json!({"holder": "Ana"}), json!({"id": "b1", "level": 1})),
    )
    .unwrap();
    apply(
        &mut party,
        &edit(json!({"holder": "ANA"}), json!({"level": 4})),
    )
    .unwrap();
    let state = party.state().unwrap();
    let badge = state.badges.iter().find(|badge| badge.id == "b1").unwrap();
    assert_eq!((badge.holder.as_str(), badge.level), ("Ana", 4));
    assert_eq!(state.badges.len(), 1);
}

#[test]
fn insert_and_move_place_an_item_beside_another_or_at_the_end() {
    let mut party = Replica::<Party>::new();
    let paths = Party::paths().guests;
    let named = |name: &str| json!({"name": name, "team": "", "seats": 0});
    let insert = |id: &str, at: At, name: &str| {
        paths.edit(EditOp::InsertItem {
            id: Some(Id::from_raw(id)),
            at,
            value: named(name),
        })
    };
    apply(&mut party, &insert("a", At::End, "a")).unwrap();
    apply(&mut party, &insert("c", At::End, "c")).unwrap();
    apply(&mut party, &insert("b", At::After(Id::from_raw("a")), "b")).unwrap();
    apply(&mut party, &insert("z", At::Before(Id::from_raw("a")), "z")).unwrap();
    let order = |party: &Replica<Party>| {
        guests(party)
            .into_iter()
            .map(|(name, _, _)| name)
            .collect::<Vec<_>>()
    };
    assert_eq!(order(&party), ["z", "a", "b", "c"]);
    let wire: EditOp =
        serde_json::from_value(json!({"op": "move-to", "id": "z", "at": "end"})).unwrap();
    apply(&mut party, &paths.edit(wire)).unwrap();
    let wire: EditOp =
        serde_json::from_value(json!({"op": "move-to", "id": "c", "at": {"before": "a"}})).unwrap();
    apply(&mut party, &paths.edit(wire)).unwrap();
    assert_eq!(order(&party), ["c", "a", "b", "z"]);
    // A numeric position still works, and one naming no item is refused.
    apply(&mut party, &paths.move_to(&Id::from_raw("z"), 0)).unwrap();
    assert_eq!(order(&party), ["z", "c", "a", "b"]);
    let refused = apply(
        &mut party,
        &insert("q", At::After(Id::from_raw("gone")), "q"),
    )
    .unwrap_err();
    assert!(refused.contains("gone"), "{refused}");
}

#[test]
fn an_agent_upserts_through_the_published_field_edit_alone() {
    let mut party = Replica::<Party>::new();
    let published = party.published().unwrap();
    let spec = published
        .actions
        .iter()
        .find(|spec| spec.id == tilo_resource::FIELD_EDIT)
        .unwrap();
    let schema = spec.params()[0].schema.as_deref().unwrap().clone();
    assert!(schema.to_string().contains("upsert-item"));
    let edit = json!({
        "path": [{"field": "guests"}],
        "op": "upsert-item",
        "match": {"name": "Ana"},
        "value": {"team": "Ops", "seats": 2},
    });
    tilo_actions::validate_json_schema(&edit, &schema).expect("the upsert fits the schema");
    let edit: Edit<Party> = serde_json::from_value(edit).unwrap();
    party.apply(edit).unwrap();
    assert_eq!(guests(&party), [guest("Ana", "Ops", 2)]);
}
