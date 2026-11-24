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

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, trap};
use crate::engine::Wasm;
use crate::value::Value;

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
    ) -> Result<(String, Option<(String, Vec<u8>, [f64; 4])>), String>;

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
    ) -> Result<(String, Option<(String, Vec<u8>, [f64; 4])>), String> {
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

impl Core {
    /// `canvas.*`: the device's drawing engine answers from the text and bytes the call names.
    pub(crate) fn canvas_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::CanvasLook
            | Imp::CanvasExport
            | Imp::CanvasGrade
            | Imp::CanvasCompare
            | Imp::CanvasDiffPicture
            | Imp::CanvasSave => self.canvas_picture(imp, a),
            Imp::CanvasWand
            | Imp::CanvasFind
            | Imp::CanvasCombine
            | Imp::CanvasMeasure
            | Imp::CanvasDiff
            | Imp::CanvasSelect
            | Imp::CanvasInspect
            | Imp::CanvasRemoveAlong
            | Imp::CanvasSvg
            | Imp::CanvasSvgImport => self.canvas_text(imp, a),
            Imp::CanvasOpen => self.canvas_open(a),
            _ => trap("not a canvas import"),
        }
    }

    /// The calls that answer with a picture this device keeps as a file: the artwork drawn,
    /// exported, graded or compared, and the page saved.
    fn canvas_picture(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::CanvasLook | Imp::CanvasExport => {
                let artwork = self.text(a.h(0)?)?;
                let request = self.text(a.h(1)?)?;
                let pictures = self.file_pairs(a.h(2)?)?;
                let answer = if imp == Imp::CanvasLook {
                    self.canvas.borrow().look(&artwork, &request, &pictures)
                } else {
                    self.canvas.borrow().export(&artwork, &request, &pictures)
                };
                self.canvas_made(answer)
            }
            Imp::CanvasGrade => {
                let picture = self.canvas_bytes(a.h(0)?)?;
                let adjust = self.text(a.h(1)?)?;
                let answer = self.canvas.borrow().grade(&picture, &adjust);
                match answer {
                    Ok(bytes) => self.canvas_file(&bytes, "graded.png", "image/png"),
                    Err(why) => self.result(Err(why)),
                }
            }
            Imp::CanvasCompare => {
                let artwork = self.text(a.h(0)?)?;
                let request = self.text(a.h(1)?)?;
                let pictures = self.file_pairs(a.h(2)?)?;
                let answer = self.canvas.borrow().compare(&artwork, &request, &pictures);
                self.canvas_made(answer)
            }
            Imp::CanvasDiffPicture => {
                let before = self.text(a.h(0)?)?;
                let after = self.text(a.h(1)?)?;
                let request = self.text(a.h(2)?)?;
                let pictures = self.file_pairs(a.h(3)?)?;
                let answer = self
                    .canvas
                    .borrow()
                    .diff(&before, &after, &request, &pictures);
                match answer {
                    Ok((Some(bytes), info)) => self.canvas_made(Ok((bytes, info))),
                    Ok((None, _)) => {
                        self.result(Err("the two versions draw the same region".to_owned()))
                    }
                    Err(why) => self.result(Err(why)),
                }
            }
            Imp::CanvasSave => {
                let artwork = self.text(a.h(0)?)?;
                let pictures = self.file_pairs(a.h(1)?)?;
                let answer = self.canvas.borrow().save(&artwork, &pictures);
                match answer {
                    Ok(bytes) => self.canvas_file(
                        &bytes,
                        "Artwork.artwork",
                        "application/vnd.tilo.artwork+zip",
                    ),
                    Err(why) => self.result(Err(why)),
                }
            }
            _ => trap("not a canvas import"),
        }
    }

    /// The calls that answer with text: the color a place picks, the region of a find or a
    /// measure, and the document written as SVG.
    fn canvas_text(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::CanvasWand => {
                let picture = self.picture_bytes(a.h(0)?)?;
                let request = self.text(a.h(1)?)?;
                let answer = self.canvas.borrow().wand(&picture, &request);
                self.result(answer.map(Value::text))
            }
            Imp::CanvasFind => {
                let artwork = self.text(a.h(0)?)?;
                let request = self.text(a.h(1)?)?;
                let pictures = self.file_pairs(a.h(2)?)?;
                let answer = self.canvas.borrow().find(&artwork, &request, &pictures);
                match answer {
                    Ok((d, matte)) => {
                        let mut held = serde_json::json!({ "d": d });
                        if let Some((_, bytes, rect)) = matte {
                            let file = self
                                .keep_bytes("matte.png", "image/png", &bytes)
                                .or_else(trap)?;
                            held["matte_src"] = serde_json::Value::String(file.url());
                            held["matte_rect"] = serde_json::json!(rect);
                        }
                        self.result(Ok(Value::text(held.to_string())))
                    }
                    Err(why) => self.result(Err(why)),
                }
            }
            Imp::CanvasCombine => {
                let artwork = self.text(a.h(0)?)?;
                let request = self.text(a.h(1)?)?;
                let answer = self.canvas.borrow().combine(&artwork, &request);
                self.result(answer.map(Value::text))
            }
            Imp::CanvasMeasure => {
                let artwork = self.text(a.h(0)?)?;
                let request = self.text(a.h(1)?)?;
                let pictures = self.file_pairs(a.h(2)?)?;
                let answer = self.canvas.borrow().measure(&artwork, &request, &pictures);
                self.result(answer.map(Value::text))
            }
            Imp::CanvasDiff => {
                let before = self.text(a.h(0)?)?;
                let after = self.text(a.h(1)?)?;
                let request = self.text(a.h(2)?)?;
                let pictures = self.file_pairs(a.h(3)?)?;
                let answer = self
                    .canvas
                    .borrow()
                    .diff(&before, &after, &request, &pictures);
                self.result(answer.map(|(_, info)| Value::text(info)))
            }
            Imp::CanvasSelect => {
                let artwork = self.text(a.h(0)?)?;
                let request = self.text(a.h(1)?)?;
                let answer = self.canvas.borrow().select(&artwork, &request);
                self.result(answer.map(Value::text))
            }
            Imp::CanvasInspect => {
                let artwork = self.text(a.h(0)?)?;
                let request = self.text(a.h(1)?)?;
                let answer = self.canvas.borrow().inspect(&artwork, &request);
                self.result(answer.map(Value::text))
            }
            Imp::CanvasRemoveAlong => {
                let artwork = self.text(a.h(0)?)?;
                let request = self.text(a.h(1)?)?;
                let answer = self.canvas.borrow().remove_along(&artwork, &request);
                self.result(answer.map(Value::text))
            }
            Imp::CanvasSvg => {
                let artwork = self.text(a.h(0)?)?;
                let request = self.text(a.h(1)?)?;
                let answer = self.canvas.borrow().svg(&artwork, &request);
                self.result(answer.map(Value::text))
            }
            Imp::CanvasSvgImport => {
                let markup = self.text(a.h(0)?)?;
                let request = self.text(a.h(1)?)?;
                let answer = self.canvas.borrow().svg_import(&markup, &request);
                self.result(answer.map(Value::text))
            }
            _ => trap("not a canvas import"),
        }
    }

    /// `canvas.open(archive)`: the artwork the archive holds, with its pictures kept by their
    /// address, so the marks that name them find them on this device as on the one that saved
    /// it.
    fn canvas_open(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let archive = self.canvas_bytes(a.h(0)?)?;
        let answer = self.canvas.borrow().open(&archive);
        let (artwork, pictures) = match answer {
            Ok(opened) => opened,
            Err(why) => return self.result(Err(why)),
        };
        for (_, bytes) in &pictures {
            let _ = self.keep_bytes("picture.png", "image/png", bytes);
        }
        self.result(Ok(Value::text(artwork)))
    }

    /// The file the picture an engine made reads as: its bytes under the name and type its
    /// `info` gives, or `png` when it names no type.
    fn canvas_made(&self, answer: Result<(Vec<u8>, String), String>) -> HResult<Option<Wasm>> {
        match answer {
            Ok((bytes, info)) => {
                let (name, mime) = named(&info, "png");
                self.canvas_file(&bytes, &name, mime)
            }
            Err(why) => self.result(Err(why)),
        }
    }

    /// `bytes` kept as a file called `name` of type `mime`, as the run's value.
    fn canvas_file(&self, bytes: &[u8], name: &str, mime: &str) -> HResult<Option<Wasm>> {
        let file = self.keep_bytes(name, mime, bytes).or_else(trap)?;
        self.result(Ok(Value::file(file)))
    }

    /// The pictures behind `h`, as `(address, bytes)`: each address the artwork's marks name,
    /// read from the content store this device keeps files in.
    fn file_pairs(&self, h: i32) -> HResult<Vec<(String, Vec<u8>)>> {
        let listed = self.val_whole(h)?;
        let Value::List(items) = listed else {
            return trap("a list of picture addresses was expected");
        };
        let blobs = self.blobs.borrow();
        let host = blobs
            .as_ref()
            .ok_or_else(|| "this device keeps no pictures".to_owned())
            .or_else(trap)?;
        items
            .iter()
            .map(|item| -> HResult<(String, Vec<u8>)> {
                match item {
                    Value::Text(address) => {
                        let bare = hash_of(address)
                            .ok_or_else(|| format!("{address} names no picture"))
                            .or_else(trap)?;
                        let bytes = host
                            .read(bare)
                            .ok_or_else(|| format!("the picture {address} is not on this device"))
                            .or_else(trap)?;
                        Ok((address.clone(), bytes))
                    }
                    other => trap(format!(
                        "expected a picture address, got {}",
                        other.kind_name()
                    )),
                }
            })
            .collect()
    }

    /// The bytes of the picture behind `h`: a file the person chose, or the address a picture of
    /// the artwork is kept at.
    fn picture_bytes(&self, h: i32) -> HResult<Vec<u8>> {
        match self.val_whole(h)? {
            Value::File(file) => self.file_bytes(&file).or_else(trap),
            Value::Text(address) => {
                let bare = hash_of(&address)
                    .ok_or_else(|| format!("{address} names no picture"))
                    .or_else(trap)?;
                let blobs = self.blobs.borrow();
                let host = blobs
                    .as_ref()
                    .ok_or_else(|| "this device keeps no pictures".to_owned())
                    .or_else(trap)?;
                host.read(bare)
                    .ok_or_else(|| format!("the picture {address} is not on this device"))
                    .or_else(trap)
            }
            other => trap(format!(
                "a picture is a file or the address of one, got {}",
                other.kind_name()
            )),
        }
    }

    /// The bytes of the file behind `h`, when this device holds them.
    fn canvas_bytes(&self, h: i32) -> HResult<Vec<u8>> {
        let file = self.file_arg(h)?;
        self.file_bytes(&file).or_else(trap)
    }
}

/// The content reference an address names, in either form a picture is addressed by.
fn hash_of(address: &str) -> Option<&str> {
    let bare = address
        .strip_prefix("tilo-blob://")
        .or_else(|| address.strip_prefix("tilo.content:"))
        .unwrap_or(address);
    (!bare.is_empty() && !bare.contains('/')).then_some(bare)
}

/// The name and type of the picture an answer describes, from its info JSON: `format` is `png`,
/// `jpeg` or `webp`.
fn named(info: &str, fallback: &'static str) -> (String, &'static str) {
    let format = serde_json::from_str::<serde_json::Value>(info)
        .ok()
        .and_then(|value| value.get("format")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| fallback.to_owned());
    match format.as_str() {
        "jpeg" | "jpg" => ("picture.jpg".to_owned(), "image/jpeg"),
        "webp" => ("picture.webp".to_owned(), "image/webp"),
        _ => ("picture.png".to_owned(), "image/png"),
    }
}
