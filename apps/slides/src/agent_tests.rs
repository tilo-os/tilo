//! What an agent does with a deck through the published door: slides added
//! over the untouched starter, slides named by their number, pictures from
//! content the room holds, and decks carried in and out as `.pptx` content.

use serde_json::{Value, json};
use ui::framework::testing::TestApp;

use crate::resource::SlidesShell;

fn app() -> TestApp<SlidesShell> {
    TestApp::<SlidesShell>::new().unwrap()
}

fn call(app: &mut TestApp<SlidesShell>, id: &str, arguments: Value) -> Value {
    app.call(id, arguments)
        .unwrap_or_else(|error| panic!("{id}: {error}"))
        .unwrap_or(Value::Null)
}

fn titles(app: &TestApp<SlidesShell>) -> Vec<String> {
    crate::deck::mirror(&app.state())
        .slides
        .iter()
        .map(|slide| {
            slide
                .elements
                .iter()
                .find(|element| element.kind == crate::model::ElementKind::Text)
                .map(|element| element.text.clone())
                .unwrap_or_default()
        })
        .collect()
}

#[test]
fn add_slide_takes_the_place_of_the_untouched_starter_only() {
    let mut app = app();
    call(&mut app, "add-slide", json!({"title": "Kickoff"}));
    assert_eq!(titles(&app), ["Kickoff"], "the starter gave way");
    call(&mut app, "add-slide", json!({"title": "Plan"}));
    assert_eq!(titles(&app), ["Kickoff", "Plan"]);
}

#[test]
fn add_slide_keeps_a_starter_someone_wrote_on() {
    let mut app = app();
    call(&mut app, "set-text", json!({"slide": "1", "title": "Mine"}));
    call(&mut app, "add-slide", json!({"title": "Next"}));
    assert_eq!(titles(&app), ["Mine", "Next"]);
}

#[test]
fn verbs_taking_a_slide_take_its_number_and_refuse_one_past_the_end() {
    let mut app = app();
    call(&mut app, "add-slide", json!({"title": "One"}));
    call(&mut app, "add-slide", json!({"title": "Two"}));
    call(
        &mut app,
        "set-text",
        json!({"slide": "2", "title": "Second"}),
    );
    assert_eq!(titles(&app), ["One", "Second"]);
    let first = app.state().slides.ids().next().unwrap().clone();
    call(
        &mut app,
        "move-slide",
        json!({"slide": format!("{{#{}}}", first.as_str()), "position": 1}),
    );
    assert_eq!(titles(&app), ["Second", "One"]);
    let refused = app
        .call_confirmed("delete-slide", json!({"slide": "9"}))
        .unwrap_err()
        .to_string();
    assert!(refused.contains("no slide 9"), "{refused}");
}

#[test]
fn a_picture_the_room_holds_goes_onto_a_slide_by_its_content_address() {
    let mut app = app();
    let hash = app.content().put(&crate::sample::photo_png()).unwrap();
    call(
        &mut app,
        "add-element",
        json!({"slide": "1", "element": {"kind": "image", "x": 100, "y": 100, "w": 320, "h": 200, "src": format!("tilo.content:{hash}")}}),
    );
    let deck = crate::deck::mirror(&app.state());
    let picture = deck.slides[0]
        .elements
        .iter()
        .find(|element| element.kind == crate::model::ElementKind::Image)
        .expect("the picture is on the slide");
    assert_eq!(
        picture.src,
        format!("{}{hash}", crate::assets::ASSET_PREFIX)
    );
}

#[test]
fn a_deck_exported_as_pptx_content_imports_into_a_new_deck() {
    let mut source = app();
    call(
        &mut source,
        "add-slide",
        json!({"title": "Roadmap", "body": "Ship it"}),
    );
    call(&mut source, "add-slide", json!({"title": "Budget"}));
    let output = source
        .call_on_output("slides.deck", "export-pptx-content", json!({}))
        .unwrap()
        .unwrap();
    let address = output.resources.first().cloned().unwrap();
    let hash = address.strip_prefix("tilo.content:").unwrap();
    let bytes = source.content().read(hash).unwrap();

    let mut target = app();
    target.content().put(&bytes).unwrap();
    let outline = call(
        &mut target,
        "import-pptx-content",
        json!({"content": address}),
    );
    assert!(outline.as_str().unwrap().contains("Roadmap"), "{outline}");
    target.settle();
    assert_eq!(titles(&target), ["Roadmap", "Budget"]);

    let refused = target
        .call("import-pptx-content", json!({"content": address}))
        .unwrap_err()
        .to_string();
    assert!(refused.contains("someone wrote"), "{refused}");
}
