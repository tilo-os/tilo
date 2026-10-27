//! A `render` that names an item answers with that item's view: the app shows
//! the item first, as picking its search hit would, before it draws.

use std::cell::RefCell;

use serde_json::json;
use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Page {
    #[model(label)]
    pub title: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Notebook {
    pub pages: Collection<Page>,
}

#[resource(kind = "test.notebook", label = "Notebook")]
impl Notebook {
    /// Write a page.
    #[action]
    pub fn write(&mut self, cx: &mut ActionCx, title: String) {
        self.pages.push(cx.id(), Page { title });
    }
}

thread_local! {
    /// Every anchor the view was asked to show, in order.
    static SHOWN: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

struct NotebookApp;

impl ResourceApp for NotebookApp {
    type Resource = Notebook;
    const ID: &'static str = "so.tilo.test-notebook";
    const TITLE: &'static str = "Notebook";

    fn view(cx: &mut ViewCx<Notebook>) -> Element {
        cx.on_open(|anchor| {
            SHOWN.with(|shown| shown.borrow_mut().push(anchor.to_owned()));
            if anchor == "page:gone" {
                Err(Refusal::new("that page is gone"))
            } else {
                Ok(())
            }
        });
        Box::new(Column::new())
    }
}

fn shown() -> Vec<String> {
    SHOWN.with(|shown| shown.borrow().clone())
}

#[test]
fn rendering_an_item_shows_that_item_first() {
    let mut app = TestApp::<NotebookApp>::new().unwrap();
    let drawn = app
        .call("render", json!({ "anchor": "page:2" }))
        .expect("a view of the page")
        .expect("render answers with the picture");
    let address = drawn["content"].as_str().expect("a stored picture");
    assert!(
        tilo_app::tilo_actions::content_hash(address).is_some(),
        "the picture has a content address: {address}"
    );
    assert_eq!(shown(), ["page:2"], "the app showed the asked-for item");
}

#[test]
fn rendering_without_an_item_draws_what_the_window_shows() {
    let mut app = TestApp::<NotebookApp>::new().unwrap();
    app.call("render", json!({}))
        .expect("a view of the window")
        .expect("render answers with the picture");
    assert!(shown().is_empty(), "nothing was shown: {:?}", shown());
}

#[test]
fn rendering_an_item_the_app_cannot_show_is_refused() {
    let mut app = TestApp::<NotebookApp>::new().unwrap();
    let refused = app.call("render", json!({ "anchor": "page:gone" }));
    assert!(
        matches!(&refused, Err(error) if error.to_string().contains("that page is gone")),
        "{refused:?}"
    );
}
