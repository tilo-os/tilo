//! The device's drawing engine, as a package's `canvas.*` reaches it: `tilo-canvas-engine` over
//! the artwork's records, answering with a picture's bytes or the engine's JSON.
//!
//! Every call is made on the instance's thread, from the text and the picture bytes the runtime
//! read out of the person's content store, so nothing here opens a file or holds a path.

use std::rc::Rc;

use tilolang_tilo::runtime::canvas::CanvasHost;

/// The device's drawing engine: one per instance, sharing the process's engine.
#[derive(Clone, Copy, Debug, Default)]
pub struct Engine;

impl Engine {
    /// The engine an instance sets, when this device can draw at all.
    #[must_use]
    pub fn host() -> Rc<dyn CanvasHost> {
        Rc::new(Self)
    }
}

fn artwork(json: &str) -> Result<canvas_engine::model::Artwork, String> {
    canvas_engine::host::artwork(json)
}

impl CanvasHost for Engine {
    fn look(
        &self,
        artwork_json: &str,
        request: &str,
        pictures: &[(String, Vec<u8>)],
    ) -> Result<(Vec<u8>, String), String> {
        let art = artwork(artwork_json)?;
        canvas_engine::host::look(&art, request, pictures)
    }

    fn export(
        &self,
        artwork_json: &str,
        request: &str,
        pictures: &[(String, Vec<u8>)],
    ) -> Result<(Vec<u8>, String), String> {
        let art = artwork(artwork_json)?;
        canvas_engine::host::export(&art, request, pictures)
    }

    fn grade(&self, _picture: &[u8], _adjust: &str) -> Result<Vec<u8>, String> {
        Err(
            "grading a picture on its own is not on this device's drawing engine yet; the \
             artwork is graded as it is drawn"
                .to_owned(),
        )
    }

    fn wand(&self, _picture: &[u8], _request: &str) -> Result<String, String> {
        Err(
            "finding an area in a picture on its own is not on this device's drawing engine \
             yet; select the area in the artwork instead"
                .to_owned(),
        )
    }

    fn svg(&self, artwork_json: &str, request: &str) -> Result<String, String> {
        let art = artwork(artwork_json)?;
        canvas_engine::host::svg(&art, request)
    }

    fn svg_import(&self, markup: &str, request: &str) -> Result<String, String> {
        canvas_engine::host::import_svg(markup, request)
    }

    fn save(&self, artwork_json: &str, pictures: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
        let art = artwork(artwork_json)?;
        canvas_engine::host::save(&art, pictures)
    }

    fn open(&self, archive: &[u8]) -> Result<(String, Vec<(String, Vec<u8>)>), String> {
        canvas_engine::host::open(archive)
    }
}
