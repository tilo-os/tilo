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
    fn open(&self, archive: &[u8]) -> Result<(String, Vec<(String, Vec<u8>)>), String>;
}

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

    fn svg(&self, _artwork: &str, _request: &str) -> Result<String, String> {
        Err(unavailable())
    }

    fn svg_import(&self, _markup: &str, _request: &str) -> Result<String, String> {
        Err(unavailable())
    }

    fn save(&self, _artwork: &str, _pictures: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
        Err(unavailable())
    }

    fn open(&self, _archive: &[u8]) -> Result<(String, Vec<(String, Vec<u8>)>), String> {
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
            Imp::CanvasLook | Imp::CanvasExport => {
                let artwork = self.text(a.h(0)?)?;
                let request = self.text(a.h(1)?)?;
                let pictures = self.file_pairs(a.h(2)?)?;
                let host = self.canvas.borrow();
                let answer = if imp == Imp::CanvasLook {
                    host.look(&artwork, &request, &pictures)
                } else {
                    host.export(&artwork, &request, &pictures)
                };
                drop(host);
                match answer {
                    Ok((bytes, info)) => {
                        let (name, mime) = named(&info, "png");
                        let file = self.keep_bytes(&name, mime, &bytes).or_else(trap)?;
                        self.ret_h(Value::file(file))
                    }
                    Err(why) => self.result(Err(why)),
                }
            }
            Imp::CanvasGrade => {
                let picture = self.canvas_bytes(a.h(0)?)?;
                let adjust = self.text(a.h(1)?)?;
                let answer = self.canvas.borrow().grade(&picture, &adjust);
                match answer {
                    Ok(bytes) => {
                        let file = self
                            .keep_bytes("graded.png", "image/png", &bytes)
                            .or_else(trap)?;
                        self.ret_h(Value::file(file))
                    }
                    Err(why) => self.result(Err(why)),
                }
            }
            Imp::CanvasWand => {
                let picture = self.canvas_bytes(a.h(0)?)?;
                let request = self.text(a.h(1)?)?;
                let answer = self.canvas.borrow().wand(&picture, &request);
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
            Imp::CanvasSave => {
                let artwork = self.text(a.h(0)?)?;
                let pictures = self.file_pairs(a.h(1)?)?;
                let answer = self.canvas.borrow().save(&artwork, &pictures);
                match answer {
                    Ok(bytes) => {
                        let file = self
                            .keep_bytes(
                                "Artwork.artwork",
                                "application/vnd.tilo.artwork+zip",
                                &bytes,
                            )
                            .or_else(trap)?;
                        self.ret_h(Value::file(file))
                    }
                    Err(why) => self.result(Err(why)),
                }
            }
            Imp::CanvasOpen => {
                let archive = self.canvas_bytes(a.h(0)?)?;
                let answer = self.canvas.borrow().open(&archive);
                let (artwork, pictures) = match answer {
                    Ok(opened) => opened,
                    Err(why) => return self.result(Err(why)),
                };
                // The pictures the package carried are kept by their address, so the marks that
                // name them find them on this device as on the one that saved it.
                for (_, bytes) in &pictures {
                    let _ = self.keep_bytes("picture.png", "image/png", bytes);
                }
                self.result(Ok(Value::text(artwork)))
            }
            _ => trap("not a canvas import"),
        }
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
            .map(|item| -> HResult<(String, Vec<u8>)> match item {
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
                other => trap(format!("expected a picture address, got {}", other.kind_name())),
            })
            .collect()
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
