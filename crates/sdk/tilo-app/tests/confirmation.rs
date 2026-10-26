#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! A destructive verb of a document is undoable: from whatever path it
//! comes it applies at once, nobody is asked, and Undo takes it back. Only
//! what Undo cannot take back is confirmed, from a gesture and through the
//! door alike (and in the palette's tests).

use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Note {
    pub text: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Notes {
    pub notes: Collection<Note>,
    /// How long the notes stay open unattended: any change can weaken the
    /// person's security, so the door asks, though Undo takes it back.
    #[model(trust = "weakening")]
    pub open_minutes: u32,
}

#[resource(kind = "test.notes", label = "Notes")]
impl Notes {
    /// Add a note.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, text: String) {
        self.notes.push(cx.id(), Note { text });
    }

    /// Remove a note.
    #[action(destructive)]
    pub fn remove(&mut self, notes: &Id) -> Result<(), Refusal> {
        self.notes
            .remove(notes)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such note"))
    }

    /// Remove every note.
    #[action(destructive)]
    pub fn clear(&mut self) {
        self.notes = Collection::new();
    }

    /// Forget every note, with what each device kept beside the document:
    /// Undo brings the notes back but not what the devices forgot.
    #[action(destructive, irreversible)]
    pub fn forget(&mut self) {
        self.notes = Collection::new();
    }
}

struct NotesApp;

impl ResourceApp for NotesApp {
    type Resource = Notes;
    const ID: &'static str = "so.tilo.test-confirmation";
    const TITLE: &'static str = "Notes";

    fn view(cx: &mut ViewCx<Notes>) -> Element {
        let first = cx.read(|notes| notes.and_then(|notes| notes.notes.ids().next().cloned()));
        Box::new(tilo_app::column![
            cx.button(
                "remove",
                Notes::REMOVE.with(first.unwrap_or_else(|| Id::from_raw("")))
            ),
            cx.button("clear", Notes::CLEAR.with(())),
            cx.button("forget", Notes::FORGET.with(())),
            cx.button("keep open", Notes::paths().open_minutes.set(60)),
        ])
    }
}

/// An undoable destructive verb applies at once and offers Undo, with no
/// confirmation.
#[test]
fn an_undoable_destructive_verb_applies_at_once_and_undo_takes_it_back() {
    let mut app = TestApp::<NotesApp>::new().unwrap();
    app.invoke(Notes::ADD.with("Milk")).unwrap();
    let mut app = app.reopen().unwrap();
    app.harness().click_named("remove").unwrap();
    app.settle();
    assert_eq!(app.asking(), None, "nobody is asked");
    assert!(
        app.state().notes.is_empty(),
        "the verb applied at once: {:?}",
        app.harness().control_failures()
    );
    assert!(app.undo(), "Undo takes it back");
    assert_eq!(app.state().notes.len(), 1);
}

/// What Undo cannot take back is asked first through the door: the
/// person's press of a control for an irreversible destructive verb.
#[test]
fn an_irreversible_destructive_verb_through_the_door_asks_first() {
    let mut app = TestApp::<NotesApp>::new().unwrap();
    app.invoke(Notes::ADD.with("Milk")).unwrap();
    let mut app = app.reopen().unwrap();
    app.harness().click_named("forget").unwrap();
    app.settle();
    assert!(app.asking().is_some(), "the person is asked first");
    assert_eq!(app.state().notes.len(), 1, "nothing is forgotten yet");
}

#[test]
fn an_undoable_destructive_verb_without_arguments_applies_at_once() {
    let mut app = TestApp::<NotesApp>::new().unwrap();
    app.invoke(Notes::ADD.with("Milk")).unwrap();
    app.invoke(Notes::ADD.with("Eggs")).unwrap();
    app.harness().click_named("clear").unwrap();
    app.settle();
    assert_eq!(app.asking(), None, "nobody is asked");
    assert!(
        app.state().notes.is_empty(),
        "the verb applied at once: {:?}",
        app.harness().control_failures()
    );
    assert!(app.undo(), "Undo takes it back");
    assert_eq!(app.state().notes.len(), 2);
}

