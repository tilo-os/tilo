//! The wallpapers Tilo ships.
//!
//! [`catalog.toml`](../catalog.toml) is the one list: each picture's id, its
//! author, licence and source, and the two files the OS image installs for it
//! under `backgrounds/tilo/` in the XDG data directories — the picture at
//! the laptop panel's resolution and a thumbnail for the Settings picker.
//! The files are not in the source tree: `tools/wallpapers.py` makes them
//! from the pinned originals, and the image build fetches them by digest
//! (`os/Containerfile`, stage `os-wallpapers`).
//!
//! The person's choice is the `appearance.wallpaper` setting: a catalog id,
//! [`CUSTOM`] for a picture of their own (set through the Wallpaper portal),
//! or unset for the catalog's default. [`choice`] reads it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

/// The `appearance.wallpaper` value of a picture the person chose
/// themselves, which the compositor keeps in its private wallpaper cache.
pub const CUSTOM: &str = "custom";

/// Where under each XDG data directory the image installs the pictures.
pub const INSTALL_SUBDIR: &str = "backgrounds/tilo";

/// Whether a picture reads as light or dark behind windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    Light,
    Mid,
    Dark,
}

/// One installed file of a wallpaper, pinned by size and digest.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub file: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    pub sha256: String,
}

/// One wallpaper of the catalog.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wallpaper {
    /// Stable id, the `appearance.wallpaper` value that chooses it.
    pub id: String,
    pub tone: Tone,
    /// The work's title at its source.
    pub title: String,
    pub author: String,
    /// SPDX identifier, or `public-domain` for a work with no copyright.
    pub license: String,
    pub license_url: String,
    /// The page that states the author and the licence.
    pub source_url: String,
    /// The original file the installed ones are made from.
    pub original_url: String,
    pub original_sha256: String,
    pub picture: Asset,
    pub thumbnail: Asset,
}

/// The catalog format this build reads.
const SCHEMA: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    schema: u32,
    default: String,
    wallpaper: Vec<Wallpaper>,
}

fn parsed() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    // The tests parse the same text; a build whose catalog does not parse
    // fails them, so the empty catalog is never what ships.
    CATALOG.get_or_init(|| {
        toml::from_str(include_str!("../catalog.toml"))
            .ok()
            .filter(|catalog: &Catalog| catalog.schema == SCHEMA)
            .unwrap_or(Catalog {
                schema: 0,
                default: String::new(),
                wallpaper: Vec::new(),
            })
    })
}

/// Every wallpaper, in the order the picker shows them.
#[must_use]
pub fn all() -> &'static [Wallpaper] {
    &parsed().wallpaper
}

/// The wallpaper a person who has chosen none sees.
#[must_use]
pub fn default_wallpaper() -> Option<&'static Wallpaper> {
    let catalog = parsed();
    find(&catalog.default).or_else(|| catalog.wallpaper.first())
}

/// The wallpaper with `id`.
#[must_use]
pub fn find(id: &str) -> Option<&'static Wallpaper> {
    all().iter().find(|wallpaper| wallpaper.id == id)
}

/// What an `appearance.wallpaper` value shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// A wallpaper of the catalog.
    Bundled(&'static Wallpaper),
    /// The person's own picture.
    Custom,
}

/// Read an `appearance.wallpaper` value. Unset, or an id this build does not
/// know (a newer device's choice), is the default.
#[must_use]
pub fn choice(value: Option<&str>) -> Option<Choice> {
    match value {
        Some(CUSTOM) => Some(Choice::Custom),
        Some(id) => find(id).or_else(default_wallpaper).map(Choice::Bundled),
        None => default_wallpaper().map(Choice::Bundled),
    }
}

/// The `appearance.wallpaper` value a choice is stored as.
#[must_use]
pub fn value_of(choice: Choice) -> &'static str {
    match choice {
        Choice::Bundled(wallpaper) => &wallpaper.id,
        Choice::Custom => CUSTOM,
    }
}

/// Every value `appearance.wallpaper` may hold.
#[must_use]
pub fn values() -> Vec<&'static str> {
    all()
        .iter()
        .map(|wallpaper| wallpaper.id.as_str())
        .chain([CUSTOM])
        .collect()
}

/// Decoded straight-alpha RGBA8 pixels.
pub use tilo_image_codec::DecodedImage;

impl Wallpaper {
    /// The installed picture, from the first XDG data directory that has it.
    #[must_use]
    pub fn picture_path(&self) -> Option<PathBuf> {
        installed(&self.picture)
    }

    /// The installed thumbnail.
    #[must_use]
    pub fn thumbnail_path(&self) -> Option<PathBuf> {
        installed(&self.thumbnail)
    }

    /// Decode the installed picture, at its full size.
    #[must_use]
    pub fn decode_picture(&self) -> Option<DecodedImage> {
        decode(&self.picture_path()?, &self.picture)
    }

    /// Decode the installed thumbnail.
    #[must_use]
    pub fn decode_thumbnail(&self) -> Option<DecodedImage> {
        decode(&self.thumbnail_path()?, &self.thumbnail)
    }
}

fn installed(asset: &Asset) -> Option<PathBuf> {
    tilo_paths::xdg::data_dirs()
        .into_iter()
        .map(|dir| dir.join(INSTALL_SUBDIR).join(&asset.file))
        .find(|path| path.is_file())
}

/// Larger than any installed picture: a file that is not the one the
/// catalog describes is refused before it is read.
const MAX_ASSET_BYTES: u64 = 16 * 1024 * 1024;

fn decode(path: &Path, asset: &Asset) -> Option<DecodedImage> {
    let length = std::fs::metadata(path).ok()?.len();
    if length > MAX_ASSET_BYTES {
        return None;
    }
    let encoded = std::fs::read(path).ok()?;
    tilo_image_codec::DecodeImage {
        encoded: &encoded,
        max_dimension: asset.width.max(asset.height),
    }
    .decode_native()
    .ok()
}

#[cfg(test)]
mod tests;
