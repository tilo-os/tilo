//! Canvas artworks: what a package asks of the device's drawing engine, and nothing about how a
//! window shows one.
//!
//! A package keeps its artwork as records and asks the engine for what only the engine can do:
//! draw it (`canvas.look`, `canvas.export`), grade a picture (`canvas.grade`), find an area
//! (`canvas.wand`), read and write SVG (`canvas.svg`, `canvas.svg_import`) and carry the artwork
//! over its own `.artwork` file (`canvas.save`, `canvas.open`). The artwork travels as its JSON,
//! with the pictures it shows as `(address, bytes)` pairs, so nothing here opens a path or reads
//! a content store of its own.
//!
//! Every call is the device's work, never the sandboxed code's: the runtime reads the bytes it
//! was given and hands them to the host's [`CanvasHost`]. Until a host sets one, every call
//! answers that this device cannot draw.

/// The soft edge a refine keeps: the matte picture's file name, its bytes, and the frame it
/// covers.
pub type Matte = (String, Vec<u8>, [f64; 4]);

/// The device's drawing engine, as the runtime reaches it.
pub trait CanvasHost {
    /// The artwork as `request` asks it seen (a look), as PNG or JPEG bytes and the info JSON.
    ///
    /// # Errors
    ///
    /// Why it cannot be drawn.
    fn look(
        &self,
        artwork: &str,
        request: &str,
        pictures: &[(String, Vec<u8>)],
    ) -> Result<(Vec<u8>, String), String>;

    /// The artwork at full size as `request` asks (an export), as PNG, JPEG or WebP bytes and the
    /// info JSON.
    ///
    /// # Errors
    ///
    /// Why it cannot be drawn.
    fn export(
        &self,
        artwork: &str,
        request: &str,
        pictures: &[(String, Vec<u8>)],
    ) -> Result<(Vec<u8>, String), String>;

    /// One picture with a grade applied, as encoded bytes of the same format.
    ///
    /// # Errors
    ///
    /// Why it cannot be decoded, graded or encoded.
    fn grade(&self, picture: &[u8], adjust: &str) -> Result<Vec<u8>, String>;

    /// The area of like colour in `picture` as `request` names it, as SVG path data.
    ///
    /// # Errors
    ///
    /// Why the picture cannot be read or nothing is found.
    fn wand(&self, picture: &[u8], request: &str) -> Result<String, String>;

    /// The area `request` names in the artwork as it shows, as SVG path data: the magic wand at
    /// a point or a colour, a picture's subject, or the selection's edge refined, and the soft
    /// edge a refine keeps (its picture, bytes and frame) when there is one.
    ///
    /// # Errors
    ///
    /// Why the artwork cannot be drawn or nothing is found.
    fn find(
        &self,
        artwork: &str,
        request: &str,
        pictures: &[(String, Vec<u8>)],
    ) -> Result<(String, Option<Matte>), String>;

    /// The marks `request` names combined into one path, as the path's JSON: its outline and
    /// fill.
    ///
    /// # Errors
    ///
    /// Why the shapes cannot be combined.
    fn combine(&self, artwork: &str, request: &str) -> Result<String, String>;

    /// How the marks `request` names put down ink, as the engine's JSON.
    ///
    /// # Errors
    ///
    /// Why the artwork cannot be drawn or there is no such mark.
    fn measure(
        &self,
        artwork: &str,
        request: &str,
        pictures: &[(String, Vec<u8>)],
    ) -> Result<String, String>;

    /// The artwork beside, over or against its reference picture, as `request` asks: PNG bytes
    /// and the info JSON.
    ///
    /// # Errors
    ///
    /// Why it cannot be drawn or there is no reference picture.
    fn compare(
        &self,
        artwork: &str,
        request: &str,
        pictures: &[(String, Vec<u8>)],
    ) -> Result<(Vec<u8>, String), String>;

    /// What changed between two versions of the artwork, as the engine's JSON, and with
    /// `look`, a picture of the changed region before (left) and after (right).
    ///
    /// # Errors
    ///
    /// Why it cannot be compared or drawn.
    fn diff(
        &self,
        before: &str,
        after: &str,
        request: &str,
        pictures: &[(String, Vec<u8>)],
    ) -> Result<(Option<Vec<u8>>, String), String>;

    /// The selection `request` makes in the artwork (a shape, a lasso, an outline, the marks it
    /// names, grown, shrunk, inverted or feathered, added to or taken from the one it holds), as
    /// the selection's JSON.
    ///
    /// # Errors
    ///
    /// Why the selection cannot be made.
    fn select(&self, artwork: &str, request: &str) -> Result<String, String>;

