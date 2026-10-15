//! The artwork as a document: the sizes and grounds a new one starts from,
//! the file it is kept as in Files, and the artworks opened or saved lately.
//!
//! A window holds one artwork. Starting a new one or opening a file
//! replaces it as one step of its history, so Undo brings the old one back;
//! a new tab starts a separate artwork beside it.
//!
//! The file (`.artwork`) is a ZIP package, so it travels whole: a
//! `mimetype` entry first, `artwork.json` (the artwork's content: its
//! title, size, ground, layers in order, marks in order and the media saved
//! in it) and, under `content/`, each picture its marks show, once however
//! often it is placed, as the bytes it was placed from and named by its
//! content address. The JSON is the same data every collaborator simulates
//! from, so a painting opened from a file paints exactly as it was saved;
//! opening it puts its pictures back in the content store, so they show on
//! a machine that never had them. The JSON alone still opens as an artwork
//! without pictures of its own.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ui::icons::Icon;

use crate::media::MediumSpec;
use crate::model::{Artwork, Ground, MarkBody};

/// The file extension of a saved artwork.
pub const EXTENSION: &str = "artwork";
/// The media type of a saved artwork.
pub const MEDIA_TYPE: &str = "application/vnd.tilo.artwork+zip";
/// What the file says it is.
const FORMAT: &str = "tilo-artwork";
/// The largest artwork file read, in bytes: room for a few large photos.
pub const MAX_FILE_BYTES: usize = 256 << 20;
/// The package's entry holding the artwork's JSON.
const DOCUMENT_ENTRY: &str = "artwork.json";
/// Where the package keeps pictures, each named by its content hash.
const CONTENT_DIR: &str = "content/";

/// A size a new artwork starts at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub id: &'static str,
    /// The Fluent id of its name.
    pub label: &'static str,
    pub width: f64,
    pub height: f64,
    pub icon: Icon,
}

/// The sizes offered, in units (a unit is a pixel at 1x export).
pub const SIZES: [Size; 9] = [
    Size {
        id: "screen",
        label: "canvas-size-screen",
        width: 1920.0,
        height: 1080.0,
        icon: Icon::Monitor,
    },
    Size {
        id: "a4",
        label: "canvas-size-a4",
        width: 2480.0,
        height: 3508.0,
        icon: Icon::FileText,
    },
    Size {
        id: "letter",
        label: "canvas-size-letter",
        width: 2550.0,
        height: 3300.0,
        icon: Icon::FileText,
    },
    Size {
        id: "square",
        label: "canvas-size-square",
        width: 2048.0,
        height: 2048.0,
        icon: Icon::Square,
    },
    Size {
        id: "poster",
        label: "canvas-size-poster",
        width: 3600.0,
        height: 5400.0,
        icon: Icon::Image,
    },
    Size {
        id: "icon",
        label: "canvas-size-icon",
        width: 1024.0,
        height: 1024.0,
        icon: Icon::Star,
    },
    Size {
        id: "pixel-32",
        label: "canvas-size-pixel-32",
        width: 32.0,
        height: 32.0,
        icon: Icon::Square,
    },
    Size {
        id: "pixel-64",
        label: "canvas-size-pixel-64",
        width: 64.0,
        height: 64.0,
        icon: Icon::Square,
    },
    Size {
        id: "pixel-128",
        label: "canvas-size-pixel-128",
        width: 128.0,
        height: 128.0,
        icon: Icon::Square,
    },
];

/// The size named `id`.
pub fn size(id: &str) -> Option<Size> {
    SIZES.into_iter().find(|size| size.id == id)
}

/// What a new artwork is painted on and with: a ground and the medium of
/// its first layer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Start {
    pub id: &'static str,
    /// The Fluent id of its name.
    pub label: &'static str,
    pub texture: &'static str,
    /// The ground's colour, `#rrggbb`: the artwork's, the same in every
    /// theme.
    pub colour: &'static str,
    /// The first layer's medium; empty for plain vector art.
    pub medium: &'static str,
}

