#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! A role change while the app runs reaches views, surfaces and the
//! dispatcher without reopening the app.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Memo {
    pub title: String,
    pub lines: Collection<String>,
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
}

type Changes = Rc<RefCell<Vec<bool>>>;

thread_local! {
    static PROBE_CHANGES: RefCell<Option<Changes>> = const { RefCell::new(None) };
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

    fn attach(&mut self, _: SurfaceCx<Memo>) {
        PROBE_CHANGES.with(|probe| *probe.borrow_mut() = Some(self.changes.clone()));
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
