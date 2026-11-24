//! Canvas compares an artwork with one of its versions on the device's own store: the marks
//! the artwork held then are read from the store at that version, each as it stood, and handed
//! to the drawing engine beside the artwork as it stands.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};

use tilolang_runtime::canvas::{CanvasHost, OpenedArtwork};
use tilolang_runtime::{ResourceRef, Value};

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