/// The grounds and media offered.
pub const STARTS: [Start; 7] = [
    Start {
        id: "paper",
        label: "canvas-start-paper",
        texture: "paper",
        colour: "#f4efe3", // document colour: warm drawing paper
        medium: "",
    },
    Start {
        id: "white",
        label: "canvas-start-white",
        texture: "smooth",
        colour: "#ffffff", // document colour: a plain white sheet
        medium: "",
    },
    Start {
        id: "watercolour",
        label: "canvas-start-watercolour",
        texture: "cold-press",
        colour: "#f7f3ea", // document colour: cold-pressed watercolour paper
        medium: "watercolour",
    },
    Start {
        id: "oil",
        label: "canvas-start-oil",
        texture: "linen",
        colour: "#e8dcc6", // document colour: primed linen
        medium: "oil",
    },
    Start {
        id: "panel",
        label: "canvas-start-panel",
        texture: "panel",
        colour: "#efe6d4", // document colour: a gessoed panel
        medium: "oil",
    },
    Start {
        id: "dark",
        label: "canvas-start-dark",
        texture: "smooth",
        colour: "#1e2024", // document colour: a dark sheet for light work
        medium: "",
    },
    Start {
        id: "transparent",
        label: "canvas-start-transparent",
        texture: "smooth",
        colour: "#ffffff00",
        medium: "",
    },
];

/// The start named `id`.
pub fn start(id: &str) -> Option<Start> {
    STARTS.into_iter().find(|start| start.id == id)
}

/// One layer as the file keeps it; its place is its index.
#[allow(
    clippy::struct_excessive_bools,
    reason = "the file keeps a layer's flags one to one"
)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileLayer {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub medium: String,
    #[serde(default = "one")]
    pub opacity: f64,
    #[serde(default = "yes")]
    pub visible: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub texture: f64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
    /// The layer this one masks; empty for an ordinary layer.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mask_of: String,
    /// A mask's grey before any mark.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub base: f64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub invert: bool,
    /// Clipped to the layer below.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clip: bool,
    /// A mask that stays put when its layer's marks move.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unlinked: bool,
    /// Colour and tone adjustments of everything on the layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adjust: Option<crate::model::Adjust>,
}

/// One mark as the file keeps it; its place within its layer is its
/// index.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileMark {
    pub id: String,
    pub layer: String,
    #[serde(flatten)]
    pub body: MarkBody,
}

fn one() -> f64 {
    1.0
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes the field by reference"
)]
fn is_one(value: &f64) -> bool {
    (*value - 1.0).abs() < f64::EPSILON
}

fn yes() -> bool {
    true
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes the field by reference"
)]
fn is_zero(value: &f64) -> bool {
    *value == 0.0
}

/// A saved artwork.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArtworkFile {
    pub format: String,
    pub version: u32,
    #[serde(default)]
    pub title: String,
    pub width: f64,
    pub height: f64,
    #[serde(default)]
    pub ground: Ground,
    #[serde(default)]
    pub layers: Vec<FileLayer>,
    #[serde(default)]
    pub marks: Vec<FileMark>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub media: BTreeMap<String, MediumSpec>,
}

impl ArtworkFile {
    /// `art` as its file keeps it. The reference picture stays behind: it
    /// is something to work from, not part of the work.
    pub fn of(art: &Artwork) -> Self {
        Self {
            format: FORMAT.into(),
            version: 1,
            title: art.title.clone(),
            width: art.width,
            height: art.height,
            ground: art.ground.clone(),
            layers: art
                .layers
                .iter()
                .map(|layer| FileLayer {
                    id: layer.id.clone(),
                    name: layer.name.clone(),
                    group: layer.group.clone(),
                    medium: layer.medium.clone(),
                    opacity: layer.opacity,
                    visible: layer.visible,
                    texture: layer.texture,
                    locked: layer.locked,
                    mask_of: layer.mask_of.clone(),
                    base: layer.base,
                    invert: layer.invert,
                    clip: layer.clip,
                    unlinked: layer.unlinked,
                    adjust: layer.adjust.clone(),
                })
                .collect(),
            marks: art
                .marks
                .iter()
                .map(|mark| FileMark {
                    id: mark.id.clone(),
                    layer: mark.layer.clone(),
                    body: mark.body(),
                })
                .collect(),
            media: BTreeMap::from(&art.media),
        }
    }

    /// The artwork's JSON, indented, as the package's `artwork.json` keeps
    /// it and an `open-artwork` action carries it.
    pub fn to_json(&self) -> Vec<u8> {
        let mut bytes = serde_json::to_vec_pretty(self).unwrap_or_default();
        bytes.push(b'\n');
        bytes
    }

    /// The content addresses of the pictures the marks show, each once.
    pub fn pictures(&self) -> std::collections::BTreeSet<&str> {
        self.marks
            .iter()
            .filter_map(|mark| match &mark.body.shape {
                crate::model::Shape::Image { src, .. } => Some(src.as_str()),
                _ => None,
            })
            .filter(|src| ui::tilo_actions::content_hash(src).is_some())
            .collect()
    }

