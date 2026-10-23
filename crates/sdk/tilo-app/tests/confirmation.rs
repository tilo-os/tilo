#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! A destructive verb of a document is undoable: from whatever path it
//! comes it applies at once, nobody is asked, and Undo takes it back. Only
//! what Undo cannot take back is confirmed (the palette's tests cover that).

use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Note {
    pub text: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Notes {
    pub notes: Collection<Note>,
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