    /// The strokes a path crosses taken out of the artwork, as the drawing's JSON.
    ///
    /// # Errors
    ///
    /// Why the path cannot be read.
    fn remove_along(&self, artwork: &str, request: &str) -> Result<String, String>;

    /// What the artwork holds (its layers, marks and palette) as the engine's JSON.
    ///
    /// # Errors
    ///
    /// Why it cannot be read.
    fn inspect(&self, artwork: &str, request: &str) -> Result<String, String>;

    /// The artwork, or the marks `request` names, as an SVG document.
    ///
    /// # Errors
    ///
    /// Why it cannot be written.
    fn svg(&self, artwork: &str, request: &str) -> Result<String, String>;

    /// An SVG document taken in as the marks `request` places them, as the drawing's JSON.
    ///
    /// # Errors
    ///
    /// Why the SVG cannot be read or does not fit.
    fn svg_import(&self, markup: &str, request: &str) -> Result<String, String>;

    /// The artwork saved as a `.artwork` package carrying `pictures`.
    ///
    /// # Errors
    ///
    /// Why it cannot be packed.
    fn save(&self, artwork: &str, pictures: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String>;

    /// A `.artwork` package opened: the artwork's JSON and the pictures it carried.
    ///
    /// # Errors
    ///
    /// Why the file is no artwork.
    fn open(&self, archive: &[u8]) -> Result<OpenedArtwork, String>;
}

/// An opened `.artwork` package: the artwork's JSON, and each picture it carried by its name.
pub type OpenedArtwork = (String, Vec<(String, Vec<u8>)>);

/// A host that draws nothing: every call says so, in words a person reads.
pub struct NoCanvas;

impl CanvasHost for NoCanvas {
    fn look(
        &self,
        _artwork: &str,
        _request: &str,
        _pictures: &[(String, Vec<u8>)],
    ) -> Result<(Vec<u8>, String), String> {
        Err(unavailable())
    }

    fn export(
        &self,
        _artwork: &str,
        _request: &str,
        _pictures: &[(String, Vec<u8>)],
    ) -> Result<(Vec<u8>, String), String> {
        Err(unavailable())
    }

    fn grade(&self, _picture: &[u8], _adjust: &str) -> Result<Vec<u8>, String> {
        Err(unavailable())
    }

    fn wand(&self, _picture: &[u8], _request: &str) -> Result<String, String> {
        Err(unavailable())
    }

    fn find(
        &self,
        _artwork: &str,
        _request: &str,
        _pictures: &[(String, Vec<u8>)],
    ) -> Result<(String, Option<Matte>), String> {
        Err(unavailable())
    }

    fn combine(&self, _artwork: &str, _request: &str) -> Result<String, String> {
        Err(unavailable())
    }

    fn measure(
        &self,
        _artwork: &str,
        _request: &str,
        _pictures: &[(String, Vec<u8>)],
    ) -> Result<String, String> {
        Err(unavailable())
    }

    fn compare(
        &self,
        _artwork: &str,
        _request: &str,
        _pictures: &[(String, Vec<u8>)],
    ) -> Result<(Vec<u8>, String), String> {
        Err(unavailable())
    }

    fn diff(
        &self,
        _before: &str,
        _after: &str,
        _request: &str,
        _pictures: &[(String, Vec<u8>)],
    ) -> Result<(Option<Vec<u8>>, String), String> {
        Err(unavailable())
    }

    fn select(&self, _artwork: &str, _request: &str) -> Result<String, String> {
        Err(unavailable())
    }

    fn remove_along(&self, _artwork: &str, _request: &str) -> Result<String, String> {
        Err(unavailable())
    }

    fn inspect(&self, _artwork: &str, _request: &str) -> Result<String, String> {
        Err(unavailable())
    }

    fn svg(&self, _artwork: &str, _request: &str) -> Result<String, String> {
        Err(unavailable())
    }

    fn svg_import(&self, _markup: &str, _request: &str) -> Result<String, String> {
        Err(unavailable())
    }

    fn save(&self, _artwork: &str, _pictures: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
        Err(unavailable())
    }

    fn open(&self, _archive: &[u8]) -> Result<OpenedArtwork, String> {
        Err(unavailable())
    }
}

fn unavailable() -> String {
    "this device cannot draw on a Canvas artwork".to_owned()
}
