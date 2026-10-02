//! The app as it runs: the host door, undo, permissions, and the views an
//! agent sees with.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::actions::NewStroke;
use canvas::{App, ArtworkResource};
use serde_json::json;
use ui::prelude::{TestApp, tilo_resource::ActionRef};

fn stroke(colour: &str) -> NewStroke {
    NewStroke {
        id: Some("first".into()),
        name: None,
        points: vec![vec![100.0, 100.0], vec![600.0, 300.0, 0.5]],
        size: 40.0,
        colour: colour.into(),
        opacity: None,
        wet: None,
        params: None,
    }
}

/// A stroke painted through the host door is one undo step, and a viewer
/// may look but not paint.
#[test]
fn painting_is_undoable_and_viewers_cannot_paint() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(ArtworkResource::PAINT.with((None, vec![stroke("#204080")])))
        .expect("paint");
    assert_eq!(app.state().marks.len(), 1);
    assert!(app.undo(), "the passage is one undo step");
    assert!(app.state().marks.is_empty());

    let mut viewer = TestApp::<App>::with_role(ui::Role::Viewer).expect("the app opens");
    assert!(
        viewer
            .invoke(ArtworkResource::PAINT.with((None, vec![stroke("#204080")])))
            .is_err()
    );
    let look: ActionRef<ArtworkResource, ()> = ActionRef::new("look", &[]);
    assert!(
        viewer.operate(look.invoke()).unwrap().is_some(),
        "a viewer may look"
    );
}

/// `look` through the host door answers with a stored PNG and the revision
/// it shows; the revision moves when the artwork changes, so a caller can
/// tell a stale look from a fresh one. `inspect` names the marks by id.
#[test]
fn look_and_inspect_answer_through_the_host_door() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let look: ActionRef<ArtworkResource, u32> = ActionRef::new("look", &["size"]);
    let before = app
        .operate(look.with(256))
        .unwrap()
        .expect("a look finishes at once");
    let value = before.value.clone().unwrap();
    assert_eq!(value["width"], 256, "{value}");
    let address = before
        .resources
        .first()
        .expect("the picture is stored")
        .clone();
    assert_eq!(value["content"], json!(address));
    let hash = ui::tilo_actions::content_hash(&address).unwrap();
    let png = tilo_content::ContentStore::for_app()
        .read(hash)
        .expect("readable");
    assert!(png.starts_with(b"\x89PNG"));

    app.invoke(ArtworkResource::PAINT.with((None, vec![stroke("pigment:ultramarine")])))
        .expect("paint");
    let after = app.operate(look.with(256)).unwrap().unwrap();
    assert_ne!(
        after.value.unwrap()["revision"],
        value["revision"],
        "a new revision after painting"
    );

    let inspect: ActionRef<ArtworkResource, ()> = ActionRef::new("inspect", &[]);
    let listed = app
        .operate(inspect.invoke())
        .unwrap()
        .unwrap()
        .value
        .unwrap();
    assert_eq!(listed["marks_bottom_first"][0]["id"], "first", "{listed}");
    assert_eq!(listed["layers_top_first"][0]["marks"], 1);
}

/// The production root satisfies the Tilo-native contract: every control
/// that can cause an effect is bound to a published action.
#[test]
fn the_native_app_contract_is_certified() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "{audit:#?}");
}
