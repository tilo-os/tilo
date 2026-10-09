#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! A destructive verb a button submits is confirmed by the person and then
//! applied, however long they take to confirm it.

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

#[test]
fn a_destructive_verb_confirmed_later_still_applies() {
    let mut app = TestApp::<NotesApp>::new().unwrap();
    app.invoke(Notes::ADD.with("Milk")).unwrap();
    let mut app = app.reopen().unwrap();
    app.harness().click_named("remove").unwrap();
    assert_eq!(
        app.state().notes.len(),
        1,
        "nothing happens before confirming"
    );
    // The person reads the prompt before confirming it.
    std::thread::sleep(std::time::Duration::from_millis(20));
    app.harness().type_text("CONFIRM");
    app.harness().key("Enter");
    assert!(
        app.state().notes.is_empty(),
        "the confirmed verb applied: {:?}",
        app.harness().control_failures()
    );
}

#[test]
fn a_destructive_verb_without_arguments_asks_and_applies() {
    let mut app = TestApp::<NotesApp>::new().unwrap();
    app.invoke(Notes::ADD.with("Milk")).unwrap();
    app.invoke(Notes::ADD.with("Eggs")).unwrap();
    app.harness().click_named("clear").unwrap();
    assert_eq!(
        app.state().notes.len(),
        2,
        "nothing happens before confirming"
    );
    app.harness().type_text("CONFIRM");
    app.harness().key("Enter");
    assert!(
        app.state().notes.is_empty(),
        "the confirmed verb applied: {:?}",
        app.harness().control_failures()
    );
}

/// What a surface applies now on its next settle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Now {
    Verb,
    Removal,
    Group,
    Typed,
    Queued,
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

    fn draw(
        &mut self,
        _: &mut tilo_app::advanced::Scene,
        _: tilo_app::advanced::Affine,
        _: &mut TextRenderer,
        _: &Theme,
        _: &PresenceCtx,
    ) {
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
            Now::Queued => {
                dispatch.invoke(Invoke::edit(&Notes::paths().notes.remove(&first)).unwrap());
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

/// Applying `now` immediately waits for the person's consent: nothing is
/// removed until they confirm, and then it is.
fn asks_before_applying_now(now: Now) {
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
    assert_eq!(
        app.state().notes.len(),
        2,
        "{now:?}: nothing is removed before the person confirms"
    );
    let asked = app.asking().expect("the host asks").0;
    assert_eq!(asked.resource, "test.notes", "{now:?}");
    assert!(app.confirm_asked().is_some());
    assert_eq!(app.asking(), None, "{now:?}: answered once");
    assert_eq!(
        app.state().notes.len(),
        if now == Now::Group { 2 } else { 1 },
        "{now:?}: the confirmed removal applied: {:?}",
        app.harness().control_failures()
    );
    assert!(
        !app.state().notes.values().any(|note| note.text == "Milk"),
        "{now:?}: the first note is gone"
    );
}

#[test]
fn a_destructive_verb_applied_now_asks_first() {
    asks_before_applying_now(Now::Verb);
}

#[test]
fn a_removal_applied_now_asks_first() {
    asks_before_applying_now(Now::Removal);
}

#[test]
fn a_group_holding_a_removal_applied_now_asks_first() {
    asks_before_applying_now(Now::Group);
}

#[test]
fn a_typed_removal_applied_now_asks_first() {
    asks_before_applying_now(Now::Typed);
}

/// The queued door itself asks before a removal by field edit, though the
/// field edit's declaration asks nothing: these arguments remove.
#[test]
fn a_queued_removal_asks_first() {
    asks_before_applying_now(Now::Queued);
}
