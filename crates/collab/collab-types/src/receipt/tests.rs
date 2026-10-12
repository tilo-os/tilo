use automerge::transaction::{CommitOptions, Transactable};
use automerge::{AutoCommit, ROOT};
use tilo_action_policy::{Answer, Context, Grant, KindSet, Party, Subject, roles};
use tilo_actions::{ActionContext, ActionKey, Did, EditOp, FieldEdit, VerbCall};

use super::*;
use crate::claim::{claims_of, parse_changes};

const ROOM: &str = "room-1";
const APP: &str = "so.tilo.notes";
const KIND: &str = "notes.page";

fn record() -> Record<'static> {
    Record {
        room: ROOM,
        app: APP,
        record: "page-1",
    }
}

/// The package surface of a page with a body, a unit of comments and
/// a delete verb on each comment.
struct Page;

impl ReceiptSurface for Page {
    fn kind(&self) -> &str {
        KIND
    }
    fn verb(&self, action: &str) -> Option<(ActionKind, Option<String>)> {
        match action {
            "add-comment" | "resolve-comment" => {
                Some((ActionKind::Undoable, Some(roles::COMMENTS_FIELD.to_owned())))
            }
            "rename" => Some((ActionKind::Undoable, None)),
            "publish" => Some((ActionKind::Outside, None)),
            _ => None,
        }
    }
    fn editable(&self, path: &[PathStep]) -> bool {
        matches!(path.first(), Some(PathStep::Field(field)) if field == "body" || field == "title")
    }
    fn footprint(&self, action: &ClaimedAction) -> Vec<UnitPath> {
        match action {
            ClaimedAction::Verb { action, item } if action.action.ends_with("-comment") => {
                match item {
                    Some(item) => vec![comment(item)],
                    None => vec![vec![PathStep::Field("comments".into())]],
                }
            }
            _ => vec![Vec::new()],
        }
    }
}

fn comment(id: &str) -> UnitPath {
    vec![
        PathStep::Field("comments".into()),
        PathStep::Item(id.into()),
    ]
}

fn verb(action: &str, item: Option<&str>) -> ClaimedAction {
    ClaimedAction::Verb {
        action: ActionKey::new(APP, KIND, action),
        item: item.map(str::to_owned),
    }
}

fn edit(field: &str) -> ClaimedAction {
    ClaimedAction::Edit {
        kind: KIND.into(),
        path: vec![PathStep::Field(field.into())],
    }
}

fn claim(byte: u8, action: ClaimedAction, units: Vec<UnitPath>) -> ChangeClaim {
    ChangeClaim {
        change: [byte; 32],
        action,
        units,
    }
}

/// Ada edits the room; Cleo only comments.
fn grants() -> Vec<Grant> {
    let room = Subject::person("room-owner");
    vec![
        Grant::new(
            "ada-edits",
            Subject::person("ada"),
            TreePath::room(ROOM),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            room.clone(),
        ),
        Grant::new(
            "cleo-comments",
            Subject::person("cleo"),
            roles::comments(ROOM),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            room,
        ),
    ]
}

fn decide(person: &str, claims: &[ChangeClaim]) -> Result<Receipt, ReceiptRefusal> {
    let grants = grants();
    let party = Party::new(vec![Subject::person(person)], grants.iter());
    let allows = |node: &TreePath, kind: ActionKind| {
        party.decide(node, kind, &Context::at(0)).answer == Answer::Allow
    };
    check_claims(claims, &record(), None, None, Some(&Page), &allows)
}

#[test]
fn the_relay_refuses_an_action_the_author_is_not_granted() {
    let rename = [claim(1, verb("rename", None), vec![Vec::new()])];
    assert_eq!(decide("ada", &rename), Ok(Receipt::Admit));
    assert!(matches!(
        decide("cleo", &rename),
        Err(ReceiptRefusal::NotGranted { .. })
    ));
    assert!(matches!(
        decide("ada", &[claim(2, verb("publish", None), vec![Vec::new()])]),
        Err(ReceiptRefusal::NotGranted {
            kind: ActionKind::Outside,
            ..
        })
    ));
    assert!(matches!(
        decide(
            "ada",
            &[claim(3, verb("format-disk", None), vec![Vec::new()])]
        ),
        Err(ReceiptRefusal::NotPublished(_))
    ));
    assert!(
        matches!(
            decide("ada", &[claim(4, edit("secret"), vec![Vec::new()])]),
            Err(ReceiptRefusal::NotPublished(_))
        ),
        "a field the app does not offer edits of"
    );
}

