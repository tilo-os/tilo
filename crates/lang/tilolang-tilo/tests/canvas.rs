//! Canvas compares an artwork with one of its versions on the device's own store: the marks
//! the artwork held then are read from the store at that version, each as it stood, and handed
//! to the drawing engine beside the artwork as it stands.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};

use tilolang_runtime::canvas::{CanvasHost, OpenedArtwork};
use tilolang_runtime::{ResourceRef, UiNode, Value, ViewInstance};

use crate::fixtures::{Replica, Workspace, call, target};

const CANVAS_APP: &str = "so.tilo.canvas";
const CANVAS: (&str, &str) = (
    include_str!("../../../../apps/tl/canvas/app.tl"),
    include_str!("../../../../apps/tl/canvas/app.ids"),
);

/// An engine that keeps each pair of artworks it was asked to compare.
#[derive(Clone, Default)]
struct Engine(Rc<RefCell<Vec<(String, String)>>>);

fn cannot() -> String {
    "this engine only compares".to_owned()
}

impl CanvasHost for Engine {
    fn look(&self, _: &str, _: &str, _: &[(String, Vec<u8>)]) -> Result<(Vec<u8>, String), String> {
        Err(cannot())
    }
    fn export(
        &self,
        _: &str,
        _: &str,
        _: &[(String, Vec<u8>)],
    ) -> Result<(Vec<u8>, String), String> {
        Err(cannot())
    }
    fn grade(&self, _: &[u8], _: &str) -> Result<Vec<u8>, String> {
        Err(cannot())
    }
    fn wand(&self, _: &[u8], _: &str) -> Result<String, String> {
        Err(cannot())
    }
    fn find(
        &self,
        _: &str,
        _: &str,
        _: &[(String, Vec<u8>)],
    ) -> Result<(String, Option<(String, Vec<u8>, [f64; 4])>), String> {
        Err(cannot())
    }
    fn combine(&self, _: &str, _: &str) -> Result<String, String> {
        Err(cannot())
    }
    fn measure(&self, _: &str, _: &str, _: &[(String, Vec<u8>)]) -> Result<String, String> {
        Err(cannot())
    }
    fn compare(
        &self,
        _: &str,
        _: &str,
        _: &[(String, Vec<u8>)],
    ) -> Result<(Vec<u8>, String), String> {
        Err(cannot())
    }
    fn diff(
        &self,
        before: &str,
        after: &str,
        _: &str,
        _: &[(String, Vec<u8>)],
    ) -> Result<(Option<Vec<u8>>, String), String> {
        self.0
            .borrow_mut()
            .push((before.to_owned(), after.to_owned()));
        Ok((
            Some(vec![7, 7, 7]),
            r#"{"same":false,"marks":[{"id":"mark-1"}],"layers":[],"artwork":[],"media":[],"changed_region":[8.0,8.0,4.0,4.0]}"#
                .to_owned(),
        ))
    }
    fn select(&self, _: &str, _: &str) -> Result<String, String> {
        Err(cannot())
    }
    fn remove_along(&self, _: &str, _: &str) -> Result<String, String> {
        Err(cannot())
    }
    fn inspect(&self, _: &str, _: &str) -> Result<String, String> {
        Err(cannot())
    }
    fn svg(&self, _: &str, _: &str) -> Result<String, String> {
        Err(cannot())
    }
    fn svg_import(&self, _: &str, _: &str) -> Result<String, String> {
        Err(cannot())
    }
    fn save(&self, _: &str, _: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
        Err(cannot())
    }
    fn open(&self, _: &[u8]) -> Result<OpenedArtwork, String> {
        Err(cannot())
    }
}

/// The device's content store: what a comparison's picture is kept in.
#[derive(Clone, Default)]
struct Device(Rc<RefCell<HashMap<String, Vec<u8>>>>);

impl BlobHost for Device {
    fn read(&self, hash: &str) -> Option<Vec<u8>> {
        self.0.borrow().get(hash).cloned()
    }
    fn choose(&self, _: &FileRequest) -> Result<Vec<FileValue>, String> {
        Err("this test chooses nothing".to_owned())
    }
    fn save(&self, _: &SaveRequest, _: &[u8]) -> Result<(), String> {
        Err("this test writes no files out".to_owned())
    }
    fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
        let hash = format!("h{}", self.0.borrow().len());
        self.0.borrow_mut().insert(hash.clone(), bytes.to_vec());
        Ok(FileValue::new(
            name,
            media,
            i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            hash,
        ))
    }
}

