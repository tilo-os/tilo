//! The faces of a catalog, recorded beside its files when the catalog is
//! built.
//!
//! Every process that sets text builds a font database, and the catalog's
//! part of it costs the same parse each time: fontdb opens each of the
//! catalog's files and reads every face's names, style, weight and width.
//! Those answers are a function of the files, and the files are pinned by
//! the manifest, so the build that lays a catalog out records them once, as
//! [`FILE`] in the catalog's directory: the signed runtime bundle's
//! `lib/fonts` (mounted into every app sandbox), and its hard-linked copies
//! in the image. A reader that finds a record of this manifest in a catalog
//! the image's integrity pins ([`crate::Trust::Immutable`]) builds the
//! catalog's faces from it and parses no font; any other reader, or any
//! other catalog, parses as before.
//!
//! Paths are relative to the catalog, so the record holds wherever the
//! catalog is mounted: the shell reads the image's copy, an app its
//! sandbox's `/run/tilo-fonts`, from the same bytes.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Error, Scope, manifest, manifest_sha256};

/// The record's file name in a catalog directory.
pub const FILE: &str = "face-index.json";

/// Bumped whenever what a record holds, or how a face is made from it,
/// changes.
const VERSION: u32 = 1;

/// The faces fontdb finds in each file of one catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaceIndex {
    version: u32,
    /// SHA-256 of the manifest whose files these are.
    manifest: String,
    files: Vec<IndexedFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct IndexedFile {
    /// The manifest's path, relative to the catalog.
    file: String,
    faces: Vec<IndexedFace>,
}

/// One face, as fontdb describes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedFace {
    index: u32,
    families: Vec<String>,
    post_script_name: String,
    /// 0 normal, 1 italic, 2 oblique.
    style: u8,
    weight: u16,
    /// fontdb's width, 1 (ultra-condensed) to 9 (ultra-expanded).
    stretch: u16,
    monospaced: bool,
}

impl IndexedFace {
    /// What fontdb read of `face`.
    #[must_use]
    pub fn of(face: &fontdb::FaceInfo) -> Self {
        Self {
            index: face.index,
            families: face.families.iter().map(|(name, _)| name.clone()).collect(),
            post_script_name: face.post_script_name.clone(),
            style: match face.style {
                fontdb::Style::Normal => 0,
                fontdb::Style::Italic => 1,
                fontdb::Style::Oblique => 2,
            },
            weight: face.weight.0,
            stretch: face.stretch.to_number(),
            monospaced: face.monospaced,
        }
    }

    /// The face fontdb would have read, its bytes at `source`.
    #[must_use]
    pub fn info(&self, source: fontdb::Source) -> fontdb::FaceInfo {
        fontdb::FaceInfo {
            id: fontdb::ID::dummy(),
            source,
            index: self.index,
            families: self
                .families
                .iter()
                .map(|name| (name.clone(), fontdb::Language::English_UnitedStates))
                .collect(),
            post_script_name: self.post_script_name.clone(),
            style: match self.style {
                1 => fontdb::Style::Italic,
                2 => fontdb::Style::Oblique,
                _ => fontdb::Style::Normal,
            },
            weight: fontdb::Weight(self.weight),
            stretch: stretch(self.stretch),
            monospaced: self.monospaced,
        }
    }
}

fn stretch(number: u16) -> fontdb::Stretch {
    match number {
        1 => fontdb::Stretch::UltraCondensed,
        2 => fontdb::Stretch::ExtraCondensed,
        3 => fontdb::Stretch::Condensed,
        4 => fontdb::Stretch::SemiCondensed,
        6 => fontdb::Stretch::SemiExpanded,
        7 => fontdb::Stretch::Expanded,
        8 => fontdb::Stretch::ExtraExpanded,
        9 => fontdb::Stretch::UltraExpanded,
        _ => fontdb::Stretch::Normal,
    }
}

impl FaceIndex {
    /// Parse every file of the catalog at `root` this build's manifest
    /// names (fixtures aside) and record its faces. A file the catalog does
    /// not hold (the standalone catalog has no `Os` files) is left out.
    ///
    /// # Errors
    ///
    /// When the catalog is not this build's, or a file it holds is not the
    /// pinned file or holds no face.
    pub fn build(root: &Path) -> Result<Self, Error> {
        crate::check_manifest(root)?;
        let mut files = Vec::new();
        for asset in &manifest()?.font {
            if asset.scope == Scope::Fixture {
                continue;
            }
            let path = asset.path_in(root)?;
            if !path.exists() {
                continue;
            }
            asset.verify(root)?;
            let mut db = fontdb::Database::new();
            let ids = db.load_font_source(fontdb::Source::File(path.clone()));
            let faces: Vec<IndexedFace> = ids
                .iter()
                .filter_map(|id| db.face(*id))
                .map(IndexedFace::of)
                .collect();
            if faces.is_empty() {
                return Err(Error::at(&path, "holds no face fontdb reads"));
            }
            files.push(IndexedFile {
                file: asset.file.clone(),
                faces,
            });
        }
        Ok(Self {
            version: VERSION,
            manifest: manifest_sha256(),
            files,
        })
    }

    /// Write the record into the catalog at `root`.
    ///
    /// # Errors
    ///
    /// When it cannot be written.
    pub fn write(&self, root: &Path) -> Result<(), Error> {
        let path = root.join(FILE);
        let mut bytes = serde_json::to_vec_pretty(self).map_err(|e| Error::at(&path, e))?;
        bytes.push(b'\n');
        std::fs::write(&path, bytes).map_err(|e| Error::at(&path, e))
    }

    /// The record in the catalog at `root`, when there is one of this
    /// build's manifest. Whether the catalog's storage makes it worth
    /// believing is the reader's question ([`crate::Trust`]).
    #[must_use]
    pub fn read(root: &Path) -> Option<Self> {
        let bytes = std::fs::read(root.join(FILE)).ok()?;
        let index: Self = serde_json::from_slice(&bytes).ok()?;
        (index.version == VERSION && index.manifest == manifest_sha256()).then_some(index)
    }

    /// [`Self::read`], for a catalog read as `trust`: only a record the
    /// image's integrity pins like the fonts themselves (an
    /// [immutable](crate::file_is_immutable) file in an immutable catalog) is
    /// believed; anywhere else the fonts are parsed.
    #[must_use]
    pub fn read_trusted(root: &Path, trust: crate::Trust) -> Option<Self> {
        if trust != crate::Trust::Immutable || !crate::file_is_immutable(&root.join(FILE)) {
            return None;
        }
        Self::read(root)
    }

    /// The faces recorded for the manifest's `file`, in fontdb's order.
    #[must_use]
    pub fn faces(&self, file: &str) -> Option<&[IndexedFace]> {
        self.files
            .iter()
            .find(|indexed| indexed.file == file)
            .map(|indexed| indexed.faces.as_slice())
    }
}