#[test]
fn the_relay_refuses_a_granted_action_that_writes_another_unit() {
    let add = verb("add-comment", None);
    let comments = vec![PathStep::Field("comments".into())];
    assert_eq!(
        decide("cleo", &[claim(1, add.clone(), vec![comments.clone()])]),
        Ok(Receipt::Admit),
        "a commenter may add a comment"
    );
    assert!(
        matches!(
            decide("cleo", &[claim(2, add, vec![comments, Vec::new()])]),
            Err(ReceiptRefusal::OutsideFootprint { .. })
        ),
        "adding a comment may not also write the page"
    );
    assert!(
        matches!(
            decide(
                "cleo",
                &[claim(
                    3,
                    verb("resolve-comment", Some("c1")),
                    vec![comment("c2")]
                )]
            ),
            Err(ReceiptRefusal::OutsideFootprint { .. })
        ),
        "resolving one comment may not write another"
    );
    assert!(
        matches!(
            decide("cleo", &[claim(4, edit("body"), vec![Vec::new()])]),
            Err(ReceiptRefusal::NotGranted { .. })
        ),
        "a commenter may not edit the body"
    );
}

#[test]
fn claims_name_exactly_the_changes_and_a_missing_surface_holds() {
    let grants = grants();
    let party = Party::new(vec![Subject::person("ada")], grants.iter());
    let allows = |node: &TreePath, kind: ActionKind| {
        party.decide(node, kind, &Context::at(0)).answer == Answer::Allow
    };
    let claims = [claim(1, edit("body"), vec![Vec::new()])];
    for changes in [&[][..], &[[1; 32], [2; 32]][..], &[[2; 32]][..]] {
        assert_eq!(
            check_claims(
                &claims,
                &record(),
                Some(changes),
                None,
                Some(&Page),
                &allows
            ),
            Err(ReceiptRefusal::Unclaimed)
        );
    }
    let twice = [claims[0].clone(), claims[0].clone()];
    assert_eq!(
        check_claims(&twice, &record(), None, None, Some(&Page), &allows),
        Err(ReceiptRefusal::Unclaimed)
    );
    assert_eq!(
        check_claims(&claims, &record(), Some(&[[1; 32]]), None, None, &allows),
        Ok(Receipt::Hold),
        "without the app's surface the change is held, never trusted"
    );
    let written = |_: &[u8; 32]| vec![comment("c1")];
    assert_eq!(
        check_claims(
            &claims,
            &record(),
            Some(&[[1; 32]]),
            Some(&written),
            Some(&Page),
            &allows
        ),
        Err(ReceiptRefusal::UnclaimedUnit("comments.c1".into())),
        "a change writing a unit its claim leaves out"
    );
}

/// The author's device claims each change from the entry the executor wrote
/// into it; the relay reads the claims and the change hashes, never the
/// content.
#[test]
fn claims_derived_from_real_changes_pass_the_relay() {
    let mut doc = AutoCommit::new();
    let before = doc.get_heads();
    let entry = |what: Did| ActionContext {
        what: Some(what),
        ..ActionContext::for_resource("page-1", KIND, "Page")
    };
    doc.put(ROOT, "title", "Plan").unwrap();
    doc.commit_with(
        CommitOptions::default().with_message(
            entry(Did::Edit(FieldEdit {
                ty: "page".into(),
                field: "title".into(),
                path: vec![PathStep::Field("title".into())],
                op: EditOp::Set,
                old: None,
                new: None,
                args: std::collections::BTreeMap::new(),
            }))
            .change_message()
            .unwrap(),
        ),
    );
    doc.put(ROOT, "comments", "c").unwrap();
    doc.commit_with(
        CommitOptions::default().with_message(
            entry(Did::Verb(VerbCall::new(
                ActionKey::new(APP, KIND, "add-comment"),
                [],
            )))
            .change_message()
            .unwrap(),
        ),
    );
    let bytes = doc.save_after(&before);
    let changes = parse_changes(&bytes).unwrap();
    assert_eq!(changes.len(), 2);
    let claims = claims_of(&changes, |change| {
        if change.message().is_some_and(|m| m.contains("add-comment")) {
            vec![vec![PathStep::Field("comments".into())]]
        } else {
            vec![Vec::new()]
        }
    })
    .unwrap();
    let hashes: Vec<[u8; 32]> = changes.iter().map(|change| change.hash().0).collect();
    let grants = grants();
    for (person, expected) in [("ada", true), ("cleo", false)] {
        let party = Party::new(vec![Subject::person(person)], grants.iter());
        let allows = |node: &TreePath, kind: ActionKind| {
            party.decide(node, kind, &Context::at(0)).answer == Answer::Allow
        };
        let receipt = check_claims(
            &claims,
            &record(),
            Some(&hashes),
            None,
            Some(&Page),
            &allows,
        );
        assert_eq!(receipt.is_ok(), expected, "{person}: {receipt:?}");
    }
}
