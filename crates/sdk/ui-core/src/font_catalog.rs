use std::collections::HashSet;
#[cfg(not(windows))]
use std::io::Read;
use std::path::{Path, PathBuf};

use cosmic_text::{FontSystem, fontdb};
use serde::Deserialize;
use sha2::{Digest, Sha256};

mod fallback;
#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows;

const MANIFEST: &str = include_str!("../assets/fonts/manifest.toml");

/// Share verified font data across native Windows renderers. Each renderer
/// keeps its own shaping caches while font bytes and discovery are reused.
#[cfg(windows)]
pub(crate) fn windows_font_system(locale: String) -> Option<FontSystem> {
    static DATABASE: std::sync::OnceLock<Option<fontdb::Database>> = std::sync::OnceLock::new();
    let database = DATABASE.get_or_init(|| {
        let installed = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("fonts")))
            .filter(|root| root.is_dir());
        let root =
            installed.unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts"));
        match FontCatalog::product(root, locale.clone()).load() {
            Ok(font_system) => Some(font_system.into_locale_and_db().1),
            Err(error) => {
                tracing::error!(%error, "Windows font catalog unavailable");
                None
            }
        }
    });
    database.as_ref().map(|db| {
        FontSystem::new_with_locale_and_db_and_fallback(locale, db.clone(), fallback::TiloFallback)
    })
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Scope {
    Product,
    Fixture,
}

#[derive(Clone, Debug)]
pub struct FontCatalog {
    root: PathBuf,
    locale: String,
    fixtures: bool,
    system: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("Tilo font packaging error at {path}: {problem}")]
pub struct FontCatalogError {
    path: PathBuf,
    problem: String,
}

impl FontCatalogError {
    #[allow(
        clippy::needless_pass_by_value,
        reason = "owned conversion inputs let callers pass either paths or formatted errors without borrowing temporaries"
    )]
    fn at(path: impl Into<PathBuf>, problem: impl ToString) -> Self {
        Self {
            path: path.into(),
            problem: problem.to_string(),
        }
    }
}

#[derive(Deserialize)]
struct Manifest {
    schema: u32,
    font: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    scope: Scope,
    #[serde(default)]
    browser_startup: bool,
    file: String,
    bytes: u64,
    sha256: String,
    reserved_aliases: Vec<String>,
    faces: Vec<Face>,
}

#[derive(Deserialize)]
struct Face {
    index: u32,
    family: String,
    post_script_name: String,
    style: String,
}

