#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! A role change while the app runs reaches views, surfaces and the
//! dispatcher without reopening the app.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_app::framework::{Participant, resource_kind};
use tilo_app::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Memo {
    pub title: String,
    pub lines: Collection<String>,
    pub remarks: Collection<String>,
}

#[resource(kind = "test.memo", label = "Memo")]
impl Memo {
    /// Retitle the memo.
    #[action]
    pub fn retitle(&mut self, title: String) {
        self.title = title;
    }

    /// Add a line.
    #[action]
    pub fn add_line(&mut self, cx: &mut ActionCx, line: String) {
        self.lines.push(cx.id(), line);
    }

    /// Remark on the memo: someone who may only comment may.
    #[action(permission = "comment")]
    pub fn remark(&mut self, cx: &mut ActionCx, remark: String) {
        self.remarks.push(cx.id(), remark);
    }
}

type Changes = Rc<RefCell<Vec<bool>>>;

thread_local! {
    static PROBE_CHANGES: RefCell<Option<Changes>> = const { RefCell::new(None) };
    static PROBE_DISPATCHER: RefCell<Option<Dispatcher<Memo>>> = const { RefCell::new(None) };
}

struct MemoApp;

impl ResourceApp for MemoApp {
    type Resource = Memo;
    const ID: &'static str = "so.tilo.test-memo";
    const TITLE: &'static str = "Memo";

    fn view(cx: &mut ViewCx<Memo>) -> Element {
        Box::new(
            Column::new()
                .push(Box::new(
                    cx.input(|memo| memo.title.clone(), Memo::RETITLE)
                        .with_id("title"),
                ))
                .push(Box::new(
                    Text::from_state(&cx.read_only_state(), |read_only| {
                        if *read_only { "View only" } else { "Editing" }.to_owned()
                    })
                    .with_id("access"),
                ))
                .push(cx.surface(AccessProbe::default())),
        )
    }
}

/// A custom surface that records what it was told about permissions.
#[derive(Default)]
struct AccessProbe {
    rect: Rect,
    changes: Changes,
}

impl Widget for AccessProbe {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.new_leaf(tilo_app::taffy::Style::default()).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, origin: Point) {
        self.rect = Rect::from_origin_size(origin, Size::ZERO);
    }

    fn draw(&mut self, _: &mut Scene, _: Affine, _: &mut TextRenderer, _: &Theme, _: &PresenceCtx) {
    }

    fn name(&self) -> &'static str {
        "AccessProbe"
    }

    fn test_id(&self) -> Option<&str> {
        Some("access-probe")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ResourceSurface<Memo> for AccessProbe {
    fn present(&mut self, _: &Rc<Memo>) {}

    fn attach(&mut self, cx: SurfaceCx<Memo>) {
        PROBE_CHANGES.with(|probe| *probe.borrow_mut() = Some(self.changes.clone()));
        PROBE_DISPATCHER.with(|probe| *probe.borrow_mut() = Some(cx.dispatcher()));
    }

    fn read_only_changed(&mut self, read_only: bool) {
        self.changes.borrow_mut().push(read_only);
    }
}

#[test]
fn a_role_change_takes_effect_while_the_app_runs() {
    let mut app = TestApp::<MemoApp>::new().unwrap();
    let changes = PROBE_CHANGES.with(|probe| probe.borrow().clone()).unwrap();
    assert!(
        app.harness()
            .semantic_owner_instance("access-probe")
            .is_some(),
        "a surface is addressed by name like any widget"
    );
    app.invoke(Memo::ADD_LINE.with("milk".into())).unwrap();
    assert!(app.harness().inspection_json().unwrap().contains("Editing"));

    // The owner makes this participant a viewer while the app is open.
    app.set_role(tilo_app::Role::Viewer);
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("View only")
    );
    assert_eq!(*changes.borrow(), [true], "the surface is told");
    assert!(app.invoke(Memo::ADD_LINE.with("sneaky".into())).is_err());
    app.harness().click_named("title").unwrap();
    app.harness().type_text("typed while viewing");
    assert_eq!(app.state().title, "");
    assert_eq!(app.state().lines.len(), 1);

    // And back: editing works again without reopening the app.
    app.set_role(tilo_app::Role::Editor);
    assert!(app.harness().inspection_json().unwrap().contains("Editing"));
    assert_eq!(*changes.borrow(), [true, false]);
    app.invoke(Memo::ADD_LINE.with("eggs".into())).unwrap();
    app.harness().click_named("title").unwrap();
    app.harness().type_text("Groceries");
    assert_eq!(app.state().title, "Groceries");
    assert_eq!(app.state().lines.len(), 2);
}

