//! `outline`: an app answers what its window shows as lines an agent reads —
//! every label, value and control in reading order, each control's action —
//! and, asked for, the same view as a picture.

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

#[resource(kind = "test.outline-notebook", catalogue = "tests/locales")]
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
    const ID: &'static str = "so.tilo.test-outline-notebook";
    const TITLE: &'static str = "Notebook";

    fn view(cx: &mut ViewCx<Notebook>) -> Element {
        cx.on_open(|anchor| {
            SHOWN.with(|shown| shown.borrow_mut().push(anchor.to_owned()));
            Ok::<(), Refusal>(())
        });
        Box::new(
            Column::new()
                .push(Box::new(Text::new("Pages")))
                .push(Box::new(
                    cx.button("Write a page", Notebook::WRITE.with("Groceries")),
                )),
        )
    }
}

fn shown() -> Vec<String> {
    SHOWN.with(|shown| shown.borrow().clone())
}

#[test]
fn an_outline_reads_what_the_window_shows() {
    let mut app = TestApp::<NotebookApp>::new().unwrap();
    let read = app
        .call("outline", json!({}))
        .expect("the window's lines")
        .expect("an outline answers");
    let text = read["outline"].as_str().expect("the lines as text");
    assert!(
        text.contains("static_text \"Pages\"") && text.contains("button \"Write a page\""),
        "the window's words, in reading order: {text}"
    );
    // Every line stands in the view, so a caller acting on one knows where it
    // is, and can aim a click at a person's window.
    let tree = read["tree"].as_array().expect("the tree");
    let line = tree
        .iter()
        .flat_map(flatten)
        .find(|line| line["name"] == "Write a page")
        .expect("the button's line");
    let rect = line["rect"].as_array().expect("its bounds");
    assert!(rect[2].as_f64().unwrap_or(0.0) > 0.0, "{line}");
    assert!(
        line.get("action").is_some() || line.get("control").is_some(),
        "a control names what it does: {line}"
    );
    assert!(
        read["window_origin"]
            .as_array()
            .is_some_and(|at| at.len() == 2),
        "the view says where it stands in the window: {read}"
    );
    assert!(shown().is_empty(), "nothing was asked to show first");
}

fn flatten(line: &serde_json::Value) -> Vec<&serde_json::Value> {
    let mut all = vec![line];
    if let Some(children) = line["children"].as_array() {
        for child in children {
            all.extend(flatten(child));
        }
    }
    all
}

#[test]
fn a_control_in_an_outline_names_what_it_does() {
    let mut app = TestApp::<NotebookApp>::new().unwrap();
    let read = app
        .call("outline", json!({}))
        .expect("the window's lines")
        .expect("an outline answers");
    assert_eq!(read["nodes"].as_u64(), Some(2), "{read}");
}

#[test]
fn an_outline_asked_for_a_picture_answers_one() {
    let mut app = TestApp::<NotebookApp>::new().unwrap();
    let read = app
        .call("outline", json!({ "picture": true }))
        .expect("the window's lines and its picture")
        .expect("an outline answers");
    let address = read["content"].as_str().expect("a stored picture");
    assert!(
        tilo_app::tilo_actions::content_hash(address).is_some(),
        "the picture has a content address: {address}"
    );
    assert!(
        read["outline"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
    );
    assert!(
        read["width"].as_u64().unwrap_or(0) > 0,
        "the picture has a size: {read}"
    );
}

#[test]
fn outlining_an_item_shows_that_item_first() {
    let mut app = TestApp::<NotebookApp>::new().unwrap();
    app.call("outline", json!({ "anchor": "page:2" }))
        .expect("the item's lines")
        .expect("an outline answers");
    assert_eq!(shown(), ["page:2"], "the app showed the asked-for item");
}

#[test]
fn an_outline_without_a_picture_draws_nothing() {
    let mut app = TestApp::<NotebookApp>::new().unwrap();
    let read = app
        .call("outline", json!({}))
        .expect("the window's lines")
        .expect("an outline answers");
    assert!(read.get("content").is_none(), "no picture was made: {read}");
}
