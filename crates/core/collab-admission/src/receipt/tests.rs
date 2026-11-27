use automerge::transaction::{CommitOptions, Transactable};
use automerge::{AutoCommit, ROOT};
use tilo_action_policy::{Answer, Context, Grant, Grantee, KindSet, MemberId, Party, roles};
use tilo_actions::{ActionContext, ActionKey, Did, EditOp, FieldEdit, VerbCall};

use super::*;
use crate::claim::parse_changes;
use collab_types::claim::{UnitPath, claims_of};
use collab_types::receipt::*;
use tilo_action_policy::TreePath;
use tilo_actions::PathStep;

const WORKSPACE: &str = "workspace-1";
const APP: &str = "so.tilo.notes";
const KIND: &str = "notes.page";

fn document() -> WrittenDocument<'static> {
    WrittenDocument {
        workspace: WORKSPACE,
        app: APP,
        document: "page-1",
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
        invoker: None,
        call: None,
    }
}

/// Ada edits the workspace; Cleo only comments.
fn grants() -> Vec<Grant> {
    let owner = Grantee::person("workspace-owner");
    vec![
        Grant::new(
            "ada-edits",
            Grantee::person("ada"),
            TreePath::workspace(WORKSPACE),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            owner.clone(),
        ),
        Grant::new(
            "cleo-comments",
            Grantee::person("cleo"),
            roles::comments(WORKSPACE),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            owner,
        ),
    ]
}

fn decide(person: &str, claims: &[ChangeClaim]) -> Result<Receipt, ReceiptRefusal> {
    let grants = grants();
    let party = Party::new(vec![Grantee::person(person)], grants.iter());
    let allows = |_: Option<&tilo_action_policy::Acting>, node: &TreePath, kind: ActionKind| {
        party.decide(node, kind, &Context::at(0)).answer == Answer::Allow
    };
    check_claims(claims, &document(), None, None, Some(&Page), &allows)
}

