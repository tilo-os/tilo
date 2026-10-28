#![allow(clippy::unwrap_used, reason = "test assertions")]

//! A host without a screen acts through the app's own view: mounted over the
//! record, never drawn, every operation through the door a click takes, and
//! the change it made flowing back to the person's window.

use tilo_app::collaboration::DocumentMutation;
use tilo_app::framework::{AttachedView, definition};
use tilo_app::permissions::{KindSet, PermissionContext, TreePath};
use tilo_app::prelude::*;
use tilo_app::record::RecordCheckout;

use crate::kinds::{Tally, TallyApp};

const AGENT: &str = "scribe";

fn writer() -> PermissionContext {
    PermissionContext::agent_granted(
        AGENT,
        &TreePath::root().app(TallyApp::ID),
        KindSet::READ_UNDOABLE,
    )
}

fn attached(app: &TestApp<TallyApp>, permissions: PermissionContext) -> AttachedView {
    let definition = definition::<TallyApp>();
    AttachedView::open(
        definition,
        "test-workspace",
        RecordCheckout::new(app.record().clone(), definition.record_kind.clone()),
        app.read_document(|doc| doc.clone().save()),
        AGENT,
        permissions,
        false,
    )
    .unwrap()
}

#[test]
fn an_agent_acts_through_the_view_and_the_window_sees_it() {
    let mut app = TestApp::<TallyApp>::new().unwrap();
    app.invoke(Tally::RENAME.with("visitors")).unwrap();
    let mut view = attached(&app, writer());
    assert!(view.take_changes().is_none(), "mounting writes nothing");

    let bump = tilo_actions::ActionAddress::new("test.tally", "bump");
    assert!(
        view.operations()
            .iter()
            .any(|operation| operation.address == bump),
        "the view offers what a person's window offers"
    );
    view.invoke(&bump, &serde_json::Map::new(), false).unwrap();
    assert!(view.landed().is_some(), "the action ran in the view");
    let changes = view.take_changes().expect("the agent's own change");
    assert!(view.take_changes().is_none(), "nothing is taken twice");

    assert!(
        matches!(changes, DocumentMutation::Incremental(_)),
        "an existing record answers with increments"
    );
    let (DocumentMutation::Incremental(bytes) | DocumentMutation::Snapshot(bytes)) = changes;
    let mut peer = app.peer_document();
    tilo_automerge::ApplyIncoming::load_incoming(&mut peer, &bytes).unwrap();
    app.merge_peer(&mut peer).unwrap();
    assert_eq!(app.state().count, 1);
    let version = app.versions().into_iter().last().unwrap();
    assert_eq!(version.context.unwrap().invoker.unwrap().actor, AGENT);
}

#[test]
fn the_view_refuses_what_the_participant_may_not_do() {
    let app = TestApp::<TallyApp>::new().unwrap();
    let reader =
        PermissionContext::agent_granted(AGENT, &TreePath::root().app(TallyApp::ID), KindSet::READ);
    let mut view = attached(&app, reader);
    let bump = tilo_actions::ActionAddress::new("test.tally", "bump");
    assert!(view.invoke(&bump, &serde_json::Map::new(), false).is_err());
    assert!(view.take_changes().is_none());
}

/// What the view did and has not sent yet, merged into the person's window.
fn merge(app: &mut TestApp<TallyApp>, view: &mut AttachedView) {
    let Some(DocumentMutation::Incremental(bytes) | DocumentMutation::Snapshot(bytes)) =
        view.take_changes()
    else {
        return;
    };
    let mut peer = app.peer_document();
    tilo_automerge::ApplyIncoming::load_incoming(&mut peer, &bytes).unwrap();
    app.merge_peer(&mut peer).unwrap();
}

fn count(view: &AttachedView) -> i64 {
    view.read(|doc| <Tally as tilo_resource::Resource>::read(doc).unwrap().count)
        .unwrap()
}

