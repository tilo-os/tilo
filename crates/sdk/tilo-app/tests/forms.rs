#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! A form is a verb's parameters filled in locally: typing into it changes
//! no document, and submitting it invokes the verb once.

use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Bookmark {
    #[model(label)]
    pub title: String,
    pub url: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Bookmarks {
    pub bookmarks: Collection<Bookmark>,
}

#[resource(kind = "test.bookmarks", label = "Bookmarks")]
impl Bookmarks {
    /// Add a bookmark.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, url: String, title: String) -> Result<(), Refusal> {
        if url.trim().is_empty() {
            return Err(Refusal::new("a bookmark needs an address"));
        }
        self.bookmarks.push(cx.id(), Bookmark { title, url });
        Ok(())
    }
}

struct BookmarksApp;

impl ResourceApp for BookmarksApp {
    type Resource = Bookmarks;
    const ID: &'static str = "so.tilo.test-bookmarks";
    const TITLE: &'static str = "Bookmarks";

    fn view(cx: &mut ViewCx<Bookmarks>) -> Element {
        let form = cx.form::<bookmarks::Add>();
        let p = form.paths();
        Box::new(tilo_app::column![
            cx.input(p.url).with_id("url"),
            cx.input(p.title).with_id("title"),
            cx.button("add", form.submit()),
        ])
    }
}

fn heads(app: &TestApp<BookmarksApp>) -> Vec<tilo_app::automerge::ChangeHash> {
    app.read_document(|doc| tilo_app::tilo_resource::heads_of(doc).unwrap_or_default())
}

#[test]
fn typing_into_a_form_changes_no_document_and_submitting_invokes_the_verb_once() {
    let mut app = TestApp::<BookmarksApp>::new().unwrap();
    let versions = app.versions().len();
    let before = heads(&app);

    app.harness().click_named("url").unwrap();
    app.harness().type_text("https://tilo.so");
    app.harness().click_named("title").unwrap();
    app.harness().type_text("Tilo");
    assert_eq!(heads(&app), before, "typing into a form writes nothing");
    assert_eq!(app.versions().len(), versions);
    assert!(app.state().bookmarks.is_empty());

    app.harness().click_named("add").unwrap();
    let state = app.state();
    let added: Vec<&Bookmark> = state.bookmarks.values().collect();
    assert_eq!(
        added,
        [&Bookmark {
            title: "Tilo".into(),
            url: "https://tilo.so".into()
        }]
    );
    assert_eq!(app.versions().len(), versions + 1, "one invocation");

    // The form empties once sent; an empty one is refused by the verb.
    let shown = app.harness().inspection_json().unwrap();
    assert!(!shown.contains("https://tilo.so"), "{shown}");
    app.harness().click_named("add").unwrap();
    assert_eq!(app.state().bookmarks.len(), 1);
    assert_eq!(app.versions().len(), versions + 1);
}

/// The bookmarks again, showing the one a search hit or an agent opens.
struct OpeningApp;

thread_local! {
    static OPENED: std::cell::RefCell<Vec<Id>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl ResourceApp for OpeningApp {
    type Resource = Bookmarks;
    const ID: &'static str = "so.tilo.test-opening";
    const TITLE: &'static str = "Bookmarks";

    fn view(cx: &mut ViewCx<Bookmarks>) -> Element {
        let live = cx.live().clone();
        cx.on_open_item(move |item| {
            if !live.with(|bookmarks| bookmarks.bookmarks.contains(item)) {
                return Err(Refusal::new("that bookmark is gone"));
            }
            OPENED.with(|opened| opened.borrow_mut().push(item.clone()));
            Ok(())
        });
        Box::new(Column::new())
    }
}

#[test]
fn opening_an_item_shows_it_or_says_why_not() {
    let mut app = TestApp::<OpeningApp>::new().unwrap();
    app.invoke(Bookmarks::ADD.with(("https://tilo.so".to_owned(), "Tilo".to_owned())))
        .unwrap();
    let id = app.state().bookmarks.ids().next().cloned().unwrap();
    let anchor = format!("test.bookmarks/bookmarks/{id}");
    app.call("open", serde_json::json!({ "anchor": anchor }))
        .unwrap();
    assert_eq!(OPENED.with(|opened| opened.borrow().clone()), [id]);
    let gone = app.call(
        "open",
        serde_json::json!({ "anchor": "test.bookmarks/bookmarks/nothing" }),
    );
    assert!(
        matches!(&gone, Err(error) if error.to_string().contains("that bookmark is gone")),
        "{gone:?}"
    );
}

/// Rows that are not items of the resource (a process, a vault entry) are
/// opened by anchor, and a gone one is refused.
struct AnchorApp;

impl ResourceApp for AnchorApp {
    type Resource = Bookmarks;
    const ID: &'static str = "so.tilo.test-anchors";
    const TITLE: &'static str = "Bookmarks";

    fn view(cx: &mut ViewCx<Bookmarks>) -> Element {
        cx.on_open(|anchor| {
            if anchor == "row:here" {
                Ok(())
            } else {
                Err(Refusal::new("that row is gone"))
            }
        });
        Box::new(Column::new())
    }
}

#[test]
fn opening_an_anchor_may_be_refused() {
    let mut app = TestApp::<AnchorApp>::new().unwrap();
    app.call("open", serde_json::json!({ "anchor": "row:here" }))
        .unwrap();
    let gone = app.call("open", serde_json::json!({ "anchor": "row:gone" }));
    assert!(
        matches!(&gone, Err(error) if error.to_string().contains("that row is gone")),
        "{gone:?}"
    );
}