pub fn catalog_manifest_sha256() -> String {
    hex::encode(Sha256::digest(MANIFEST.as_bytes()))
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn fixture_catalog() -> FontCatalog {
    FontCatalog::controlled_fixture(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts"),
        "en-US",
    )
}

#[cfg(test)]
pub(crate) fn fixture_font_system() -> FontSystem {
    fixture_catalog()
        .load()
        .expect("checked-in canonical font fixture")
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn fixture_renderer() -> crate::text_render::TextRenderer {
    crate::text_render::TextRenderer::with_font_catalog(&fixture_catalog())
        .expect("checked-in canonical renderer fixture")
}

impl FontCatalog {
    pub fn product(root: impl Into<PathBuf>, locale: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            locale: locale.into(),
            fixtures: false,
            system: true,
        }
    }

    pub fn controlled_fixture(root: impl Into<PathBuf>, locale: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            locale: locale.into(),
            fixtures: true,
            system: false,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn locale(&self) -> &str {
        &self.locale
    }

    ///
    /// # Errors
    ///
    /// Returns a [`FontCatalogError`] when the manifest or any declared font
    /// is absent, corrupt, or does not match its declared face metadata.
    pub fn load(&self) -> Result<FontSystem, FontCatalogError> {
        self.load_with_system(fontdb::Database::load_system_fonts)
    }

    // Injection is private, synchronous, and per-call: no test changes a global.
    #[allow(
        clippy::too_many_lines,
        reason = "font discovery, validation and database construction form one ordered loading transaction"
    )]
    fn load_with_system(
        &self,
        discover: impl FnOnce(&mut fontdb::Database),
    ) -> Result<FontSystem, FontCatalogError> {
        let root = self
            .root
            .canonicalize()
            .map_err(|e| FontCatalogError::at(&self.root, e))?;
        let manifest_path = root.join("manifest.toml");
        let shipped =
            std::fs::read(&manifest_path).map_err(|e| FontCatalogError::at(&manifest_path, e))?;
        if shipped != MANIFEST.as_bytes() {
            return Err(FontCatalogError::at(
                manifest_path,
                "manifest differs from this Tilo build",
            ));
        }
        self.build(
            &root,
            |asset| {
                let path = root.join(&asset.file);
                let metadata =
                    std::fs::metadata(&path).map_err(|e| FontCatalogError::at(&path, e))?;
                if !metadata.is_file() || metadata.len() != asset.bytes {
                    return Err(FontCatalogError::at(&path, "font size mismatch"));
                }
                if hash_file(&path)? != asset.sha256 {
                    return Err(FontCatalogError::at(&path, "SHA-256 mismatch"));
                }
                Ok(fontdb::Source::File(path))
            },
            discover,
        )
    }

    /// Load canonical assets from host-provided bytes, with no filesystem or ambient fonts.
    /// Every asset is size/hash/face validated before a font system is returned.
    /// # Errors
    /// Rejects missing, corrupt, or mismatched assets.
    pub fn load_bytes(
        &self,
        mut bytes: impl FnMut(&str) -> Option<Vec<u8>>,
    ) -> Result<FontSystem, FontCatalogError> {
        let mut portable = self.clone();
        portable.system = false;
        portable.build(
            Path::new("canonical-fonts"),
            |asset| {
                let data = bytes(&asset.file)
                    .ok_or_else(|| FontCatalogError::at(&asset.file, "missing font bytes"))?;
                if data.len() as u64 != asset.bytes
                    || hex::encode(Sha256::digest(&data)) != asset.sha256
                {
                    return Err(FontCatalogError::at(
                        &asset.file,
                        "font size or SHA-256 mismatch",
                    ));
                }
                Ok(fontdb::Source::Binary(std::sync::Arc::new(data)))
            },
            |_| {},
        )
    }

    /// Load the canonical core families required before a browser can mount.
    /// Script, symbol, math, and emoji assets remain absent until shaping
    /// reports demand and the host supplies their verified bytes.
    /// # Errors
    /// Rejects a missing or corrupt startup asset.
    pub fn load_browser_startup_bytes(
        &self,
        mut bytes: impl FnMut(&str) -> Option<Vec<u8>>,
    ) -> Result<FontSystem, FontCatalogError> {
        let mut portable = self.clone();
        portable.system = false;
        portable.build_selected(
            Path::new("canonical-browser-fonts"),
            |asset| asset.scope == Scope::Product && asset.browser_startup,
            |asset| {
                let data = bytes(&asset.file)
                    .ok_or_else(|| FontCatalogError::at(&asset.file, "missing font bytes"))?;
                verify_asset_bytes(asset, &data)?;
                Ok(fontdb::Source::Binary(std::sync::Arc::new(data)))
            },
            |_| {},
        )
    }

    /// Validate and add one canonical optional browser asset to a live font
    /// system. Returns `false` when the exact asset faces are already present.
    /// # Errors
    /// Rejects unknown names, corrupt bytes, or mismatched face metadata.
    pub fn install_browser_asset(
        &self,
        system: &mut FontSystem,
        name: &str,
        data: Vec<u8>,
    ) -> Result<bool, FontCatalogError> {
        let manifest = parse_manifest(Path::new("canonical-browser-fonts/manifest.toml"))?;
        let asset = manifest
            .font
            .iter()
            .find(|asset| asset.scope == Scope::Product && asset.file == name)
            .ok_or_else(|| FontCatalogError::at(name, "unknown canonical font asset"))?;
        verify_asset_bytes(asset, &data)?;
        if asset.faces.iter().all(|expected| {
            system.db().faces().any(|face| {
                face.index == expected.index
                    && face.post_script_name == expected.post_script_name
                    && face
                        .families
                        .iter()
                        .any(|family| family.0 == expected.family)
            })
        }) {
            return Ok(false);
        }
        let source = fontdb::Source::Binary(std::sync::Arc::new(data));
        // Validate in an isolated database first. Even an internally
        // inconsistent release manifest must not partially mutate the live
        // renderer before returning its packaging error.
        let mut probe = fontdb::Database::new();
        let probe_ids = probe.load_font_source(source.clone());
        validate_faces(&probe, asset, &probe_ids, Path::new(name))?;
        let db = system.db_mut();
        let ids = db.load_font_source(source);
        validate_faces(db, asset, &ids, Path::new(name))?;
        Ok(true)
    }

    fn build(
        &self,
        root: &Path,
        source: impl FnMut(&Asset) -> Result<fontdb::Source, FontCatalogError>,
        discover: impl FnOnce(&mut fontdb::Database),
    ) -> Result<FontSystem, FontCatalogError> {
        self.build_selected(root, |_| true, source, discover)
    }

    fn build_selected(
        &self,
        root: &Path,
        include: impl Fn(&Asset) -> bool,
        mut source: impl FnMut(&Asset) -> Result<fontdb::Source, FontCatalogError>,
        discover: impl FnOnce(&mut fontdb::Database),
    ) -> Result<FontSystem, FontCatalogError> {
        let manifest_path = root.join("manifest.toml");
        let manifest = parse_manifest(&manifest_path)?;
        let mut db = fontdb::Database::new();
        let mut canonical = HashSet::new();
        let mut reserved = HashSet::new();
        for asset in &manifest.font {
            if (asset.scope == Scope::Fixture && !self.fixtures) || !include(asset) {
                continue;
            }
            reserved.extend(asset.reserved_aliases.iter().map(|s| s.to_lowercase()));
            let path = root.join(&asset.file);
            let ids = db.load_font_source(source(asset)?);
            validate_faces(&db, asset, &ids, &path)?;
            canonical.extend(ids);
        }
        if self.system {
            discover(&mut db);
            let remove: Vec<_> = db
                .faces()
                .filter(|face| {
                    !canonical.contains(&face.id)
                        && face
                            .families
                            .iter()
                            .any(|name| reserved.contains(&name.0.to_lowercase()))
                })
                .map(|face| face.id)
                .collect();
            for id in remove {
                db.remove_face(id);
            }
        }
        db.set_sans_serif_family(crate::text_render::UI_SANS_FAMILY);
        db.set_serif_family(crate::text_render::UI_SERIF_FAMILY);
        db.set_monospace_family(crate::text_render::UI_MONOSPACE_FAMILY);
        for asset in &manifest.font {
            if (asset.scope == Scope::Fixture && !self.fixtures) || !include(asset) {
                continue;
            }
            for face in &asset.faces {
                let query = fontdb::Query {
                    families: &[fontdb::Family::Name(&face.family)],
                    style: if face.style == "italic" {
                        fontdb::Style::Italic
                    } else {
                        fontdb::Style::Normal
                    },
                    ..fontdb::Query::default()
                };
                if !db.query(&query).is_some_and(|id| canonical.contains(&id)) {
                    return Err(FontCatalogError::at(
                        &root,
                        format!("required canonical family is unavailable: {}", face.family),
                    ));
                }
            }
        }
        Ok(FontSystem::new_with_locale_and_db_and_fallback(
            self.locale.clone(),
            db,
            fallback::TiloFallback,
        ))
    }
}