#[test]
fn history_names_the_agent_its_app_and_the_run_it_acts_in() {
    let mut app = TestApp::<TallyApp>::new().unwrap();
    app.invoke(Tally::RENAME.with("visitors")).unwrap();
    let mut view = attached(&app, writer());
    view.set_context(Some(tilo_actions::ActionContext::default().with_group(
        "run-1",
        "Count",
        "agent-run",
    )));
    view.invoke_typed(&Tally::BUMP.invoke(), false).unwrap();
    view.set_context(None);
    assert_eq!(count(&view), 1);

    merge(&mut app, &mut view);
    assert_eq!(app.state().count, 1);
    let version = app.versions().into_iter().last().unwrap();
    let context = version.context.unwrap();
    let Some(tilo_actions::Did::Verb(verb)) = &context.what else {
        panic!("the agent's edit records its verb: {context:?}");
    };
    assert_eq!(verb.action.action, "bump");
    assert_eq!(context.invoker.unwrap().actor, AGENT);
    assert_eq!(context.app.unwrap().id, TallyApp::ID);
    assert_eq!(context.group.unwrap().id, "run-1");
}

#[test]
fn an_agent_acting_for_a_person_is_recorded_as_such() {
    let mut app = TestApp::<TallyApp>::new().unwrap();
    let definition = definition::<TallyApp>();
    let agent = tilo_actions::ActionInvoker::actor(AGENT)
        .with_kind(tilo_actions::ActorKind::Agent)
        .acting_for("ada");
    let mut view = AttachedView::open_for(
        definition,
        "test-workspace",
        RecordCheckout::new(app.record().clone(), definition.record_kind.clone()),
        app.read_document(|doc| doc.clone().save()),
        &agent,
        writer(),
        false,
    )
    .unwrap();
    view.invoke_typed(&Tally::BUMP.invoke(), false).unwrap();
    merge(&mut app, &mut view);
    let invoker = app
        .versions()
        .into_iter()
        .last()
        .and_then(|version| version.context)
        .and_then(|context| context.invoker)
        .unwrap();
    assert_eq!(invoker, agent, "the agent, for the person who asked");
}

#[test]
fn several_typed_edits_apply_as_one_step() {
    let mut app = TestApp::<TallyApp>::new().unwrap();
    let mut view = attached(&app, writer());
    let before = app.versions().len();
    view.invoke_typed(
        &Invoke::group([Tally::RENAME.with("guests"), Tally::RENAME.with("members")]),
        false,
    )
    .unwrap();
    merge(&mut app, &mut view);
    assert_eq!(app.state().name, "members");
    assert_eq!(app.versions().len(), before + 1, "a group is one version");
}

#[test]
fn a_reader_is_offered_no_edit() {
    let app = TestApp::<TallyApp>::new().unwrap();
    let reader =
        PermissionContext::agent_granted(AGENT, &TreePath::root().app(TallyApp::ID), KindSet::READ);
    let mut view = attached(&app, reader);
    assert!(view.operations().is_empty());
    assert!(matches!(
        view.invoke_typed(&Tally::BUMP.invoke(), false),
        Err(tilo_actions::door::DoorError::Unknown(_))
    ));
    assert_eq!(count(&view), 0);
}

#[test]
fn changes_of_another_record_are_left_alone() {
    use tilo_app::automerge::transaction::Transactable as _;
    let mut app = TestApp::<TallyApp>::new().unwrap();
    let mut view = attached(&app, writer());
    let mut other = tilo_resource::Doc::new();
    other
        .put(tilo_app::automerge::ROOT, "name", "elsewhere")
        .unwrap();
    other.commit();
    let first = other.get_heads();
    other
        .put(tilo_app::automerge::ROOT, "count", 9_i64)
        .unwrap();
    other.commit();
    let unrelated = other.save_after(&first);
    assert!(
        !view
            .merge(&DocumentMutation::Incremental(unrelated))
            .unwrap()
    );
    assert_eq!(count(&view), 0);

    app.invoke(Tally::BUMP.invoke()).unwrap();
    let ours = app.read_document(|doc| doc.clone().save());
    assert!(view.merge(&DocumentMutation::Snapshot(ours)).unwrap());
    assert_eq!(count(&view), 1);
}

