//! Real HostContext/Notes component discovery and creation timing.
//! These are component/door tests, not signed RPC or native durable-promotion proof.
use crate::component_support::*;
use notes::actions::{self, NotesAction};
use notes::doc::NotesDoc;
use std::collections::BTreeSet;
use tilo_actions::{ActionAddress, ActionInput, EncodedAction, Resource};
use ui::record::RecordCheckout;
use uuid::Uuid;

fn snapshot(f: &NotebookFixture, checkout: &RecordCheckout) -> Vec<u8> {
    f.records
        .read(&checkout.id, |doc| doc.clone().save())
        .unwrap()
}

fn address(f: &NotebookFixture, duplicate: bool) -> ActionAddress {
    if duplicate {
        ActionAddress::new(actions::page_resource_id(f.root), "duplicate")
    } else {
        ActionAddress::new("notes.notebook", "daily-note")
    }
}

fn discovery(duplicate: bool) {
    let f = NotebookFixture::rich();
    let checkout = notebook(&f);
    let mut live = driver(&f, 11, &checkout);
    let before = snapshot(&f, &checkout);
    let target = address(&f, duplicate);
    let resources = live.resources();
    let spec = tilo_actions::door::resolve(&resources, &target).unwrap();
    if let ActionInput::Immediate(encoded) = &spec.input {
        let action: NotesAction = serde_json::from_slice(&encoded.payload).unwrap();
        let preallocated = match &action {
            NotesAction::DuplicateSubtree { pages, .. } => !pages.is_empty(),
            NotesAction::UpsertPage(page) => !page.id.is_empty(),
            _ => false,
        };
        assert!(
            !preallocated,
            "discovery published creation destination IDs before activation: {action:?}"
        );
    }
    assert_eq!(snapshot(&f, &checkout), before);
    close_driver(&mut live);
}

// A resource projection must not author a future creation payload.
#[test]
fn notes_continuation_duplicate_discovery_does_not_publish_preallocated_destinations() {
    discovery(true);
}
#[test]
fn notes_continuation_daily_discovery_does_not_publish_preallocated_destination() {
    discovery(false);
}

fn destination_ids(
    encoded: &EncodedAction,
    source: Uuid,
    duplicate: bool,
    daily_order: &str,
) -> BTreeSet<Uuid> {
    assert_eq!(encoded.type_id, actions::NOTES_ACTION_TYPE);
    let action: NotesAction = serde_json::from_slice(&encoded.payload).unwrap();
    if duplicate {
        let NotesAction::DuplicateSubtree {
            source: actual,
            pages,
        } = action
        else {
            panic!("expected duplicate action");
        };
        assert_eq!(actual, source.to_string());
        assert_eq!(pages.len(), 2, "the real fixture source has one child");
        let mut ids = BTreeSet::new();
        for page in pages {
            assert!(ids.insert(page.destination.parse().unwrap()));
        }
        assert_eq!(ids.len(), 2, "every copied page receives an id of its own");
        ids
    } else {
        let NotesAction::UpsertPage(page) = action else {
            panic!("expected creation of the missing daily note");
        };
        assert!(page.parent.is_empty());
        assert!(chrono::NaiveDate::parse_from_str(&page.title, "%Y-%m-%d").is_ok());
        assert_eq!(
            page.order, daily_order,
            "daily creation keeps its published fractional key"
        );
        BTreeSet::from([page.id.parse().unwrap()])
    }
}

fn encode_retained(resources: &[Resource], target: &ActionAddress, json: bool) -> EncodedAction {
    if json {
        tilo_actions::door::encode_json(resources, target, &serde_json::Map::new()).unwrap()
    } else {
        tilo_actions::door::encode(resources, target, None).unwrap()
    }
}

fn retained_declaration(duplicate: bool, json: bool) {
    let f = NotebookFixture::rich();
    let checkout = notebook(&f);
    let mut live = driver(&f, 11, &checkout);
    let before = snapshot(&f, &checkout);
    let target = address(&f, duplicate);
    // Keep the same actual declaration. Rebuilding it between calls would mask
    // the current bug by minting replacement IDs during discovery again.
    let resources = live.resources();
    let daily_order = f
        .records
        .read(&checkout.id, |doc| {
            let pages = NotesDoc::list_pages(doc);
            notes::model::key_after(pages.iter().filter(|page| page.parent.is_none()))
        })
        .unwrap();
    let first = destination_ids(
        &encode_retained(&resources, &target, json),
        f.root,
        duplicate,
        &daily_order,
    );
    let second = destination_ids(
        &encode_retained(&resources, &target, json),
        f.root,
        duplicate,
        &daily_order,
    );
    assert!(
        first.is_disjoint(&second),
        "distinct final door encodings reused creation IDs: {first:?} / {second:?}"
    );
    assert_eq!(
        snapshot(&f, &checkout),
        before,
        "encoding must not apply a mutation"
    );
    close_driver(&mut live);
}

