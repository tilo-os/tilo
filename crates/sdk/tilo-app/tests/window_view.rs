#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! A surface's own presentation reopens with its window: the framework root
//! saves and restores what the surface keeps across restarts.

use std::rc::Rc;

use tilo_app::advanced::{Affine, NodeId, Scene, TaffyTree};
use tilo_app::application_view::{
    ApplicationFocusRestore, ApplicationLocalView, ApplicationViewState,
};
use tilo_app::prelude::*;
use tilo_app::view::ThawError;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Shelf {
    pub books: Collection<Book>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Book {
    pub title: String,
}

#[resource(kind = "test.shelf", label = "Shelf")]
impl Shelf {
    /// Add a book.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, title: String) {
        self.books.push(cx.id(), Book { title });
    }
}

struct ShelfApp;

impl ResourceApp for ShelfApp {
    type Resource = Shelf;
    const ID: &'static str = "so.tilo.test-shelf";
    const TITLE: &'static str = "Shelf";

    fn view(cx: &mut ViewCx<Shelf>) -> Element {
        cx.surface(OpenBook::default())
    }
}

/// Shows the book someone opened, which is theirs, not the shelf's.
struct OpenBook {
    inner: Text,
    open: LocalState<String>,
    restored: bool,
}

thread_local! {
    /// The open book of the window built last, which the test changes as a
    /// person picking another book would.
    static OPEN: std::cell::RefCell<Option<LocalState<String>>> = const { std::cell::RefCell::new(None) };
}

impl Default for OpenBook {
    fn default() -> Self {
        let open = LocalState::new(String::new());
        OPEN.with(|slot| *slot.borrow_mut() = Some(open.clone()));
        Self {
            inner: Text::from_state(&open, |title| format!("Open: {title}")),
            open,
            restored: false,
        }
    }
}

impl Widget for OpenBook {
    tilo_app::provides!(dyn ApplicationViewState);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.inner.build_layout(taffy, theme, nodes)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.inner.apply_layout(taffy, node, origin);
    }

    fn sync(&mut self, state: &tilo_app::widget::AppModel, path: &tilo_app::widget::ModelPath) {
        self.inner.sync(state, path);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.inner.draw(scene, transform, text, theme, presence);
    }

    fn name(&self) -> &'static str {
        "OpenBook"
    }

    fn get_rect(&self) -> Rect {
        self.inner.get_rect()
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.inner]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.inner]
    }
}

impl ResourceSurface<Shelf> for OpenBook {
    fn present(&mut self, state: &Rc<Shelf>) {
        // A fresh window opens the first book; a reopened one keeps its own.
        if !self.restored && self.open.get().is_empty() {
            let first = state.books.values().next().map(|book| book.title.clone());
            self.open.set(first.unwrap_or_default());
        }
    }
}

impl ApplicationViewState for OpenBook {
    fn capture_application_view(&self) -> Result<Option<ApplicationLocalView>, ThawError> {
        Ok(Some(ApplicationLocalView {
            app_id: ShelfApp::ID.into(),
            version: 1,
            payload: self.open.get().into_bytes(),
        }))
    }

    fn restore_application_view(
        &mut self,
        local: &ApplicationLocalView,
        _doc: &tilo_app::automerge::AutoCommit,
    ) -> Result<ApplicationFocusRestore, ThawError> {
        let title = String::from_utf8(local.payload.clone())
            .map_err(|e| ThawError::State(e.to_string()))?;
        self.open.set(title);
        self.restored = true;
        Ok(ApplicationFocusRestore::Generic)
    }
}

fn open(app: &mut TestApp<ShelfApp>, title: &str) {
    OPEN.with(|slot| slot.borrow().as_ref().unwrap().set(title.to_owned()));
    app.harness().relayout();
}

#[test]
fn a_reopened_window_shows_the_book_it_had_open() {
    let mut app = TestApp::<ShelfApp>::new().unwrap();
    app.invoke(Shelf::ADD.with("Dune")).unwrap();
    app.invoke(Shelf::ADD.with("Emma")).unwrap();
    // A fresh window opens the first book.
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("Open: Dune")
    );
    open(&mut app, "Emma");
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("Open: Emma")
    );

    let mut app = app.reopen().unwrap();
    let frame = app.harness().inspection_json().unwrap();
    assert!(frame.contains("Open: Emma"), "{frame}");
}

#[test]
fn a_test_reaches_the_surface_its_view_hosts() {
    let mut app = TestApp::<ShelfApp>::new().unwrap();
    app.invoke(Shelf::ADD.with("Dune")).unwrap();
    let open = app.surface(|book: &mut OpenBook| book.open.get()).unwrap();
    assert_eq!(open, "Dune");
}