#[test]
fn a_run_is_recorded_in_the_coordinating_record_by_the_agent() {
    let mut app = TestApp::<TallyApp>::new().unwrap();
    let mut view = attached(&app, writer());
    view.invoke_typed(&Tally::BUMP.invoke(), false).unwrap();
    let last = view
        .read(|doc| tilo_resource::history::versions(&mut doc.clone()))
        .unwrap()
        .into_iter()
        .last()
        .unwrap();
    let end = tilo_app::history_source::entry_id_from_hash(&last.hash.to_string());
    let steps = [tilo_app::RunStep {
        app: TallyApp::ID.into(),
        record: String::new(),
        run: tilo_app::ChangeRun::single(end),
        what: vec![tilo_actions::Described::new(tilo_actions::Did::verb(
            tilo_actions::ActionKey::new(TallyApp::ID, "document", "bump"),
            [],
        ))],
    }];
    assert!(view.record_run("Count", None, None, &steps));
    assert!(!view.record_run("Nothing", None, None, &[]));

    merge(&mut app, &mut view);
    let runs = app.read_document(tilo_app::read_runs);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs.first().unwrap().label, "Count");
}

#[test]
fn a_grant_of_one_action_reaches_that_action_only() {
    use tilo_app::permissions::policy::Level;
    let app = TestApp::<TallyApp>::new().unwrap();
    let only_bump = PermissionContext::agent_granted(
        AGENT,
        &TreePath::root()
            .app(TallyApp::ID)
            .any(Level::Kind)
            .resource("test.tally")
            .action("bump"),
        KindSet::READ_UNDOABLE,
    );
    let mut view = attached(&app, only_bump);
    let actions: Vec<String> = view
        .operations()
        .into_iter()
        .map(|operation| operation.address.action)
        .collect();
    assert_eq!(actions, ["bump"]);
    view.invoke_typed(&Tally::BUMP.invoke(), false).unwrap();
    assert_eq!(count(&view), 1);
    assert!(
        view.invoke_typed(&Tally::RENAME.with("guests"), false)
            .is_err()
    );
    assert_eq!(count(&view), 1);
}

/// A bench whose view sits whoever opens it down: the mistake the attached
/// view refuses in a record its participant joined.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Bench {
    pub seated: i64,
}

#[resource(kind = "test.bench", label = "Bench", app = "so.tilo.test-bench")]
impl Bench {
    /// Sit down.
    #[action]
    pub fn sit(&mut self) {
        self.seated = self.seated.saturating_add(1);
    }
}

pub struct BenchApp;

impl ResourceApp for BenchApp {
    type Resource = Bench;
    const ID: &'static str = "so.tilo.test-bench";
    const TITLE: &'static str = "Bench";

    fn view(cx: &mut ViewCx<Bench>) -> Element {
        cx.dispatcher().submit(Bench::SIT.invoke());
        Box::new(cx.button("Sit", Bench::SIT.invoke()))
    }

    fn label(_: &Bench) -> String {
        "Bench".into()
    }
}

/// Only a record's creator writes its initial content: a view that writes
/// as it opens a record its participant joined is refused, naming the app,
/// instead of keeping a change no peer would ever be sent.
#[test]
fn a_view_that_writes_as_it_opens_a_joined_record_is_refused() {
    let app = TestApp::<BenchApp>::new().unwrap();
    let definition = definition::<BenchApp>();
    let open = |created| {
        AttachedView::open(
            definition,
            "test-workspace",
            RecordCheckout::new(app.record().clone(), definition.record_kind.clone()),
            app.read_document(|doc| doc.clone().save()),
            AGENT,
            PermissionContext::permissive(),
            created,
        )
    };
    let Err(refusal) = open(false) else {
        panic!("a joined bench opened although its view sat the agent down");
    };
    assert!(refusal.contains(BenchApp::ID), "{refusal}");
    assert!(refusal.contains("`sit`"), "{refusal}");
    assert!(refusal.contains("joined"), "{refusal}");

    // The record's creator writes what it starts with.
    assert!(open(true).is_ok());

    // A view that writes nothing on open joins as before.
    let tally = TestApp::<TallyApp>::new().unwrap();
    assert!(attached(&tally, writer()).take_changes().is_none());
}

/// A ledger whose view looks up what is asked for off the UI thread, as a
/// viewer reads the markup a file already carries, and records the answer
/// once it lands.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Ledger {
    pub asked: String,
    pub answer: String,
}

#[resource(kind = "test.ledger", label = "Ledger", app = "so.tilo.test-ledger")]
impl Ledger {
    /// Ask for an entry.
    #[action]
    pub fn ask(&mut self, entry: String) {
        self.asked = entry;
    }

    /// Record what looking an entry up answered.
    #[action]
    pub fn answer(&mut self, answer: String) {
        self.answer = answer;
    }
}