/// What a surface applies now on its next settle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Now {
    Verb,
    Removal,
    Group,
    Typed,
    Queued,
    Gesture,
    InvokedGesture,
    IrreversibleGesture,
    IrreversibleVerb,
    Confirming,
    IrreversibleConfirming,
}

thread_local! {
    static NOW: std::cell::Cell<Option<Now>> = const { std::cell::Cell::new(None) };
    static APPLIED: std::cell::RefCell<Vec<Result<(), String>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A surface that applies the first note's removal immediately, as an
/// editor applies its own edits.
#[derive(Default)]
struct Remover {
    rect: Rect,
    dispatch: Option<tilo_app::framework::Dispatcher<Notes>>,
    first: Option<Id>,
}

impl Widget for Remover {
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
        "Remover"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn settle(&mut self, edits: &mut tilo_app::widget::Edits<'_>, _: &tilo_app::automerge::ObjId) {
        let (Some(now), Some(dispatch), Some(first)) =
            (NOW.take(), self.dispatch.as_ref(), self.first.clone())
        else {
            return;
        };
        let applied = match now {
            Now::Verb => dispatch.submit_now(edits, Notes::REMOVE.with(first)),
            Now::Removal => dispatch.submit_now(edits, Notes::paths().notes.remove(&first)),
            Now::Group => dispatch.submit_now(
                edits,
                Invoke::group([
                    Invoke::edit(&Notes::paths().notes.remove(&first)).unwrap(),
                    Notes::ADD.with("Bread").into_invoke().unwrap(),
                ]),
            ),
            Now::Gesture => dispatch.submit_gesture(edits, Notes::REMOVE.with(first)),
            Now::IrreversibleGesture => dispatch.submit_gesture(edits, Notes::FORGET.with(())),
            Now::IrreversibleVerb => dispatch.submit_now(edits, Notes::FORGET.with(())),
            Now::Confirming => dispatch.submit_confirming(edits, Notes::REMOVE.with(first)),
            Now::IrreversibleConfirming => {
                dispatch.submit_confirming(edits, Notes::FORGET.with(()))
            }
            Now::InvokedGesture => {
                let _agent = tilo_app::invocation_scope::InvokerScope::enter(
                    tilo_actions::ActionInvoker::actor("agent:helper"),
                );
                dispatch.submit_gesture(edits, Notes::REMOVE.with(first))
            }
            Now::Queued => {
                dispatch.submit(Invoke::edit(&Notes::paths().notes.remove(&first)).unwrap());
                Ok(())
            }
            Now::Typed => {
                let action = Invoke::edit(&Notes::paths().notes.remove(&first))
                    .unwrap()
                    .build(0)
                    .unwrap();
                dispatch.apply_now(edits, &action)
            }
        };
        APPLIED.with(|applied_| {
            applied_
                .borrow_mut()
                .push(applied.map_err(|e| e.to_string()));
        });
    }
}

impl ResourceSurface<Notes> for Remover {
    fn present(&mut self, notes: &std::rc::Rc<Notes>) {
        self.first = notes.notes.ids().next().cloned();
    }

    fn attach(&mut self, cx: SurfaceCx<Notes>) {
        self.dispatch = Some(cx.dispatcher());
    }
}

struct RemoverApp;

impl ResourceApp for RemoverApp {
    type Resource = Notes;
    const ID: &'static str = "so.tilo.test-confirmation-now";
    const TITLE: &'static str = "Notes";

    fn view(cx: &mut ViewCx<Notes>) -> Element {
        cx.surface(Remover::default())
    }
}

/// Applying `now` immediately applies an undoable removal at once, from
/// whatever path it comes, and Undo takes it back.
fn applies_now_without_asking(now: Now) {
    let mut app = TestApp::<RemoverApp>::new().unwrap();
    app.invoke(Notes::ADD.with("Milk")).unwrap();
    app.invoke(Notes::ADD.with("Eggs")).unwrap();
    NOW.set(Some(now));
    app.settle();
    assert_eq!(
        APPLIED.with(|applied| applied.borrow().clone()),
        [Ok(())],
        "{now:?}"
    );
    assert_eq!(app.asking(), None, "{now:?}: nobody is asked");
    assert_eq!(
        app.state().notes.len(),
        if now == Now::Group { 2 } else { 1 },
        "{now:?}: the removal applied at once: {:?}",
        app.harness().control_failures()
    );
    assert!(
        !app.state().notes.values().any(|note| note.text == "Milk"),
        "{now:?}: the first note is gone"
    );
    assert!(app.undo(), "{now:?}: Undo takes it back");
    assert!(
        app.state().notes.values().any(|note| note.text == "Milk"),
        "{now:?}: the first note is back"
    );
}

/// The person's own gesture applies the verb at once, as any undoable
/// change does.
#[test]
fn a_destructive_verb_of_the_persons_own_gesture_applies_without_asking() {
    let mut app = TestApp::<RemoverApp>::new().unwrap();
    app.invoke(Notes::ADD.with("Milk")).unwrap();
    app.invoke(Notes::ADD.with("Eggs")).unwrap();
    NOW.set(Some(Now::Gesture));
    app.settle();
    assert_eq!(app.asking(), None, "nothing is asked");
    assert_eq!(app.state().notes.len(), 1, "removed at once");
    assert!(!app.state().notes.values().any(|note| note.text == "Milk"));
}

#[test]
fn an_undoable_gesture_under_an_invocation_applies_at_once() {
    applies_now_without_asking(Now::InvokedGesture);
}

#[test]
fn an_undoable_destructive_verb_applied_now_applies_at_once() {
    applies_now_without_asking(Now::Verb);
}

#[test]
fn an_undoable_removal_applied_now_applies_at_once() {
    applies_now_without_asking(Now::Removal);
}

#[test]
fn a_group_holding_an_undoable_removal_applies_at_once() {
    applies_now_without_asking(Now::Group);
}

#[test]
fn a_typed_undoable_removal_applied_now_applies_at_once() {
    applies_now_without_asking(Now::Typed);
}

/// The queued door applies a removal by field edit at once too: Undo takes
/// it back.
#[test]
fn a_queued_undoable_removal_applies_at_once() {
    applies_now_without_asking(Now::Queued);
}

/// Applying `now` an irreversible destructive verb asks the person first
/// and forgets nothing until they answer.
fn asks_before_forgetting(now: Now) {
    let mut app = TestApp::<RemoverApp>::new().unwrap();
    app.invoke(Notes::ADD.with("Milk")).unwrap();
    NOW.set(Some(now));
    app.settle();
    assert!(app.asking().is_some(), "{now:?}: the person is asked first");
    assert_eq!(
        app.state().notes.len(),
        1,
        "{now:?}: nothing is forgotten yet"
    );
}

/// What Undo cannot take back is asked first from the person's own gesture
/// too, as through the door: the gesture confirms only what has a way back.
#[test]
fn an_irreversible_destructive_verb_of_a_gesture_asks_first() {
    asks_before_forgetting(Now::IrreversibleGesture);
}

/// A surface applying an irreversible destructive verb now asks first too.
#[test]
fn an_irreversible_destructive_verb_applied_now_asks_first() {
    asks_before_forgetting(Now::IrreversibleVerb);
}

/// Hold Shift, or let go of it, as the window's modifier change says.
fn hold_shift<A: ResourceApp>(app: &mut TestApp<A>, held: bool) {
    let names: Vec<String> = if held {
        vec!["shift".into()]
    } else {
        Vec::new()
    };
    app.harness().set_modifiers(&names);
}

/// Pick the remover's choice `now` with Shift `held`, on a fresh window.
fn choose(now: Now, held: bool) -> TestApp<RemoverApp> {
    let mut app = TestApp::<RemoverApp>::new().unwrap();
    app.invoke(Notes::ADD.with("Milk")).unwrap();
    hold_shift(&mut app, held);
    NOW.set(Some(now));
    app.settle();
    assert_eq!(
        APPLIED.with(|applied| applied.borrow_mut().drain(..).collect::<Vec<_>>()),
        [Ok(())],
        "{now:?}"
    );
    app
}

/// A choice the app asks the person to confirm (Delete in a context menu)
/// is asked about though Undo would take it back, in the action's own
/// words; once they agree it applies, and Undo takes it back.
#[test]
fn a_confirming_choice_without_shift_asks_and_undo_takes_it_back() {
    let mut app = choose(Now::Confirming, false);
    assert!(app.asking().is_some(), "the person is asked first");
    assert_eq!(
        app.harness().asking_why(),
        None,
        "the action asks, not the room's grants"
    );
    assert_eq!(app.state().notes.len(), 1, "nothing is removed yet");
    assert!(app.confirm_asked().is_some(), "the person agrees");
    assert!(app.state().notes.is_empty(), "then it applies");
    assert!(app.undo(), "Undo takes it back");
    assert_eq!(app.state().notes.len(), 1);
}

/// Holding Shift, the same choice applies at once, nobody is asked, and
/// Undo takes it back.
#[test]
fn a_confirming_choice_with_shift_applies_at_once_and_undo_takes_it_back() {
    let mut app = choose(Now::Confirming, true);
    assert_eq!(app.asking(), None, "Shift skips the question");
    assert!(app.state().notes.is_empty(), "removed at once");
    assert!(app.undo(), "Undo takes it back");
    assert_eq!(app.state().notes.len(), 1);
    hold_shift(&mut app, false);
}

/// What Undo cannot take back is asked, Shift or not.
#[test]
fn an_irreversible_confirming_choice_asks_shift_or_not() {
    for held in [false, true] {
        let mut app = choose(Now::IrreversibleConfirming, held);
        assert!(app.asking().is_some(), "Shift {held}: the person is asked");
        assert_eq!(app.state().notes.len(), 1, "Shift {held}: nothing is lost");
        hold_shift(&mut app, false);
    }
}

/// Press "keep open", whose contract asks though Undo takes it back, with
/// Shift `held`, on a fresh window.
fn keep_open(held: bool) -> TestApp<NotesApp> {
    let mut app = TestApp::<NotesApp>::new().unwrap();
    app.invoke(Notes::ADD.with("Milk")).unwrap();
    let mut app = app.reopen().unwrap();
    hold_shift(&mut app, held);
    app.harness().click_named("field-edit").unwrap();
    app.settle();
    app
}

/// The host's own question, for an action whose contract asks, is asked
/// without Shift as before.
#[test]
fn the_hosts_question_asks_without_shift_as_before() {
    let mut app = keep_open(false);
    assert!(app.asking().is_some(), "without Shift it asks");
    assert_eq!(app.state().open_minutes, 0, "nothing changed yet");
}

/// With Shift the host's question is skipped for what Undo takes back: it
/// applies at once and Undo takes it back.
#[test]
fn shift_skips_the_hosts_question_for_what_undo_takes_back() {
    let mut app = keep_open(true);
    assert_eq!(app.asking(), None, "with Shift nobody is asked");
    assert_eq!(app.state().open_minutes, 60, "it applied at once");
    assert!(app.undo(), "Undo takes it back");
    assert_eq!(app.state().open_minutes, 0);
    hold_shift(&mut app, false);
}

/// What Undo cannot take back is asked by the host though Shift is held.
#[test]
fn shift_never_skips_the_hosts_question_for_what_undo_cannot_take_back() {
    let mut app = TestApp::<NotesApp>::new().unwrap();
    app.invoke(Notes::ADD.with("Milk")).unwrap();
    let mut app = app.reopen().unwrap();
    hold_shift(&mut app, true);
    app.harness().click_named("forget").unwrap();
    app.settle();
    assert!(app.asking().is_some(), "the person is asked first");
    assert_eq!(app.state().notes.len(), 1, "nothing is forgotten yet");
    hold_shift(&mut app, false);
}
