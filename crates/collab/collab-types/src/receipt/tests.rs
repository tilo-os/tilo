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
    let written = |_: &[u8; 32]| crate::units::Written {
        units: vec![comment("c1")],
        added: Vec::new(),
    };
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

fn change_by(invoker: tilo_actions::ActionInvoker) -> Vec<automerge::Change> {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "v", 1).unwrap();
    let context = ActionContext::default().with_invoker(invoker);
    doc.commit_with(CommitOptions::default().with_message(context.change_message().unwrap()));
    parse_changes(&doc.save()).unwrap()
}

fn bot(actor: &str) -> tilo_actions::ActionInvoker {
    tilo_actions::ActionInvoker::actor(actor).with_kind(tilo_actions::ActorKind::Bot)
}

#[test]
fn an_appearance_is_admitted_only_from_the_bot_allowed_it() {
    let alice = tilo_actions::Appearance::new("alice", None, None).unwrap();
    let by_bot = change_by(bot("bot:7f1c").with_appearance(Some(alice.clone())));
    assert_eq!(check_appearances(&by_bot, Some("bot:7f1c")), Ok(()));
    assert!(matches!(
        check_appearances(&by_bot, None),
        Err(ReceiptRefusal::Appearance { .. })
    ));
    assert!(matches!(
        check_appearances(&by_bot, Some("bot:other")),
        Err(ReceiptRefusal::Appearance { .. })
    ));
    // An invoker that names the bot's actor id without being a bot is refused.
    let posing = change_by(
        tilo_actions::ActionInvoker::actor("bot:7f1c").with_appearance(Some(alice.clone())),
    );
    assert!(matches!(
        check_appearances(&posing, Some("bot:7f1c")),
        Err(ReceiptRefusal::Appearance { .. })
    ));
    let by_person = change_by(
        tilo_actions::ActionInvoker::actor("person:mallory").with_appearance(Some(alice)),
    );
    assert!(matches!(
        check_appearances(&by_person, Some("bot:7f1c")),
        Err(ReceiptRefusal::Appearance { .. })
    ));
    let plain = change_by(tilo_actions::ActionInvoker::actor("person:ada"));
    assert_eq!(check_appearances(&plain, None), Ok(()));
}

#[test]
fn an_invalid_appearance_is_refused_even_from_the_bot() {
    let hidden = tilo_actions::Appearance {
        name: "a\u{200B}lice".into(),
        avatar: None,
        persona: None,
    };
    let changes = change_by(bot("bot:7f1c").with_appearance(Some(hidden)));
    assert!(matches!(
        check_appearances(&changes, Some("bot:7f1c")),
        Err(ReceiptRefusal::Appearance { .. })
    ));
}

/// A calendar whose every verb may queue an upload to the server it syncs
/// with, in its `uploads` outbox.
struct Calendar;

fn uploads() -> UnitPath {
    vec![PathStep::Field("uploads".into())]
}

impl ReceiptSurface for Calendar {
    fn kind(&self) -> &str {
        KIND
    }
    fn verb(&self, action: &str) -> Option<(ActionKind, Option<String>)> {
        (action == "new-event").then_some((ActionKind::Outside, None))
    }
    fn editable(&self, path: &[PathStep]) -> bool {
        matches!(path.first(), Some(PathStep::Field(field)) if field == "title")
    }
    fn footprint(&self, _: &ClaimedAction) -> Vec<UnitPath> {
        vec![Vec::new()]
    }
    fn outboxes(&self) -> Vec<UnitPath> {
        vec![uploads()]
    }
}

/// Whether Ada, who may make undoable changes but not reach outside the
/// room, is admitted `claims`, each change writing what its claim says
/// (inside each unit it names); `read`: the receiver read the changes
/// themselves.
fn calendar(claims: &[ChangeClaim], read: bool) -> Result<Receipt, ReceiptRefusal> {
    let wrote = |units: &[UnitPath]| crate::units::Written {
        added: units
            .iter()
            .filter(|unit| !unit.is_empty())
            .cloned()
            .collect(),
        units: units.to_vec(),
    };
    calendar_read(claims, read.then_some(&wrote))
}