fn parse_manifest(path: &Path) -> Result<Manifest, FontCatalogError> {
    let manifest: Manifest =
        toml::from_str(MANIFEST).map_err(|error| FontCatalogError::at(path, error))?;
    if manifest.schema != 1 {
        return Err(FontCatalogError::at(path, "unsupported catalog schema"));
    }
    Ok(manifest)
}

fn verify_asset_bytes(asset: &Asset, data: &[u8]) -> Result<(), FontCatalogError> {
    if data.len() as u64 != asset.bytes || hex::encode(Sha256::digest(data)) != asset.sha256 {
        return Err(FontCatalogError::at(
            &asset.file,
            "font size or SHA-256 mismatch",
        ));
    }
    Ok(())
}

fn validate_faces(
    db: &fontdb::Database,
    asset: &Asset,
    ids: &[fontdb::ID],
    path: &Path,
) -> Result<(), FontCatalogError> {
    if ids.len() != asset.faces.len() {
        return Err(FontCatalogError::at(
            path,
            format!("expected {} faces, loaded {}", asset.faces.len(), ids.len()),
        ));
    }
    for expected in &asset.faces {
        let found = ids
            .iter()
            .filter_map(|id| db.face(*id))
            .find(|face| face.index == expected.index);
        let valid = found.is_some_and(|face| {
            face.families.iter().any(|name| name.0 == expected.family)
                && face.post_script_name == expected.post_script_name
                && face.style
                    == if expected.style == "italic" {
                        fontdb::Style::Italic
                    } else {
                        fontdb::Style::Normal
                    }
        });
        if !valid {
            return Err(FontCatalogError::at(
                path,
                format!(
                    "face {} must be {} / {} ({})",
                    expected.index, expected.family, expected.post_script_name, expected.style
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn hash_file(path: &Path) -> Result<String, FontCatalogError> {
    let file = std::fs::File::open(path).map_err(|error| FontCatalogError::at(path, error))?;
    windows::digest(file)
        .map(hex::encode)
        .map_err(|error| FontCatalogError::at(path, error))
}

#[cfg(not(windows))]
#[allow(
    clippy::large_stack_arrays,
    reason = "the fixed 64 KiB hashing buffer avoids heap allocation during font discovery"
)]
fn hash_file(path: &Path) -> Result<String, FontCatalogError> {
    let mut file = std::fs::File::open(path).map_err(|e| FontCatalogError::at(path, e))?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|e| FontCatalogError::at(path, e))?;
        if n == 0 {
            break;
        }
        let chunk = buffer
            .get(..n)
            .ok_or_else(|| FontCatalogError::at(path, "font read exceeded its buffer"))?;
        hash.update(chunk);
    }
    Ok(hex::encode(hash.finalize()))
}

///
/// # Errors
///
/// Propagates the [`FontCatalogError`] returned by [`FontCatalog::load`].
pub fn configured_font_system_with_catalog(
    catalog: &FontCatalog,
) -> Result<FontSystem, FontCatalogError> {
    catalog.load()
}