#[test]
fn a_commenter_may_take_the_actions_that_need_only_commenting() {
    let mut app = TestApp::<MemoApp>::new().unwrap();
    let dispatcher = PROBE_DISPATCHER
        .with(|probe| probe.borrow().clone())
        .unwrap();
    app.set_role(tilo_app::Role::Commenter);
    assert!(dispatcher.read_only(), "a commenter may not write");

    app.invoke(Memo::REMARK.with("looks good".into()))
        .expect("a commenter remarks through the door");
    assert!(
        app.invoke(Memo::ADD_LINE.with("sneaky".into())).is_err(),
        "but adds no line"
    );
    assert_eq!(app.state().remarks.len(), 1);
    assert!(app.state().lines.is_empty());

    // A surface asks the dispatcher what this participant may apply, and an
    // immediate edit follows the action's own level.
    let remark = Memo::REMARK.with("typed".into()).build(0).unwrap();
    let line = Memo::ADD_LINE.with("typed".into()).build(0).unwrap();
    assert!(dispatcher.may_apply(&remark));
    assert!(!dispatcher.may_apply(&line));
    assert!(dispatcher.may(PermissionRequirement::Comment));
    assert!(!dispatcher.may(PermissionRequirement::Write));

    // A viewer may do neither.
    app.set_role(tilo_app::Role::Viewer);
    assert!(app.invoke(Memo::REMARK.with("no".into())).is_err());
    assert!(!dispatcher.may_apply(&remark));
}

#[test]
fn a_commenters_typed_edit_needs_what_its_value_needs() {
    // Through the window's door: the generic `apply` is offered to a
    // commenter, and admits the values that need only commenting.
    let mut app = TestApp::<MemoApp>::new().unwrap();
    app.set_role(tilo_app::Role::Commenter);
    let remark = Memo::REMARK.with("typed remark".into()).build(0).unwrap();
    let line = Memo::ADD_LINE.with("typed line".into()).build(0).unwrap();
    app.invoke(Invoke::apply(&remark).unwrap())
        .expect("a commenter's typed remark is admitted");
    assert!(
        app.invoke(Invoke::apply(&line).unwrap()).is_err(),
        "a commenter's typed line is refused"
    );
    let group = Invoke::<Memo>::group([
        Invoke::apply(&remark).unwrap(),
        Invoke::apply(&line).unwrap(),
    ]);
    assert!(
        app.invoke(group).is_err(),
        "a group needs what its most demanding member needs"
    );
    assert_eq!(app.state().remarks.len(), 1);
    assert!(app.state().lines.is_empty());

    // Through a participant without a window, by the same rule.
    let snapshot = app.read_document(|doc| doc.clone().save());
    let commenting = PermissionContext::from_capabilities(
        Actor::agent("scribe"),
        vec![CapOverride::grant(
            Subject::agent("scribe"),
            Scope::App(MemoApp::ID.to_owned()),
            Verb::Comment,
        )],
    );
    let mut participant =
        Participant::from_snapshot(resource_kind::<MemoApp>(), &snapshot, "scribe", commenting)
            .unwrap();
    participant
        .apply_typed::<Memo>(&remark)
        .expect("the participant remarks");
    assert!(participant.apply_typed::<Memo>(&line).is_err());
    assert_eq!(participant.state::<Memo>().unwrap().remarks.len(), 2);
    assert!(participant.state::<Memo>().unwrap().lines.is_empty());
}
