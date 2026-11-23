//! The engine as a device answers a package's `canvas.*`: artwork JSON in, a picture's bytes or
//! JSON out. No window, no resource, no history: callers that keep the artwork as records call
//! these to see it and to carry it over the two files it saves.
//!
//! Pictures the artwork names travel with the JSON as `(address, bytes)` pairs. Rendering lends
//! them to the engine's content store for the call, so a look or an export shows them exactly as
//! the window would.

use std::collections::BTreeMap;

use tilo_content::ContentStore;
use tilo_ui::text_render::TextRenderer;

use crate::document::{ArtworkFile, pack, unpack};
use crate::model::Artwork;
use crate::render::Renderer;
use crate::resource::{Export, ExportSvg, Look};
use crate::svg::ImportSvg;
use crate::{svg, view_ops};

/// The artwork `json` holds, as the window keeps it (its live shape, not the file's).
///
/// # Errors
///
/// Why the text is not an artwork.
pub fn artwork(json: &str) -> Result<Artwork, String> {
    // A package names a picture by the address its language gives a file
    // (`tilo-blob://`); the engine names the same bytes by its resource address.
    let json = json.replace("tilo-blob://", "tilo.content:");
    serde_json::from_str(&json).map_err(|error| format!("this is not a Canvas artwork: {error}"))
}

/// The content reference `address` names, whichever of the two address forms it is written in.
fn reference(address: &str) -> Option<&str> {
    if let Some(bare) = address.strip_prefix("tilo-blob://") {
        return tilo_content::valid_reference(bare).then_some(bare);
    }
    tilo_resource::content::reference_hash(address)
        .or_else(|| tilo_content::valid_reference(address).then_some(address))
}

/// The artwork `file` holds, as a fresh document.
///
/// # Errors
///
/// Why the file cannot be a document (a mark on a layer that is gone, too many marks).
pub fn artwork_of(file: &ArtworkFile) -> Result<Artwork, String> {
    let mut art = Artwork::default();
    crate::actions::replace(&mut art, file).map_err(|refusal| refusal.to_string())?;
    Ok(art)
}

/// Run `build` with `pictures` lent to the engine's content stores.
fn lending<T>(pictures: &[(String, Vec<u8>)], build: impl FnOnce() -> T) -> T {
    let scope = tilo_wake::WakeScope::new();
    let store = ContentStore::memory();
    for (address, bytes) in pictures {
        if let Some(reference) = reference(address) {
            let _ = store.put_as(reference, bytes);
        }
    }
    tilo_resource::content::register(&scope.handle(), store.clone());
    let _entered = scope.enter();
    tilo_ui::pictures::lend_content(Some(store));
    let made = build();
    tilo_ui::pictures::lend_content(None);
    made
}

/// A look at `art` as `request` asks, as PNG or JPEG bytes and the info JSON.
///
/// # Errors
///
/// The engine's refusal, in words a person reads.
pub fn look(
    art: &Artwork,
    request: &str,
    pictures: &[(String, Vec<u8>)],
) -> Result<(Vec<u8>, String), String> {
    let request: Look = serde_json::from_str(request)
        .map_err(|error| format!("a look's fields cannot be read: {error}"))?;
    lending(pictures, || {
        let mut renderer = Renderer::default();
        let mut text = TextRenderer::new();
        let (picture, info) = view_ops::look(&mut renderer, &mut text, art, &request)?;
        let info = serde_json::to_string(&info)
            .map_err(|error| format!("the look's answer cannot be written: {error}"))?;
        Ok((picture.bytes, info))
    })
}

/// `art` at full size as `request` asks, as PNG, JPEG or WebP bytes and the info JSON.
///
/// # Errors
///
/// The engine's refusal, in words a person reads.
pub fn export(
    art: &Artwork,
    request: &str,
    pictures: &[(String, Vec<u8>)],
) -> Result<(Vec<u8>, String), String> {
    let request: Export = serde_json::from_str(request)
        .map_err(|error| format!("an export's fields cannot be read: {error}"))?;
    lending(pictures, || {
        let mut renderer = Renderer::default();
        let mut text = TextRenderer::new();
        let (picture, info) = view_ops::export(&mut renderer, &mut text, art, &request)?;
        let info = serde_json::to_string(&info)
            .map_err(|error| format!("the export's answer cannot be written: {error}"))?;
        Ok((picture.bytes, info))
    })
}

/// `art`, or the marks `request` names, as an SVG document.
///
/// # Errors
///
/// The engine's refusal, in words a person reads.
pub fn svg(art: &Artwork, request: &str) -> Result<String, String> {
    let request: ExportSvg = serde_json::from_str(request)
        .map_err(|error| format!("an SVG export's fields cannot be read: {error}"))?;
    let ground = request.ids.as_ref().is_none_or(Vec::is_empty);
    Ok(svg::export(
        art,
        ground,
        |_| None,
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
    ))
}

/// An SVG document taken in as marks, as `request` places them: the drawing's JSON.
///
/// # Errors
///
/// Why the SVG cannot be read or does not fit.
pub fn import_svg(markup: &str, request: &str) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Place {
        layer: Option<String>,
        group: Option<String>,
        fit: Option<[f64; 4]>,
    }
    let place: Place = serde_json::from_str(request)
        .map_err(|error| format!("an SVG import's fields cannot be read: {error}"))?;
    let draw = svg::import(&ImportSvg {
        svg: markup.to_owned(),
        layer: place.layer,
        group: place.group,
        fit: place.fit,
    })?;
    serde_json::to_string(&draw)
        .map_err(|error| format!("the imported marks cannot be written: {error}"))
}

/// `art` saved as a `.artwork` package, with `pictures` as the bytes it carries.
///
/// # Errors
///
/// A picture the artwork shows that `pictures` does not hold, or a pack that fails.
pub fn save(art: &Artwork, pictures: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let store = ContentStore::memory();
    for (address, bytes) in pictures {
        if let Some(reference) = reference(address) {
            let _ = store.put_as(reference, bytes);
        }
    }
    let file = ArtworkFile::of(art);
    let (bytes, missing) = pack(&file, &store)?;
    if !missing.is_empty() {
        return Err(format!(
            "the artwork shows pictures this device does not hold: {}",
            missing.join(", ")
        ));
    }
    Ok(bytes)
}

/// A `.artwork` package opened: the artwork's JSON and the pictures it carried.
///
/// # Errors
///
/// Why the file is no artwork, or cannot be read.
pub fn open(archive: &[u8]) -> Result<(String, Vec<(String, Vec<u8>)>), String> {
    let store = ContentStore::memory();
    let file = unpack(archive, &store)?;
    let art = artwork_of(&file)?;
    let mut pictures = Vec::new();
    for address in file.pictures() {
        if let Some(reference) = reference(address)
            && let Some(bytes) = store.read(reference)
        {
            pictures.push((address.to_owned(), bytes));
        }
    }
    let json = serde_json::to_string(&art)
        .map_err(|error| format!("the artwork cannot be written: {error}"))?;
    Ok((json, pictures))
}