/// The ledger's view: asks a worker for each entry asked for, owing the
/// window its reply, and records the reply in the frame after it lands.
struct Lookup {
    dispatcher: tilo_app::framework::Dispatcher<Ledger>,
    asked: String,
    replies: Option<std::sync::mpsc::Receiver<String>>,
    rect: Rect,
}

impl Widget for Lookup {
    fn sync(&mut self, state: &tilo_app::widget::AppModel, path: &tilo_app::widget::ModelPath) {
        // An answer is taken up before anything is asked, so the answer to
        // what this frame asks lands after it, in a frame of its own.
        if let Some(answer) = self
            .replies
            .as_ref()
            .and_then(|replies| replies.try_recv().ok())
        {
            self.replies = None;
            self.dispatcher.submit(Ledger::ANSWER.with(answer));
        }
        let (doc, _) = tilo_app::doc::document_ref(state, path);
        let Ok(ledger) = <Ledger as tilo_resource::Resource>::read(doc) else {
            return;
        };
        if ledger.asked == self.asked {
            return;
        }
        self.asked.clone_from(&ledger.asked);
        let (reply, replies) = tilo_app::wake::waking_channel();
        let owed = tilo_app::wake::WakeHandle::new().owe();
        std::thread::spawn(move || {
            let _ = reply.send(format!("{}: found", ledger.asked));
            drop(owed);
        });
        self.replies = Some(replies);
    }

    fn build_layout(
        &mut self,
        taffy: &mut tilo_app::advanced::TaffyTree<()>,
        _: &Theme,
        _: &mut std::collections::HashMap<tilo_app::advanced::NodeId, *mut dyn Widget>,
    ) -> tilo_app::advanced::NodeId {
        taffy.new_leaf(tilo_app::taffy::Style::default()).unwrap()
    }

    fn apply_layout(
        &mut self,
        _: &tilo_app::advanced::TaffyTree<()>,
        _: tilo_app::advanced::NodeId,
        origin: Point,
    ) {
        self.rect = Rect::from_origin_size(origin, Size::ZERO);
    }

    fn name(&self) -> &'static str {
        "Lookup"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

pub struct LedgerApp;

impl ResourceApp for LedgerApp {
    type Resource = Ledger;
    const ID: &'static str = "so.tilo.test-ledger";
    const TITLE: &'static str = "Ledger";

    fn view(cx: &mut ViewCx<Ledger>) -> Element {
        Box::new(Lookup {
            dispatcher: cx.dispatcher(),
            asked: String::new(),
            replies: None,
            rect: Rect::ZERO,
        })
    }

    fn label(_: &Ledger) -> String {
        "Ledger".into()
    }
}

/// An action answers once the work it caused in the app has landed: the
/// reply a worker owed the view is taken up and its change made before the
/// action answers, so the change is the action's, never the next one's.
#[test]
fn an_action_answers_once_the_work_it_caused_has_landed() {
    let app = TestApp::<LedgerApp>::new().unwrap();
    let definition = definition::<LedgerApp>();
    let mut view = AttachedView::open(
        definition,
        "test-workspace",
        RecordCheckout::new(app.record().clone(), definition.record_kind.clone()),
        app.read_document(|doc| doc.clone().save()),
        AGENT,
        PermissionContext::permissive(),
        false,
    )
    .unwrap();
    let answer = |view: &AttachedView| {
        view.read(|doc| {
            <Ledger as tilo_resource::Resource>::read(doc)
                .unwrap()
                .answer
        })
        .unwrap()
    };
    for entry in ["lamps", "chairs"] {
        view.invoke_typed(&Ledger::ASK.with(entry), false).unwrap();
        assert_eq!(answer(&view), format!("{entry}: found"));
        let changed = view.take_changes();
        assert!(changed.is_some(), "the ask and its answer leave together");
        assert!(view.take_changes().is_none());
    }
    let did: Vec<String> = view
        .read(|doc| tilo_resource::history::versions(&mut doc.clone()))
        .unwrap()
        .into_iter()
        .filter_map(|version| match version.context?.what? {
            tilo_actions::Did::Verb(verb) => Some(verb.action.action),
            _ => None,
        })
        .collect();
    assert_eq!(did, ["ask", "answer", "ask", "answer"]);
}