    /// Read an artwork's JSON.
    ///
    /// # Errors
    /// Says why the bytes are not an artwork this version reads.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let file: Self = serde_json::from_slice(bytes)
            .map_err(|error| format!("this is not a Canvas artwork: {error}"))?;
        if file.format != FORMAT {
            return Err(format!(
                "this is a {:?} file, not a Canvas artwork",
                file.format
            ));
        }
        if file.version != 1 {
            return Err(format!(
                "this artwork was saved by a newer Canvas (version {})",
                file.version
            ));
        }
        Ok(file)
    }
}

/// A file name for `title`: what a person would call it, without the
/// characters file systems refuse.
pub fn file_name(title: &str, extension: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let words = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let stem = if words.is_empty() {
        ui::i18n::I18nManager::lookup_plain("canvas-untitled")
    } else {
        words
    };
    format!("{stem}.{extension}")
}

/// An artwork opened or saved lately.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recent {
    /// Where the file is.
    pub path: String,
    pub title: String,
    pub width: f64,
    pub height: f64,
    /// When it was last opened or saved, in milliseconds since the epoch.
    pub at_ms: u64,
    /// A small picture of it, by content address, when one was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,
}

/// How many recent artworks are remembered.
pub const RECENTS_KEPT: usize = 8;

/// The artworks opened or saved lately, newest first. Their paths name
/// files on this device, so the list is a device-local setting.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RecentArtworks {
    pub artworks: Vec<Recent>,
}

impl ui::storage::SettingsSchema for RecentArtworks {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::DeviceLocal;
}

impl RecentArtworks {
    /// `recent` first, then the others it was not, at most
    /// [`RECENTS_KEPT`].
    pub fn remember(&mut self, recent: Recent) {
        self.artworks.retain(|kept| kept.path != recent.path);
        self.artworks.insert(0, recent);
        self.artworks.truncate(RECENTS_KEPT);
    }

    /// Forget the artwork at `path` (it moved or was removed).
    pub fn forget(&mut self, path: &str) {
        self.artworks.retain(|kept| kept.path != path);
    }
}

/// The package of `file`: its JSON and every picture its marks show that
/// `store` holds, as the bytes it holds them as. Also the pictures it does
/// not hold, by address: the file opens without them, as the artwork shows
/// now.
///
/// # Errors
/// Says why the package cannot be written (in memory, it always can).
pub fn pack(
    file: &ArtworkFile,
    store: &tilo_content::ContentStore,
) -> Result<(Vec<u8>, Vec<String>), String> {
    use std::io::Write as _;
    use zip::CompressionMethod::{Deflated, Stored};
    use zip::write::SimpleFileOptions;
    let failed = |error: &dyn std::fmt::Display| format!("the artwork cannot be packed: {error}");
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    // First and stored, so the type shows at a fixed place in the file.
    let stored = SimpleFileOptions::default().compression_method(Stored);
    zip.start_file("mimetype", stored)
        .map_err(|error| failed(&error))?;
    zip.write_all(MEDIA_TYPE.as_bytes())
        .map_err(|error| failed(&error))?;
    zip.start_file(
        DOCUMENT_ENTRY,
        SimpleFileOptions::default().compression_method(Deflated),
    )
    .map_err(|error| failed(&error))?;
    zip.write_all(&file.to_json())
        .map_err(|error| failed(&error))?;
    let mut missing = Vec::new();
    for address in file.pictures() {
        let held = ui::tilo_actions::content_hash(address)
            .and_then(|hash| Some((hash, store.read(hash)?)));
        let Some((hash, bytes)) = held else {
            missing.push(address.to_owned());
            continue;
        };
        // Pictures are compressed already: kept as they are.
        zip.start_file(format!("{CONTENT_DIR}{hash}"), stored)
            .map_err(|error| failed(&error))?;
        zip.write_all(&bytes).map_err(|error| failed(&error))?;
    }
    let bytes = zip.finish().map_err(|error| failed(&error))?.into_inner();
    Ok((bytes, missing))
}