// Both public ActionInput and private typed builder are actual execution lanes.
#[test]
fn notes_continuation_duplicate_retained_legacy_door_mints_fresh_ids() {
    retained_declaration(true, false);
}
#[test]
fn notes_continuation_duplicate_retained_json_door_mints_fresh_ids() {
    retained_declaration(true, true);
}
#[test]
fn notes_continuation_daily_retained_legacy_door_mints_fresh_ids() {
    retained_declaration(false, false);
}
#[test]
fn notes_continuation_daily_retained_json_door_mints_fresh_ids() {
    retained_declaration(false, true);
}

#[test]
fn notes_continuation_component_discovery_and_model_pass_are_read_only() {
    let f = NotebookFixture::rich();
    let checkout = notebook(&f);
    let mut live = driver(&f, 11, &checkout);
    let before = snapshot(&f, &checkout);
    let history = revision(&f, &checkout);
    let declared = tilo_actions::door::survey(&live.resources());
    assert!(declared.iter().any(|row| row.address == address(&f, true)));
    assert!(declared.iter().any(|row| row.address == address(&f, false)));
    for _ in 0..3 {
        live.relayout(); // Real FrameHarness update/sync/owned component lifecycle.
        assert_eq!(tilo_actions::door::survey(&live.resources()), declared);
        assert_eq!(snapshot(&f, &checkout), before);
        assert_eq!(revision(&f, &checkout), history);
    }
    close_driver(&mut live);
}

fn page_ids(f: &NotebookFixture, checkout: &RecordCheckout) -> BTreeSet<Uuid> {
    f.records
        .read(&checkout.id, |doc| {
            NotesDoc::list_pages(doc)
                .into_iter()
                .map(|page| page.id)
                .collect()
        })
        .unwrap()
}

#[test]
fn notes_continuation_real_duplicate_activations_create_distinct_owned_subtrees() {
    let f = NotebookFixture::rich();
    let checkout = notebook(&f);
    let mut live = driver(&f, 11, &checkout);
    let before = page_ids(&f, &checkout);
    invoke(
        &mut live,
        11,
        actions::page_resource_id(f.root),
        "duplicate",
        None,
    );
    let first: BTreeSet<_> = page_ids(&f, &checkout)
        .difference(&before)
        .copied()
        .collect();
    assert_eq!(first.len(), 2);
    let middle = page_ids(&f, &checkout);
    invoke(
        &mut live,
        11,
        actions::page_resource_id(f.root),
        "duplicate",
        None,
    );
    let second: BTreeSet<_> = page_ids(&f, &checkout)
        .difference(&middle)
        .copied()
        .collect();
    assert_eq!(second.len(), 2);
    assert!(first.is_disjoint(&second));
    assert_eq!(
        revision(&f, &checkout),
        2,
        "both actual mutations cross owned history"
    );
    close_driver(&mut live);
}

#[test]
fn notes_continuation_real_daily_activation_creates_once_then_opens_existing_note() {
    let f = NotebookFixture::rich();
    let checkout = notebook(&f);
    let mut live = driver(&f, 11, &checkout);
    let before = page_ids(&f, &checkout);
    invoke(&mut live, 11, "notes.notebook", "daily-note", None);
    let created = page_ids(&f, &checkout);
    assert_eq!(created.difference(&before).count(), 1);
    assert_eq!(revision(&f, &checkout), 1);
    invoke(&mut live, 11, "notes.notebook", "daily-note", None);
    assert_eq!(page_ids(&f, &checkout), created);
    assert_eq!(
        revision(&f, &checkout),
        1,
        "existing daily note is a view operation"
    );
    close_driver(&mut live);
}

#[test]
fn notes_continuation_dynamic_declarations_identify_actual_resolution_domains() {
    let f = NotebookFixture::rich();
    let checkout = notebook(&f);
    let mut live = driver(&f, 11, &checkout);
    let resources = live.resources();
    for row in tilo_actions::door::survey(&resources) {
        // The notebook's own door (its declarations, `apply`, history) is the
        // framework's; Notes captures the domains of the children it builds.
        if !row.address.resource.starts_with("notes.") || row.address.resource == "notes.notebook" {
            continue;
        }
        let spec = tilo_actions::door::resolve(&resources, &row.address).unwrap();
        if spec.fixed_invocation_encoding().is_none() {
            assert!(
                matches!(
                    spec.continuation_identity(tilo_actions::ContinuationQuery::Draft),
                    Some(tilo_actions::ContinuationEvidence::Draft { .. })
                ),
                "missing real captured domain: {:?}",
                row.address
            );
        }
    }
    close_driver(&mut live);
}

#[test]
fn notes_continuation_actual_notes_cover_lazy_invocation_domains() {
    let f = NotebookFixture::rich();
    let checkout = notebook(&f);
    let doc = f.records.read(&checkout.id, Clone::clone).unwrap();
    let app = notes::NotesApp::from_document(&doc, &f.author).unwrap();
    assert!(
        matches!(
            ui::resources::action_directory_invocation_domain(&app, &doc),
            ui::widget::ActionDirectoryInvocationDomain::Versioned(_)
        ),
        "actual Notes has omitted lazy page/block/tag actions"
    );
}
