//! A saved artwork travels whole: the pictures its marks show are in the
//! file, once each, as the bytes they were placed from, and opening it
//! where they never were shows them.
//!
//! Each app opens on a profile of its own, whose content store never had
//! what another profile kept.
#![allow(
    clippy::unwrap_used,
    reason = "test assertions over exact, hand-written fixtures"
)]

use std::io::Read as _;

use canvas::App;
use canvas::model::Shape;
use canvas::render::Renderer;
use canvas::resource::Look;
use canvas::view_ops;
use serde_json::json;
use tilo_content::ContentStore;
use ui::prelude::TestApp;

const RED: [u8; 4] = [210, 40, 30, 255];
const BLUE: [u8; 4] = [30, 60, 200, 255];

/// A PNG, its left half red and its right half blue.
fn halves() -> Vec<u8> {
    let picture = image::RgbaImage::from_fn(200, 100, |x, _| {
        image::Rgba(if x < 100 { RED } else { BLUE })
    });
    let mut png = Vec::new();
    picture
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    png
}

/// The colour a PNG look shows at the middle of `region`.
fn colour_at(app: &TestApp<App>, region: [f64; 4]) -> [u8; 4] {
    let look = Look {
        region: Some(region),
        size: Some(64),
        format: Some("png".into()),
        ..Look::default()
    };
    let mut renderer = Renderer::default();
    renderer.pictures.read_from(app.content());
    let (picture, _) = view_ops::look(
        &mut renderer,
        &mut ui::TextRenderer::new(),
        &app.state(),
        &look,
    )
    .unwrap();
    let decoded = image::load_from_memory(&picture.bytes).unwrap().to_rgba8();
    let (w, h) = decoded.dimensions();
    decoded.get_pixel(w / 2, h / 2).0
}

fn near(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 4)
}

/// Both copies of the picture show, red then blue.
fn shows_both(app: &TestApp<App>) {
    for x in [0.0, 300.0] {
        assert!(near(colour_at(app, [x + 40.0, 40.0, 20.0, 20.0]), RED));
        assert!(near(colour_at(app, [x + 140.0, 40.0, 20.0, 20.0]), BLUE));
    }
}

#[test]
fn a_saved_artwork_carries_its_pictures_once_and_shows_them_where_they_never_were() {
    let files = tempfile::tempdir().unwrap();
    let path = files.path().join("Twice.artwork");
    let picture_path = files.path().join("halves.png");
    let png = halves();
    std::fs::write(&picture_path, &png).unwrap();
    let hash = tilo_content::hash_bytes(&png);

    let mut painter = TestApp::<App>::new().unwrap();
    let src = picture_path.display().to_string();
    painter
        .call(
            "draw",
            json!({"marks": [
                {"id": "first", "shape": {"kind": "image", "src": src, "x": 0, "y": 0}},
                {"id": "second", "shape": {"kind": "image", "src": src, "x": 300, "y": 0}},
            ]}),
        )
        .unwrap();
    let art = painter.state();
    let address = ui::tilo_actions::content_resource(&hash).unwrap();
    for id in ["first", "second"] {
        let Shape::Image { src, .. } = &art.mark(id).unwrap().shape else {
            panic!("{id} is not a picture");
        };
        assert_eq!(src, &address);
    }
    shows_both(&painter);
    let saved = painter
        .call("save-artwork", json!({"path": path.display().to_string()}))
        .unwrap()
        .unwrap();
    assert!(saved.get("missing_pictures").is_none(), "{saved}");

    // The package: its type, the artwork, and the picture once, as placed.
    let bytes = std::fs::read(&path).unwrap();
    let mut package = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
    let names: Vec<&str> = package.file_names().collect();
    assert_eq!(names.len(), 3, "{names:?}");
    let mut kept = Vec::new();
    package
        .by_name(&format!("content/{hash}"))
        .unwrap()
        .read_to_end(&mut kept)
        .unwrap();
    assert_eq!(
        kept, png,
        "the picture is kept as its bytes, not re-encoded"
    );
    let mut mimetype = String::new();
    package
        .by_index(0)
        .unwrap()
        .read_to_string(&mut mimetype)
        .unwrap();
    assert_eq!(mimetype, canvas::document::MEDIA_TYPE);
    drop(painter);

    // Another machine: its content store never had the picture.
    let mut opener = TestApp::<App>::new().unwrap();
    assert!(!opener.content().has(&hash));
    opener
        .call("open-artwork", json!({"file": path.display().to_string()}))
        .unwrap();
    assert!(opener.content().has(&hash));
    assert_eq!(opener.state().marks.len(), 2);
    shows_both(&opener);

    // Opened by a person from Files, in yet another.
    let launched = TestApp::<App>::launched(&[&path.display().to_string()]).unwrap();
    assert!(launched.content().has(&hash));
    shows_both(&launched);
}

#[test]
fn the_artworks_json_alone_still_opens() {
    let files = tempfile::tempdir().unwrap();
    let path = files.path().join("Plain.artwork");
    let _store = tilo_test_env::lock();
    let mut app = TestApp::<App>::new().unwrap();
    for action in canvas::sample::landscape() {
        app.invoke(action).unwrap();
    }
    let json = canvas::document::ArtworkFile::of(&app.state()).to_json();
    std::fs::write(&path, json).unwrap();
    let marks = app.state().marks.len();
    app.call("new-artwork", json!({})).unwrap();
    app.call("open-artwork", json!({"file": path.display().to_string()}))
        .unwrap();
    assert_eq!(app.state().marks.len(), marks);
}

#[test]
fn a_picture_that_is_not_its_name_is_refused() {
    use std::io::Write as _;
    let _store = tilo_test_env::lock();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    zip.start_file("artwork.json", options).unwrap();
    let art = canvas::Artwork::default();
    zip.write_all(&canvas::document::ArtworkFile::of(&art).to_json())
        .unwrap();
    zip.start_file(format!("content/{}", "0".repeat(64)), options)
        .unwrap();
    zip.write_all(&halves()).unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    let error = canvas::document::unpack(&bytes, &ContentStore::memory()).unwrap_err();
    assert!(error.contains("damaged"), "{error}");
}