/// Read a saved artwork: a package, its pictures put in `store`, or the
/// artwork's JSON alone.
///
/// # Errors
/// Says why the bytes are not an artwork this version reads, or a picture
/// in it is not the content its name says.
pub fn unpack(bytes: &[u8], store: &tilo_content::ContentStore) -> Result<ArtworkFile, String> {
    if !bytes.starts_with(b"PK\x03\x04") {
        return ArtworkFile::parse(bytes);
    }
    let broken = |error: &dyn std::fmt::Display| format!("this artwork file is damaged: {error}");
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|error| broken(&error))?;
    if archive.len() > crate::model::MAX_MARKS.saturating_add(2) {
        return Err(broken(&"it holds more pictures than an artwork can show"));
    }
    // What is inflated in all, held to a bound however the entries lie
    // about their sizes.
    let mut budget = (MAX_FILE_BYTES as u64).saturating_mul(2);
    let json = {
        let entry = archive
            .by_name(DOCUMENT_ENTRY)
            .map_err(|error| broken(&error))?;
        inflate(entry, MAX_FILE_BYTES as u64, &mut budget).map_err(|error| broken(&error))?
    };
    let file = ArtworkFile::parse(&json)?;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| broken(&error))?;
        let Some(hash) = entry.name().strip_prefix(CONTENT_DIR).map(str::to_owned) else {
            continue;
        };
        if !tilo_content::valid_hash(&hash) {
            return Err(broken(&format!("{hash:?} names no content")));
        }
        if store.has(&hash) {
            continue;
        }
        let picture = inflate(entry, crate::picture::MAX_PICTURE_BYTES as u64, &mut budget)
            .map_err(|error| broken(&error))?;
        if !tilo_content::verify(&hash, &picture) {
            return Err(broken(&format!("its picture {hash} is not that content")));
        }
        store
            .put(&picture)
            .map_err(|error| format!("the artwork's picture could not be kept: {error}"))?;
    }
    Ok(file)
}

/// The bytes of a package entry, at most `cap` of them and at most what is
/// left of `budget`, which they are taken from.
fn inflate(mut entry: impl std::io::Read, cap: u64, budget: &mut u64) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let cap = cap.min(*budget);
    let mut bytes = Vec::new();
    (&mut entry)
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let read = bytes.len() as u64;
    if read > cap {
        return Err(format!("an entry is over {cap} bytes"));
    }
    *budget = budget.saturating_sub(read);
    Ok(bytes)
}

/// Answer `save-artwork`: write the artwork's file to the path given, or
/// keep it as content.
///
/// # Errors
/// Says why the path cannot be written or the content kept.
pub fn save(
    art: &Artwork,
    save: &crate::resource::SaveArtwork,
) -> Result<serde_json::Value, String> {
    let (bytes, missing) = pack(&ArtworkFile::of(art), &crate::content::store())?;
    let size = bytes.len();
    let mut answer = match &save.path {
        None => serde_json::json!({"content": crate::resource::store(&bytes)?, "bytes": size}),
        Some(path) => {
            let path = std::path::Path::new(path);
            if !path.is_absolute()
                || path.extension().and_then(|extension| extension.to_str()) != Some(EXTENSION)
            {
                return Err(format!(
                    "path is an absolute path ending in .{EXTENSION}, not {}",
                    path.display()
                ));
            }
            write(path, &bytes)?;
            serde_json::json!({"path": path.display().to_string(), "bytes": size})
        }
    };
    if !missing.is_empty() {
        // Saved all the same: the file shows what the artwork shows now.
        if let Some(answer) = answer.as_object_mut() {
            answer.insert("missing_pictures".into(), serde_json::json!(missing));
        }
    }
    Ok(answer)
}

/// Write an export's `bytes` to `path`, an absolute path ending in one of
/// `extensions`; answers the path written.
///
/// # Errors
/// Says why the path is not one or cannot be written.
pub fn export_to(path: &str, extensions: &[&str], bytes: &[u8]) -> Result<String, String> {
    let file = std::path::Path::new(path);
    let extension = file
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    if !file.is_absolute()
        || !extension
            .as_deref()
            .is_some_and(|extension| extensions.contains(&extension))
    {
        return Err(format!(
            "path is an absolute path ending in .{}, not {path}",
            extensions.join(" or .")
        ));
    }
    write(file, bytes)?;
    Ok(file.display().to_string())
}

/// Write `bytes` to the file at `path`, replacing it.
///
/// # Errors
/// Says why the file cannot be written.
pub fn write(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let cannot = |error: tilo_files::FsError| format!("cannot write {}: {error}", path.display());
    let (dir, name) = tilo_files::file_at(path).map_err(cannot)?;
    tilo_files::blocking::write_atomic(&dir, &name, bytes).map_err(cannot)
}

/// Read the artwork file at `path`, its pictures put in the app's content
/// store.
///
/// # Errors
/// Says why it cannot be read or is not an artwork.
pub fn read(path: &std::path::Path) -> Result<ArtworkFile, String> {
    let bytes = crate::resource::read_file(&path.display().to_string(), MAX_FILE_BYTES)?;
    unpack(&bytes, &crate::content::store())
}
