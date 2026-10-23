#![allow(clippy::unwrap_used, reason = "test assertions")]

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::json;
use tilo_app::advanced::{NodeId, TaffyTree};
use tilo_app::framework::EffectAnswer;
use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Book {
    #[model(label)]
    pub title: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Library {
    pub books: Collection<Book>,
}

#[resource(kind = "test.lazy-library", label = "Library")]
impl Library {
    /// Add books.
    #[action]
    pub fn add_many(&mut self, cx: &mut ActionCx, count: usize) {
        for n in 0..count {
            self.books.push(
                cx.id(),
                Book {
                    title: format!("Book {n}"),
                },
            );
        }
    }
}

/// What the shelf does with a book it shows.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "test.lazy-library.shelf")]
enum ShelfEffect {
    /// Open the book.
    #[effect(view, row)]
    Open { book: String },
}

thread_local! {
    static OPENED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

#[derive(Default)]
struct Shelf {
    rect: Rect,
}

impl Widget for Shelf {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        _: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.new_leaf(tilo_app::taffy::Style::default()).unwrap()
    }

    fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, origin: Point) {
        self.rect = Rect::from_origin_size(origin, Size::ZERO);
    }

    fn name(&self) -> &'static str {
        "Shelf"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ResourceSurface<Library, ShelfEffect> for Shelf {
    fn present(&mut self, _: &Rc<Library>) {}

    fn effect(&mut self, effect: ShelfEffect, _: &mut EffectCx<Library, Self>) -> EffectAnswer {
        let ShelfEffect::Open { book } = effect;
        OPENED.with(|opened| opened.borrow_mut().push(book));
        Ok(Effected::done())
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        let (_, book) = tilo_resource::items::item_of_resource::<Library>(id)?;
        Some(
            LiveRow::new(id, "test.lazy-library.books", "Book").effect(&ShelfEffect::Open {
                book: book.to_string(),
            }),
        )
    }
}

struct LibraryApp;

impl ResourceApp for LibraryApp {
    type Resource = Library;
    const ID: &'static str = "so.tilo.test-lazy-library";
    const TITLE: &'static str = "Library";

    fn view(cx: &mut ViewCx<Library>) -> Element {
        cx.surface(Shelf::default())
    }
}

#[test]
fn an_item_past_the_published_page_keeps_its_row_effects() {
    let mut app = TestApp::<LibraryApp>::new().unwrap();
    let count = tilo_resource::items::ITEM_PAGE + 10;
    app.invoke(Library::ADD_MANY.with(count)).unwrap();
    let last = app.state().books.ids().last().cloned().unwrap();
    let resource = format!("test.lazy-library/books/{last}");
    app.call_on(&resource, "open", json!({})).unwrap();
    assert_eq!(
        OPENED.with(|opened| opened.borrow().clone()),
        [last.to_string()]
    );
}