/// A new artwork in Ada's library, with the engine above drawing for it.
fn artwork(ada: &Replica, engine: &Engine, title: &str) -> ResourceRef {
    ada.host.runtime().set_canvas(engine.clone());
    ada.host.runtime().set_blobs(Device::default());
    let library = ada.host.root_of(CANVAS_APP).expect("the library");
    let art = target(&call(
        ada,
        "ada",
        CANVAS_APP,
        "new_artwork",
        vec![
            Value::resource(&library),
            Value::text(title),
            Value::Float(64.0),
            Value::Float(48.0),
        ],
    ));
    ada.commit();
    art
}

fn window(ada: &Replica, art: &ResourceRef) -> ViewInstance {
    let view = ada
        .host
        .package(CANVAS_APP)
        .unwrap()
        .view_id("artwork")
        .unwrap();
    ada.host
        .runtime()
        .open_view(
            &tilolang_runtime::Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            &view,
            vec![Value::resource(art)],
        )
        .unwrap()
}

fn find<'a>(node: &'a UiNode, matches: &dyn Fn(&UiNode) -> bool) -> Option<&'a UiNode> {
    if matches(node) {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, matches))
}

/// The window's timeline of versions.
fn timeline(view: &ViewInstance) -> UiNode {
    find(view.tree(), &|node| {
        node.kind == "ui.Slider"
            && matches!(node.props.get("label"), Some(Value::Text(label)) if label.starts_with("Versions of"))
    })
    .cloned()
    .expect("the window shows its versions along a timeline")
}

fn number(node: &UiNode, prop: &str) -> f64 {
    match node.props.get(prop) {
        Some(Value::Float(number)) => *number,
        other => panic!("{prop} is a number, not {other:?}"),
    }
}

fn shape(ada: &Replica, art: &ResourceRef, d: &str, colour: &str) -> ResourceRef {
    let mark = target(&call(
        ada,
        "ada",
        CANVAS_APP,
        "add_shape",
        vec![
            Value::resource(art),
            Value::none(),
            Value::text(d),
            Value::text(colour),
        ],
    ));
    ada.commit();
    mark
}

#[test]
fn a_version_holds_the_marks_the_artwork_held_then() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[CANVAS]);
    ada.commit();
    let engine = Engine::default();
    let art = artwork(&ada, &engine, "Harbour");
    let first = shape(&ada, &art, "M 0 0 L 4 0 L 4 4 Z", "#2f7fb8");
    let add_shape = ada
        .host
        .package(CANVAS_APP)
        .unwrap()
        .meta
        .actions
        .iter()
        .find(|action| action.name == "add_shape")
        .unwrap()
        .id
        .clone();
    let change = ada
        .host
        .runtime()
        .versions(&art)
        .unwrap()
        .into_iter()
        .rev()
        .find(|version| version.action == add_shape)
        .expect("the first shape is a version of the artwork")
        .change;

    // Since then: a second shape, and the first one recoloured.
    shape(&ada, &art, "M 8 8 L 12 8 L 12 12 Z", "#d24b3a");
    call(
        &ada,
        "ada",
        CANVAS_APP,
        "recolour",
        vec![
            Value::resource(&art),
            Value::resource(&first),
            Value::text("#11aa44"),
        ],
    );
    ada.commit();

    let compared = call(
        &ada,
        "ada",
        CANVAS_APP,
        "compare_versions",
        vec![Value::resource(&art), Value::text(&change)],
    );
    assert!(
        matches!(&compared, Value::Opt(Some(_))),
        "the comparison: {compared:?}; notice {:?}",
        ada.host.runtime().read(&art, "notice")
    );
    let asked = engine
        .0
        .borrow()
        .last()
        .cloned()
        .expect("the engine compared");
    let (before, after) = asked;
    assert_eq!(
        before.matches("\"kind\":\"path\"").count(),
        1,
        "the version holds the first shape alone: {before}"
    );
    assert!(
        before.contains("#2f7fb8") && !before.contains("#11aa44"),
        "the first shape has the colour it had then: {before}"
    );
    assert_eq!(
        after.matches("\"kind\":\"path\"").count(),
        2,
        "the artwork as it stands holds both: {after}"
    );
}

