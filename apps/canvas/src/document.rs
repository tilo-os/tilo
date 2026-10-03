//! The artwork as a document: the sizes and grounds a new one starts from,
//! the file it is kept as in Files, and the artworks opened or saved lately.
//!
//! A window holds one artwork. Starting a new one or opening a file
//! replaces it as one step of its history, so Undo brings the old one back;
//! a new tab starts a separate artwork beside it.
//!
//! The file (`.artwork`) is the artwork's content as JSON: its title, size,
//! ground, layers in order, marks in order and the media saved in it. It is
//! the same data every collaborator simulates from, so a painting opened
//! from a file paints exactly as it was saved.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ui::icons::Icon;

use crate::media::MediumSpec;
use crate::model::{Artwork, Ground, MarkBody};

/// The file extension of a saved artwork.
pub const EXTENSION: &str = "artwork";
/// The media type of a saved artwork.
pub const MEDIA_TYPE: &str = "application/vnd.tilo.artwork+json";
/// What the file says it is.
const FORMAT: &str = "tilo-artwork";
/// The largest artwork file read, in bytes.
pub const MAX_FILE_BYTES: usize = 64 << 20;

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
pub const SIZES: [Size; 6] = [
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
pub const STARTS: [Start; 6] = [
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
                    medium: layer.medium.clone(),
                    opacity: layer.opacity,
                    visible: layer.visible,
                    texture: layer.texture,
                    locked: layer.locked,
                    mask_of: layer.mask_of.clone(),
                    base: layer.base,
                    invert: layer.invert,
                    clip: layer.clip,
                })
                .collect(),
            marks: art
                .marks
                .iter()
                .map(|mark| FileMark {
                    id: mark.id.clone(),
                    layer: mark.layer.clone(),
                    body: MarkBody {
                        name: mark.name.clone(),
                        shape: mark.shape.clone(),
                        style: mark.style.clone(),
                        transform: mark.transform,
                        group: mark.group.clone(),
                        clip: mark.clip.clone(),
                    },
                })
                .collect(),
            media: art.media.clone(),
        }
    }

    /// The file's bytes: indented JSON, so a saved artwork diffs and merges
    /// as text too.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = serde_json::to_vec_pretty(self).unwrap_or_default();
        bytes.push(b'\n');
        bytes
    }

    /// Read a saved artwork.
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

/// The recent artworks, newest first, as kept in the app's own state.
pub mod recents {
    use super::{RECENTS_KEPT, Recent};
    use tilo_files::{Dir, VPath, blocking};

    fn place() -> Option<(Dir, VPath)> {
        let dir = Dir::open(tilo_paths::AppPaths::for_app(crate::APP_ID).state()).ok()?;
        Some((dir, VPath::new("recent-artworks.json").ok()?))
    }

    /// The recent artworks, newest first; none where the app keeps no
    /// state.
    pub fn read() -> Vec<Recent> {
        let Some((dir, path)) = place() else {
            return Vec::new();
        };
        blocking::read(&dir, &path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// `recent` first, then the others it was not, at most
    /// [`RECENTS_KEPT`].
    pub fn with(mut list: Vec<Recent>, recent: Recent) -> Vec<Recent> {
        list.retain(|kept| kept.path != recent.path);
        list.insert(0, recent);
        list.truncate(RECENTS_KEPT);
        list
    }

    /// Remember `recent`. Best effort: a machine that keeps no state for
    /// the app simply shows no recent artworks.
    pub fn remember(recent: Recent) -> Vec<Recent> {
        let list = with(read(), recent);
        if let Some((dir, path)) = place()
            && let Ok(bytes) = serde_json::to_vec_pretty(&list)
        {
            let _ = blocking::write_atomic(&dir, &path, &bytes);
        }
        list
    }

    /// Forget the artwork at `path` (it moved or was removed).
    pub fn forget(path: &str) -> Vec<Recent> {
        let mut list = read();
        list.retain(|kept| kept.path != path);
        if let Some((dir, file)) = place()
            && let Ok(bytes) = serde_json::to_vec_pretty(&list)
        {
            let _ = blocking::write_atomic(&dir, &file, &bytes);
        }
        list
    }
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
    let bytes = ArtworkFile::of(art).to_bytes();
    let size = bytes.len();
    let Some(path) = &save.path else {
        let address = crate::resource::store(&bytes)?;
        return Ok(serde_json::json!({"content": address, "bytes": size}));
    };
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
    Ok(serde_json::json!({"path": path.display().to_string(), "bytes": size}))
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

/// Read the artwork file at `path`.
///
/// # Errors
/// Says why it cannot be read or is not an artwork.
pub fn read(path: &std::path::Path) -> Result<ArtworkFile, String> {
    let bytes = crate::resource::read_file(&path.display().to_string(), MAX_FILE_BYTES)?;
    ArtworkFile::parse(&bytes)
}