/// What a receiver reads a change wrote, given the units its claim names.
type Wrote<'a> = &'a dyn Fn(&[UnitPath]) -> crate::units::Written;

/// [`calendar`], the receiver reading from each change what `wrote` says
/// of its claimed units.
fn calendar_read(
    claims: &[ChangeClaim],
    wrote: Option<Wrote<'_>>,
) -> Result<Receipt, ReceiptRefusal> {
    let grants = grants();
    let party = Party::new(vec![Subject::person("ada")], grants.iter());
    let allows = |node: &TreePath, kind: ActionKind| {
        party.decide(node, kind, &Context::at(0)).answer == Answer::Allow
    };
    let units = |hash: &[u8; 32]| {
        let units = claims
            .iter()
            .find(|claim| claim.change == *hash)
            .map(|claim| claim.units.clone())
            .unwrap_or_default();
        wrote.map(|wrote| wrote(&units)).unwrap_or_default()
    };
    let written: Option<WrittenUnits<'_>> = wrote.is_some().then_some(&units as WrittenUnits<'_>);
    check_claims(claims, &record(), None, written, Some(&Calendar), &allows)
}

/// A change reaches outside a record that queues by what it queued, not by
/// what its action might have: an event added to the local calendar queues
/// no upload and is an undoable change, while one that queued an upload, or
/// a field edit whose change queued one, reaches outside.
#[test]
fn a_change_reaches_outside_by_what_it_queued_in_an_outbox() {
    let outside = |result: Result<Receipt, ReceiptRefusal>| {
        matches!(
            result,
            Err(ReceiptRefusal::NotGranted {
                kind: ActionKind::Outside,
                ..
            })
        )
    };
    let local = [claim(1, verb("new-event", None), vec![Vec::new()])];
    assert_eq!(calendar(&local, true), Ok(Receipt::Admit));
    assert_eq!(
        calendar(&local, false),
        Ok(Receipt::Admit),
        "a sealed change that claims no outbox is decided as queueing nothing"
    );
    let synced = [claim(
        2,
        verb("new-event", None),
        vec![Vec::new(), uploads()],
    )];
    assert!(outside(calendar(&synced, true)));
    let retitled = [claim(3, edit("title"), vec![Vec::new(), uploads()])];
    assert!(
        outside(calendar(&retitled, true)),
        "an edit that queued an upload reaches outside too"
    );
    assert_eq!(
        calendar(&[claim(4, edit("title"), vec![Vec::new()])], true),
        Ok(Receipt::Admit)
    );
    let made = |units: &[UnitPath]| crate::units::Written {
        units: units.to_vec(),
        added: Vec::new(),
    };
    assert_eq!(
        calendar_read(&synced, Some(&made)),
        Ok(Receipt::Admit),
        "a new record's first event makes its outbox empty and queues nothing"
    );
    let queued = |_: &[UnitPath]| crate::units::Written {
        units: vec![Vec::new(), uploads()],
        added: vec![uploads()],
    };
    assert!(
        outside(calendar_read(&local, Some(&queued))),
        "a change that queued an upload its claim leaves out reaches outside"
    );
}

/// A history step may touch any unit, and its claim names them all rather
/// than the entries it queued: a checkpoint no receiver read is an undoable
/// change, and the devices that open it check what it queued.
#[test]
fn a_history_step_that_claims_every_unit_queues_nothing() {
    let checkpoint = ClaimedAction::History(tilo_actions::HistoryOp::Checkpoint {
        name: "before the party".into(),
    });
    let claims = [claim(5, checkpoint, vec![Vec::new(), uploads()])];
    assert_eq!(calendar(&claims, false), Ok(Receipt::Admit));
    assert!(matches!(
        calendar(&claims, true),
        Err(ReceiptRefusal::NotGranted {
            kind: ActionKind::Outside,
            ..
        })
    ));
}