#[test]
fn the_timeline_scrubs_through_the_versions_and_comes_back_to_now() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[CANVAS]);
    ada.commit();
    let engine = Engine::default();
    let art = artwork(&ada, &engine, "Harbour");
    shape(&ada, &art, "M 0 0 L 4 0 L 4 4 Z", "#2f7fb8");
    shape(&ada, &art, "M 8 8 L 12 8 L 12 12 Z", "#d24b3a");
    let mut view = window(&ada, &art);
    let shows_timeline = |view: &ViewInstance| {
        find(view.tree(), &|node| {
            node.kind == "ui.Slider"
                && matches!(node.props.get("label"), Some(Value::Text(label)) if label.starts_with("Versions of"))
        })
        .is_some()
    };
    assert!(
        !shows_timeline(&view),
        "opening the artwork reads none of its versions"
    );
    let ask = find(view.tree(), &|node| {
        node.kind == "ui.Button" && node.props.get("label") == Some(&Value::text("Versions"))
    })
    .expect("the window offers its versions")
    .key
    .clone();
    view.fire_with(&ask, "press", None).unwrap();
    view.poll_patches().unwrap();
    let line = timeline(&view);
    let newest = number(&line, "max");
    assert!(
        newest >= 2.0,
        "a version for each shape and the artwork's making: {newest}"
    );
    assert_eq!(number(&line, "value"), newest, "the knob rests on now");
    match line.props.get("stops") {
        Some(Value::List(stops)) => assert_eq!(
            f64::from(u32::try_from(stops.len()).unwrap()),
            newest + 1.0,
            "{stops:?}"
        ),
        other => panic!("each version is a stop: {other:?}"),
    }

    // Scrubbing back to the version before the second shape compares with it.
    view.fire_with(&line.key, "change", Some(&Value::Float(newest - 1.0)))
        .unwrap();
    ada.commit();
    let (before, after) = engine
        .0
        .borrow()
        .last()
        .cloned()
        .expect("the engine compared");
    assert_eq!(before.matches("\"kind\":\"path\"").count(), 1, "{before}");
    assert_eq!(after.matches("\"kind\":\"path\"").count(), 2, "{after}");
    view.poll_patches().unwrap();
    let line = timeline(&view);
    assert_eq!(
        number(&line, "max"),
        newest,
        "laying a comparison over the surface is no version of the work"
    );
    assert_eq!(
        number(&line, "value"),
        newest - 1.0,
        "the knob stays where it was left"
    );
    assert!(
        !matches!(ada.host.runtime().read(&art, "version_note").unwrap(), Value::Text(ref note) if note.is_empty()),
        "the window says what changed"
    );

    // Back at the newest, the comparison is taken away.
    view.fire_with(&line.key, "change", Some(&Value::Float(newest)))
        .unwrap();
    ada.commit();
    assert_eq!(
        ada.host.runtime().read(&art, "version_overlay").unwrap(),
        Value::text(""),
        "now is the artwork as it stands"
    );
}

#[test]
fn the_well_previews_the_brush_size_and_paint_as_they_change() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[CANVAS]);
    ada.commit();
    let art = artwork(&ada, &Engine::default(), "Sketch");
    let mut view = window(&ada, &art);
    let preview = |view: &ViewInstance| {
        find(view.tree(), &|node| node.kind == "ui.StrokePreview")
            .cloned()
            .expect("the well shows what the next stroke lays down")
    };
    let size = |view: &ViewInstance| {
        find(view.tree(), &|node| {
            node.kind == "ui.Slider" && node.props.get("label") == Some(&Value::text("Size"))
        })
        .cloned()
        .expect("the well sizes the brush")
    };
    let shown = preview(&view);
    let before = number(&shown, "size");
    view.edit(&size(&view).key, &Value::Float(before + 20.0))
        .unwrap();
    view.poll_patches().unwrap();
    assert_eq!(
        number(&preview(&view), "size"),
        before + 20.0,
        "the preview follows the size"
    );
    assert!(
        matches!(shown.props.get("colour"), Some(Value::Text(colour)) if colour.starts_with('#')),
        "the preview is in the paint: {:?}",
        shown.props
    );
}