tilo_test_env::scenario! {
    /// What a receiver decides from an op's claims: the grant, the units, the changes named and the kind each action names.
    fn the_server_admits_only_granted_actions_whose_claims_name_their_changes;

    phase the_server_refuses_an_action_the_author_is_not_granted;
    {
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

    phase the_server_refuses_a_granted_action_that_writes_another_unit;
    {
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

    phase claims_name_exactly_the_changes_and_a_missing_surface_holds;
    {
        let grants = grants();
        let party = Party::new(vec![Grantee::person("ada")], grants.iter());
        let allows = |_: Option<&tilo_action_policy::Acting>, node: &TreePath, kind: ActionKind| {
            party.decide(node, kind, &Context::at(0)).answer == Answer::Allow
        };
        let claims = [claim(1, edit("body"), vec![Vec::new()])];
        for changes in [&[][..], &[[1; 32], [2; 32]][..], &[[2; 32]][..]] {
            assert_eq!(
                check_claims(
                    &claims,
                    &document(),
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
            check_claims(&twice, &document(), None, None, Some(&Page), &allows),
            Err(ReceiptRefusal::Unclaimed)
        );
        assert_eq!(
            check_claims(&claims, &document(), Some(&[[1; 32]]), None, None, &allows),
            Ok(Receipt::Hold),
            "without the app's surface the change is held, never trusted"
        );
        let written = |_: &[u8; 32]| collab_types::units::Written {
            units: vec![comment("c1")],
            added: Vec::new(),
        };
        assert_eq!(
            check_claims(
                &claims,
                &document(),
                Some(&[[1; 32]]),
                Some(&written),
                Some(&Page),
                &allows
            ),
            Err(ReceiptRefusal::UnclaimedUnit("comments.c1".into())),
            "a change writing a unit its claim leaves out"
        );
    }

    phase claims_derived_from_real_changes_pass_the_server;
    // The author's device claims each change from the entry the executor wrote
    // into it; the server reads the claims and the change hashes, never the
    // content.
    {
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
            let party = Party::new(vec![Grantee::person(person)], grants.iter());
            let allows = |_: Option<&tilo_action_policy::Acting>, node: &TreePath, kind: ActionKind| {
                party.decide(node, kind, &Context::at(0)).answer == Answer::Allow
            };
            let receipt = check_claims(
                &claims,
                &document(),
                Some(&hashes),
                None,
                Some(&Page),
                &allows,
            );
            assert_eq!(receipt.is_ok(), expected, "{person}: {receipt:?}");
        }
    }

    phase each_change_is_decided_against_the_kind_its_action_names;
    // An op that carries changes of actions on items of different kinds (a record handed to its
    // workspace whole: a category made, then a channel added to it) is decided change by change, each
    // against the kind its own action names, never all against the first one's.
    {
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
        let payload = collab_types::op::DocumentMutation::Incremental(doc.save_after(&[]))
            .encode()
            .unwrap();
        let op = collab_types::op::SignedOp::new_unsigned(
            payload,
            collab_types::document_artifact_id(APP, WORKSPACE, "channels"),
            collab_types::Ed25519PublicKey::from_bytes([7; 32]),
            collab_types::document_attestation_id(APP, WORKSPACE, "channels"),
            collab_types::role::OpKind::Change,
            std::time::SystemTime::UNIX_EPOCH,
            1,
        )
        .with_surface(Some([1; 32]));
        let surfaces = Surfaces::from_source(std::sync::Arc::new(Channels));
        assert_eq!(
            decide_op(&op, &document(), None, None, &surfaces, &|_, _, _| true),
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
        let surface_for = |kind: Option<&str>| Channels.surface(&document(), kind, None);
        assert!(matches!(
            check_claims_by_kind(&sealed_claims, &document(), None, false, &surface_for, &|_, _, _| true),
            Err(ReceiptRefusal::NotPublished(_))
        ));
    }
}

fn change_by(invoker: tilo_actions::ActionInvoker) -> Vec<automerge::Change> {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "v", 1).unwrap();
    let context = ActionContext::default().with_invoker(invoker);
    doc.commit_with(CommitOptions::default().with_message(context.change_message().unwrap()));
    parse_changes(&doc.save()).unwrap()
}

fn bot(installation: &str) -> tilo_actions::ActionInvoker {
    tilo_actions::ActionInvoker::bot(installation)
}

tilo_test_env::scenario! {
    /// Who may change how a member appears.
    fn an_appearance_is_admitted_only_valid_and_from_its_bot;

    phase an_appearance_is_admitted_only_from_the_bot_allowed_it;
    {
        let alice = tilo_actions::Appearance::new("alice", None, None).unwrap();
        let by_bot = change_by(bot("7f1c").with_appearance(Some(alice.clone())));
        assert_eq!(check_appearances(&by_bot, Some(&MemberId::bot("7f1c"))), Ok(()));
        assert!(matches!(
            check_appearances(&by_bot, None),
            Err(ReceiptRefusal::Appearance { .. })
        ));
        assert!(matches!(
            check_appearances(&by_bot, Some(&MemberId::bot("other"))),
            Err(ReceiptRefusal::Appearance { .. })
        ));
        // A key that spells a bot's actor id is no person at all: the id is
        // refused where it is made, so no receipt can claim to be one.
        assert_eq!(
            tilo_action_policy::PersonId::parse("bot:7f1c"),
            Err(tilo_action_policy::PersonIdError::Prefixed)
        );
        let by_person = change_by(
            tilo_actions::ActionInvoker::person("mallory").with_appearance(Some(alice)),
        );
        assert!(matches!(
            check_appearances(&by_person, Some(&MemberId::bot("7f1c"))),
            Err(ReceiptRefusal::Appearance { .. })
        ));
        let plain = change_by(tilo_actions::ActionInvoker::person("ada"));
        assert_eq!(check_appearances(&plain, None), Ok(()));
    }

    phase an_invalid_appearance_is_refused_even_from_the_bot;
    {
        let hidden = tilo_actions::Appearance {
            name: "a\u{200B}lice".into(),
            avatar: None,
            persona: None,
        };
        let changes = change_by(bot("7f1c").with_appearance(Some(hidden)));
        assert!(matches!(
            check_appearances(&changes, Some(&MemberId::bot("7f1c"))),
            Err(ReceiptRefusal::Appearance { .. })
        ));
    }
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
/// workspace, is admitted `claims`, each change writing what its claim says
/// (inside each unit it names); `read`: the receiver read the changes
/// themselves.
fn calendar(claims: &[ChangeClaim], read: bool) -> Result<Receipt, ReceiptRefusal> {
    let wrote = |units: &[UnitPath]| collab_types::units::Written {
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
type Wrote<'a> = &'a dyn Fn(&[UnitPath]) -> collab_types::units::Written;

/// [`calendar`], the receiver reading from each change what `wrote` says
/// of its claimed units.
fn calendar_read(
    claims: &[ChangeClaim],
    wrote: Option<Wrote<'_>>,
) -> Result<Receipt, ReceiptRefusal> {
    let grants = grants();
    let party = Party::new(vec![Grantee::person("ada")], grants.iter());
    let allows = |_: Option<&tilo_action_policy::Acting>, node: &TreePath, kind: ActionKind| {
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
    check_claims(claims, &document(), None, written, Some(&Calendar), &allows)
}

tilo_test_env::scenario! {
    /// Whether a change reaches outside its workspace, read from what it queued in an outbox.
    fn a_change_reaches_outside_only_by_what_it_queued;

    phase a_change_reaches_outside_by_what_it_queued_in_an_outbox;
    // A change reaches outside a record that queues by what it queued, not by
    // what its action might have: an event added to the local calendar queues
    // no upload and is an undoable change, while one that queued an upload, or
    // a field edit whose change queued one, reaches outside.
    {
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
        let made = |units: &[UnitPath]| collab_types::units::Written {
            units: units.to_vec(),
            added: Vec::new(),
        };
        assert_eq!(
            calendar_read(&synced, Some(&made)),
            Ok(Receipt::Admit),
            "a new document's first event makes its outbox empty and queues nothing"
        );
        let queued = |_: &[UnitPath]| collab_types::units::Written {
            units: vec![Vec::new(), uploads()],
            added: vec![uploads()],
        };
        assert!(
            outside(calendar_read(&local, Some(&queued))),
            "a change that queued an upload its claim leaves out reaches outside"
        );
    }

    phase a_history_step_that_claims_every_unit_queues_nothing;
    // A history step may touch any unit, and its claim names them all rather
    // than the entries it queued: a checkpoint no receiver read is an undoable
    // change, and the devices that open it check what it queued.
    {
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
        _: &WrittenDocument<'_>,
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

#[path = "invoker_tests.rs"]
mod invoker_tests;

#[path = "continuation_tests.rs"]
mod continuation_tests;

#[path = "outbox_tests.rs"]
mod outbox_tests;

/// The surface of the chat's messages, published as a chain of parts.
struct Messages;

impl ReceiptSurface for Messages {
    fn kind(&self) -> &'static str {
        "chat.messages"
    }
    fn verb(&self, action: &str) -> Option<(ActionKind, Option<String>)> {
        (action == "post").then_some((ActionKind::Undoable, None))
    }
    fn editable(&self, _: &[PathStep]) -> bool {
        false
    }
    fn footprint(&self, _: &ClaimedAction) -> Vec<UnitPath> {
        vec![Vec::new()]
    }
}

const CHAT: &str = "so.tilo.chat";
const CHAIN: &str = "tab:0000000000000001";
const OTHER_CHAIN: &str = "tab:0000000000000002";

fn post() -> ClaimedAction {
    ClaimedAction::Verb {
        action: ActionKey::new(CHAT, "chat.messages", "post"),
        item: None,
    }
}

fn posted(document: &str) -> WrittenDocument<'_> {
    WrittenDocument {
        workspace: WORKSPACE,
        app: CHAT,
        document,
    }
}

/// Whether `person`, granted undoable changes on `granted`, may post `action`
/// on `document`, as `check_claims` decides it.
fn posts(
    person: &str,
    granted: &str,
    document: &str,
    action: ClaimedAction,
) -> Result<Receipt, ReceiptRefusal> {
    let grant = Grant::new(
        "chain-grant",
        Grantee::person(person),
        TreePath::workspace(WORKSPACE)
            .app(CHAT)
            .kind("chat.messages")
            .resource(granted),
        KindSet::UNDOABLE,
        Answer::Allow,
        Grantee::person("workspace-owner"),
    );
    let party = Party::new(vec![Grantee::person(person)], std::iter::once(&grant));
    let allows = |_: Option<&tilo_action_policy::Acting>, node: &TreePath, kind: ActionKind| {
        party.decide(node, kind, &Context::at(0)).answer == Answer::Allow
    };
    check_claims(
        &[claim(1, action, vec![Vec::new()])],
        &posted(document),
        None,
        None,
        Some(&Messages),
        &allows,
    )
}

#[test]
fn a_part_or_chunk_is_decided_at_the_resource_its_tree_is_published_under() {
    let part = format!("derived:part.0@{CHAIN}");
    let chain_resource = format!("chain:{CHAIN}");
    let node = claim_node(&posted(&part), &Messages, &post()).to_string();
    assert!(
        node.ends_with(&format!("resource:{chain_resource}/action:post")),
        "{node}"
    );
    assert_eq!(
        written_node(&posted(&part)),
        TreePath::workspace(WORKSPACE)
            .app(CHAT)
            .resource(chain_resource.clone())
    );
    // A chunk of a part is the part's chain; a chunk of a document is that document.
    let chunk = format!("derived:chunk.2@{part}");
    assert_eq!(published_resource(&chunk), chain_resource);
    assert_eq!(
        published_resource(&format!("derived:chunk.1@{CHAIN}")),
        CHAIN
    );
    assert_eq!(published_resource(CHAIN), CHAIN);
}

#[test]
fn a_member_granted_undoable_on_the_chain_is_admitted_for_a_part_write() {
    let part = format!("derived:part.0@{CHAIN}");
    let chain = format!("chain:{CHAIN}");
    assert_eq!(posts("ada", &chain, &part, post()), Ok(Receipt::Admit));
    // Granted on another chain, the same member is refused: the part is its
    // own chain's, never one the writer names.
    assert!(matches!(
        posts("ada", &format!("chain:{OTHER_CHAIN}"), &part, post()),
        Err(ReceiptRefusal::NotGranted { .. })
    ));
}

#[test]
fn a_change_naming_another_chain_is_refused_where_that_chain_is_granted() {
    // The change's own action context names another chain than its document's
    // id does: the node comes from the id, so the other chain's grant reaches
    // nothing.
    let named_elsewhere = ClaimedAction::of(&ActionContext {
        what: Some(Did::Verb(VerbCall::new(
            ActionKey::new(CHAT, "chat.messages", "post"),
            [],
        ))),
        ..ActionContext::for_resource(format!("chain:{OTHER_CHAIN}"), "chat.messages", "Chat")
    })
    .unwrap();
    let part = format!("derived:part.0@{CHAIN}");
    let node = claim_node(&posted(&part), &Messages, &named_elsewhere).to_string();
    assert!(
        node.contains(&format!("resource:chain:{CHAIN}/")),
        "the document's own chain decides: {node}"
    );
    assert!(matches!(
        posts(
            "ada",
            &format!("chain:{OTHER_CHAIN}"),
            &part,
            named_elsewhere
        ),
        Err(ReceiptRefusal::NotGranted { .. })
    ));
}

/// A resource kind of a test surface: `editable` says whether a change may edit it whole, and
/// `unit` the one field unit such an edit may write.
#[derive(Clone)]
struct TestKind(&'static str, bool, Option<&'static str>);

impl ReceiptSurface for TestKind {
    fn kind(&self) -> &str {
        self.0
    }
    fn verb(&self, _: &str) -> Option<(ActionKind, Option<String>)> {
        None
    }
    fn editable(&self, _: &[PathStep]) -> bool {
        self.1
    }
    fn footprint(&self, action: &ClaimedAction) -> Vec<UnitPath> {
        match action {
            ClaimedAction::Edit { path, .. } if path.is_empty() => {
                let mut units = vec![Vec::new()];
                units.extend(self.2.map(|field| vec![PathStep::Field(field.into())]));
                units
            }
            ClaimedAction::Group(members) => members
                .iter()
                .flat_map(|member| self.footprint(member))
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// A surface declaring `kinds`; `None` names the app's own, the first.
struct Kinds(Vec<TestKind>);

impl Kinds {
    fn new(kinds: impl IntoIterator<Item = TestKind>) -> Self {
        Self(kinds.into_iter().collect())
    }
}

impl SurfaceSource for Kinds {
    fn surface(
        &self,
        _: &WrittenDocument<'_>,
        kind: Option<&str>,
        _: Option<&[u8; 32]>,
    ) -> Option<std::sync::Arc<dyn ReceiptSurface>> {
        let named = kind.unwrap_or(self.0.first()?.0);
        self.0
            .iter()
            .find(|kind| kind.0 == named)
            .map(|kind| std::sync::Arc::new(kind.clone()) as std::sync::Arc<dyn ReceiptSurface>)
    }
}

/// A resource store's document: the app's own, which holds records of every kind.
fn store_document() -> WrittenDocument<'static> {
    WrittenDocument {
        workspace: WORKSPACE,
        app: APP,
        document: "*/tl.notes#0",
    }
}

/// A resource store's change that makes a record of `kind`, stamped as the store stamps it.
fn store_change(kind: &str) -> automerge::Change {
    let mut doc = AutoCommit::new();
    let record = doc
        .put_object(ROOT, "r:n1", automerge::ObjType::Map)
        .unwrap();
    doc.put(&record, "k", kind).unwrap();
    let stamp = collab_types::stamp::Stamp {
        verb: "new".into(),
        invoker: "ada".into(),
        invocation: "i1".into(),
        ..collab_types::stamp::Stamp::default()
    };
    doc.commit_with(CommitOptions::default().with_message(stamp.message()));
    parse_changes(&doc.save()).unwrap().remove(0)
}

/// A change over records of two kinds is decided by the surface of each: one kind whose record
/// is not edited whole refuses the change, and the units of both are what it is checked against.
#[test]
fn a_change_over_records_of_two_kinds_is_decided_by_the_surface_of_each() {
    let whole = |kind: &str| ClaimedAction::whole_resource_edit(kind.to_owned());
    let notes = Kinds::new([
        TestKind("tl.notes.Note", true, Some("title")),
        TestKind("tl.notes", false, None),
    ]);
    let surface_for = |kind: Option<&str>| notes.surface(&store_document(), kind, None);
    let decide = |action: ClaimedAction, units: Vec<UnitPath>| {
        check_claims_by_kind(
            &[claim(1, action, units)],
            &store_document(),
            None,
            false,
            &surface_for,
            &|_, _, _| true,
        )
    };
    assert_eq!(
        decide(whole("tl.notes.Note"), vec![Vec::new()]),
        Ok(Receipt::Admit)
    );
    let both = decide(
        ClaimedAction::Group(vec![whole("tl.notes.Note"), whole("tl.notes")]),
        vec![Vec::new()],
    );
    assert!(
        both.is_err(),
        "the package's kind publishes no edit: {both:?}"
    );

    let folders = Kinds::new([
        TestKind("tl.notes.Note", true, Some("title")),
        TestKind("tl.notes.Folder", true, Some("name")),
    ]);
    let surface_for = |kind: Option<&str>| folders.surface(&store_document(), kind, None);
    let decide = |units: Vec<UnitPath>| {
        check_claims_by_kind(
            &[claim(
                1,
                ClaimedAction::Group(vec![whole("tl.notes.Note"), whole("tl.notes.Folder")]),
                units,
            )],
            &store_document(),
            None,
            false,
            &surface_for,
            &|_, _, _| true,
        )
    };
    let title = vec![PathStep::Field("title".into())];
    let name = vec![PathStep::Field("name".into())];
    assert_eq!(
        decide(vec![Vec::new(), title, name]),
        Ok(Receipt::Admit),
        "a unit of either kind"
    );
    assert!(
        matches!(
            decide(vec![vec![PathStep::Field("elsewhere".into())]]),
            Err(ReceiptRefusal::OutsideFootprint { .. })
        ),
        "a unit no kind allows is refused"
    );
}

/// A claim naming other kinds than its change says is refused where the change is opened: a
/// store's change names the kinds of the records it writes, and a native edit of a record names
/// none, being the record's own.
#[test]
fn a_claim_naming_other_kinds_than_its_change_is_refused_when_opened() {
    let change = store_change("tl.notes.Note");
    let read = ChangeClaim::of_change(change.hash().0, change.message(), Vec::new()).unwrap();
    // A store's change claims the verb it was written under beside the kinds of its records.
    let naming = |verb: Option<&str>, kind: &str| {
        let edit = ClaimedAction::whole_resource_edit(kind.to_owned());
        match verb {
            Some(verb) => ClaimedAction::Group(vec![
                ClaimedAction::Verb {
                    action: tilo_actions::ActionKey::new("", "", verb),
                    item: None,
                },
                edit,
            ]),
            None => edit,
        }
    };
    let opened = |change: &automerge::Change, read: &ChangeClaim, claimed: ClaimedAction| {
        let op = collab_types::op::SignedOp::new_unsigned(
            Vec::new(),
            collab_types::document_artifact_id(APP, WORKSPACE, "*/tl.notes#0"),
            collab_types::Ed25519PublicKey::from_bytes([7; 32]),
            collab_types::document_attestation_id(APP, WORKSPACE, "*/tl.notes#0"),
            collab_types::role::OpKind::Change,
            std::time::SystemTime::UNIX_EPOCH,
            1,
        )
        .with_claims(vec![ChangeClaim {
            action: claimed,
            ..read.clone()
        }]);
        check_opened_mutation(
            &op,
            &collab_types::op::DocumentMutation::Incremental(change.raw_bytes().to_vec()),
        )
    };
    assert_eq!(read.action.store_verb(), Some("new"));
    assert_eq!(
        opened(&change, &read, naming(Some("new"), "tl.notes.Note")),
        Ok(())
    );
    assert!(matches!(
        opened(&change, &read, naming(Some("new"), "tl.notes.Folder")),
        Err(ReceiptRefusal::Misclaimed(_))
    ));
    // Nor may its claim name another verb than the one it was written under, or none.
    for verb in [Some("remove"), None] {
        assert!(matches!(
            opened(&change, &read, naming(verb, "tl.notes.Note")),
            Err(ReceiptRefusal::Misclaimed(_))
        ));
    }
    assert_eq!(
        collab_types::claim::store_record_kinds(
            "*/tl.notes#0",
            None,
            std::slice::from_ref(&change)
        )
        .remove(&change.hash().0),
        Some(std::iter::once("tl.notes.Note".to_owned()).collect())
    );

    let mut doc = AutoCommit::new();
    doc.put(ROOT, "title", "Groceries").unwrap();
    doc.commit_with(
        CommitOptions::default()
            .with_message(collab_types::FrameworkChange::Native.change_message()),
    );
    let native = parse_changes(&doc.save()).unwrap().remove(0);
    let native_read =
        ChangeClaim::of_change(native.hash().0, native.message(), Vec::new()).unwrap();
    assert_eq!(opened(&native, &native_read, naming(None, "")), Ok(()));
    assert!(matches!(
        opened(&native, &native_read, naming(None, "tl.notes.Note")),
        Err(ReceiptRefusal::Misclaimed(_))
    ));

    // In a store's document a device's own edit writes records of the store's kinds as a stamp
    // does, and its writer names them: the claim names them, under no verb it was not written
    // under.
    let in_store = |claimed: ClaimedAction| {
        let op = collab_types::op::SignedOp::new_unsigned(
            Vec::new(),
            collab_types::document_artifact_id(APP, WORKSPACE, "*/tl.notes#0"),
            collab_types::Ed25519PublicKey::from_bytes([7; 32]),
            collab_types::document_attestation_id(APP, WORKSPACE, "*/tl.notes#0"),
            collab_types::role::OpKind::Change,
            std::time::SystemTime::UNIX_EPOCH,
            1,
        )
        .with_claims(vec![ChangeClaim {
            action: claimed,
            ..native_read.clone()
        }]);
        crate::receipt::check_opened_mutation_of(
            &op,
            Some("*/tl.notes#0"),
            &collab_types::op::DocumentMutation::Incremental(native.raw_bytes().to_vec()),
        )
    };
    assert_eq!(in_store(naming(None, "tl.notes.Note")), Ok(()));
    assert!(matches!(
        in_store(naming(Some("new"), "tl.notes.Note")),
        Err(ReceiptRefusal::Misclaimed(_))
    ));
}

/// A store change that names no kind is a new write that says nothing of what it writes, and is
/// refused; one admitted before changes claimed their kinds, handed on from a store, keeps the
/// verdict it got: it is decided whole against the app's own resource, as it was then.
#[test]
fn a_store_change_that_names_no_kind_is_refused_new_and_kept_as_it_was_in_replay() {
    let notes = Kinds::new([TestKind("tl.notes", true, None)]);
    let surface_for = |kind: Option<&str>| notes.surface(&store_document(), kind, None);
    let unnamed = claim(
        1,
        ClaimedAction::whole_resource_edit(String::new()),
        vec![Vec::new()],
    );
    let decide = |replay: bool| {
        check_claims_by_kind(
            std::slice::from_ref(&unnamed),
            &store_document(),
            None,
            replay,
            &surface_for,
            &|_, _, _| true,
        )
    };
    assert!(
        matches!(decide(false), Err(ReceiptRefusal::Misclaimed(ref why)) if why.contains("no kind")),
        "{:?}",
        decide(false)
    );
    assert_eq!(decide(true), Ok(Receipt::Admit));
}

/// A store's content document holds one resource, whose kind its id names. A change of it that
/// names no kind (the record's derived founding, a step to its storage version, a device's own
/// edit of it) is decided against that kind, never refused for naming none and never against the
/// app's first resource. Only a store's stamp claims the kinds it writes: any other change's claim
/// stays what its own message says, so the device that opens it finds it as claimed.
#[test]
fn a_content_documents_change_that_names_no_kind_is_decided_against_the_kind_its_id_names() {
    // The app's first resource publishes no edit: a change decided against it would be refused.
    let notes = Kinds::new([
        TestKind("tl.notes", false, None),
        TestKind("tl.notes.Note", true, None),
    ]);
    let content = WrittenDocument {
        workspace: WORKSPACE,
        app: APP,
        document: "*/tl.notes.Note/n1~0",
    };
    let asked = std::cell::RefCell::new(Vec::new());
    let surface_for = |kind: Option<&str>| {
        asked.borrow_mut().push(kind.map(str::to_owned));
        notes.surface(&content, kind, None)
    };
    // A framework change writes what its kind derives, and claims no unit of the record's.
    for (action, units) in [
        (
            ClaimedAction::Framework(collab_types::FrameworkChange::Genesis),
            Vec::new(),
        ),
        (
            ClaimedAction::Framework(collab_types::FrameworkChange::Migrate),
            Vec::new(),
        ),
        (
            ClaimedAction::whole_resource_edit(String::new()),
            vec![Vec::new()],
        ),
    ] {
        assert_eq!(
            check_claims_by_kind(
                &[claim(1, action.clone(), units)],
                &content,
                None,
                false,
                &surface_for,
                &|_, _, _| true,
            ),
            Ok(Receipt::Admit),
            "{action:?}"
        );
    }
    assert!(
        asked
            .borrow()
            .iter()
            .all(|kind| kind.as_deref() == Some("tl.notes.Note")),
        "{:?}",
        asked.borrow()
    );

    // A device's own edit of the record writes a record of the store's kind as a store's stamp
    // does, and its store reads it so.
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "title", "Groceries").unwrap();
    doc.commit_with(
        CommitOptions::default()
            .with_message(collab_types::FrameworkChange::Native.change_message()),
    );
    let native = parse_changes(&doc.save()).unwrap().remove(0);
    let stamped = store_change("tl.notes.Note");
    let kinds = collab_types::claim::store_record_kinds(
        content.document,
        None,
        &[native.clone(), stamped.clone()],
    );
    let note = std::iter::once("tl.notes.Note".to_owned()).collect();
    assert_eq!(kinds.get(&native.hash().0), Some(&note));
    assert_eq!(kinds.get(&stamped.hash().0), Some(&note));
}
