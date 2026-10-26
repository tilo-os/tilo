//! A picture reaches the notebook by reference: an agent puts its bytes into
//! the room's content store first and names them `tilo.content:<hash>`, so
//! a picture far larger than an action's inline arguments still goes in.
//! A reference to content the room does not hold is refused, and nothing is
//! kept.

use notes::NotesShell;
use serde_json::json;
use ui::framework::testing::TestApp;

/// A noisy picture, so it does not compress below the size it is meant to
/// have.
fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        let v = x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503);
        let [r, g, b, _] = v.to_le_bytes();
        image::Rgb([r, g, b])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn app() -> TestApp<NotesShell> {
    ui::i18n::I18nManager::init("en-US");
    TestApp::<NotesShell>::new().unwrap()
}

fn kept(app: &TestApp<NotesShell>) -> Vec<String> {
    app.read_document(|doc| {
        notes::read::notebook(doc)
            .expect("the notebook reads")
            .images
            .iter()
            .map(|(digest, _)| digest.clone())
            .collect()
    })
}

#[test]
fn a_picture_larger_than_inline_arguments_is_kept_by_reference() {
    let mut app = app();
    let picture = png(240, 180);
    // Well over the 32 KiB an action's arguments may take inline.
    assert!(picture.len() > 64 * 1024, "{} bytes", picture.len());
    let digest = app.content().put(&picture).unwrap();
    let reference = format!("tilo.content:{digest}");

    app.call("keep-image", json!({ "bytes": reference }))
        .unwrap_or_else(|error| panic!("keep-image refused: {error:?}"));
    assert_eq!(kept(&app), std::slice::from_ref(&digest));

    app.call(
        "import-markdown",
        json!({
            "title": "Trip",
            "markdown": format!("![the view](asset:{digest})\n"),
            "images": [reference],
        }),
    )
    .unwrap_or_else(|error| panic!("import-markdown refused: {error:?}"));
    assert_eq!(kept(&app), [digest]);
}

#[test]
fn a_reference_to_content_the_room_does_not_hold_is_refused() {
    let mut app = app();
    let picture = png(240, 180);
    // Put somewhere this room's store does not reach.
    let elsewhere = tilo_content::ContentStore::memory();
    let digest = elsewhere.put(&picture).unwrap();
    let reference = format!("tilo.content:{digest}");

    let refused = app
        .call("keep-image", json!({ "bytes": reference }))
        .expect_err("a reference the room cannot read is refused");
    let said = format!("{refused:?}");
    assert!(
        said.contains("is not readable in this room"),
        "the refusal says why: {said}"
    );
    assert!(kept(&app).is_empty(), "nothing is kept");
    assert!(!app.content().has(&digest));
}