/// One resource kind of a record that holds items of several (a tilolang app's root: its
/// categories among its channels), publishing one verb.
struct ItemKind(&'static str, &'static str);

impl ReceiptSurface for ItemKind {
    fn kind(&self) -> &str {
        self.0
    }
    fn verb(&self, action: &str) -> Option<(ActionKind, Option<String>)> {
        (action == self.1).then_some((ActionKind::Undoable, None))
    }
    fn editable(&self, _: &[PathStep]) -> bool {
        false
    }
    fn footprint(&self, _: &ClaimedAction) -> Vec<UnitPath> {
        vec![Vec::new()]
    }
}

/// The surface of an app whose record holds channels and categories, each kind with its verb.
struct Channels;

impl SurfaceSource for Channels {
    fn surface(
        &self,
        _: &Record<'_>,
        kind: Option<&str>,
        _: Option<&[u8; 32]>,
    ) -> Option<std::sync::Arc<dyn ReceiptSurface>> {
        match kind {
            Some("chat.channels") => Some(std::sync::Arc::new(ItemKind(
                "chat.channels",
                "new_category",
            ))),
            Some("chat.category") => Some(std::sync::Arc::new(ItemKind(
                "chat.category",
                "add_channel",
            ))),
            _ => None,
        }
    }
}

/// An op that carries changes of actions on items of different kinds (a record handed to its
/// room whole: a category made, then a channel added to it) is decided change by change, each
/// against the kind its own action names, never all against the first one's.
#[test]
fn each_change_is_decided_against_the_kind_its_action_names() {
    let mut doc = AutoCommit::new();
    for (key, kind, action) in [
        ("a", "chat.channels", "new_category"),
        ("b", "chat.category", "add_channel"),
    ] {
        doc.put(ROOT, key, 1).unwrap();
        let entry = ActionContext {
            what: Some(Did::Verb(VerbCall::new(
                ActionKey::new(APP, kind, action),
                [],
            ))),
            ..ActionContext::for_resource("channels", kind, "Channels")
        };
        doc.commit_with(CommitOptions::default().with_message(entry.change_message().unwrap()));
    }
    let payload = crate::op::DocumentMutation::Incremental(doc.save_after(&[]))
        .encode()
        .unwrap();
    let op = crate::op::SignedOp::new_unsigned(
        payload,
        crate::document_artifact_id(APP, ROOM, "channels"),
        crate::Ed25519PublicKey::from_bytes([7; 32]),
        crate::document_attestation_id(APP, ROOM, "channels"),
        crate::role::OpKind::Change,
        std::time::SystemTime::UNIX_EPOCH,
        1,
    )
    .with_surface(Some([1; 32]));
    let surfaces = Surfaces::from_source(std::sync::Arc::new(Channels));
    assert_eq!(
        decide_op(&op, &record(), None, None, &surfaces, &|_, _| true),
        Ok(Receipt::Admit)
    );
    // A claim to a verb its kind does not publish is still refused.
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "c", 1).unwrap();
    let entry = ActionContext {
        what: Some(Did::Verb(VerbCall::new(
            ActionKey::new(APP, "chat.category", "new_category"),
            [],
        ))),
        ..ActionContext::for_resource("channels", "chat.category", "Category")
    };
    doc.commit_with(CommitOptions::default().with_message(entry.change_message().unwrap()));
    let sealed_claims =
        claims_of(&parse_changes(&doc.save()).unwrap(), |_| vec![Vec::new()]).unwrap();
    let surface_for = |kind: Option<&str>| Channels.surface(&record(), kind, None);
    assert!(matches!(
        check_claims_by_kind(&sealed_claims, &record(), None, &surface_for, &|_, _| true),
        Err(ReceiptRefusal::NotPublished(_))
    ));
}
