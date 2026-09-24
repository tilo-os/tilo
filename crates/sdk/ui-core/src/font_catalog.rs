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
        reason = "The error constructor accepts owned diagnostics and string literals and stores their formatted message."
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

#[cfg(any(test, all(feature = "native", feature = "test-support")))]
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

#[cfg(any(test, all(feature = "native", feature = "test-support")))]
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
        reason = "Catalog loading keeps manifest/hash/face validation, system-font filtering, and canonical-family checks in one sequence."
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
        let manifest: Manifest =
            toml::from_str(MANIFEST).map_err(|e| FontCatalogError::at(&manifest_path, e))?;
        if manifest.schema != 1 {
            return Err(FontCatalogError::at(
                manifest_path,
                "unsupported catalog schema",
            ));
        }
        let mut db = fontdb::Database::new();
        let mut canonical = HashSet::new();
        let mut reserved = HashSet::new();
        for asset in &manifest.font {
            if asset.scope == Scope::Fixture && !self.fixtures {
                continue;
            }
            // Only this catalog's canonical faces reserve names. Product mode
            // can still discover an ambient Adwaita as a user-named font.
            reserved.extend(asset.reserved_aliases.iter().map(|s| s.to_lowercase()));
            let path = root.join(&asset.file);
            let metadata = std::fs::metadata(&path).map_err(|e| FontCatalogError::at(&path, e))?;
            if !metadata.is_file() || metadata.len() != asset.bytes {
                return Err(FontCatalogError::at(
                    &path,
                    format!("expected regular file of {} bytes", asset.bytes),
                ));
            }
            let actual = hash_file(&path)?;
            if actual != asset.sha256 {
                return Err(FontCatalogError::at(
                    &path,
                    format!(
                        "SHA-256 mismatch: expected {}, found {actual}",
                        asset.sha256
                    ),
                ));
            }
            let ids = db.load_font_source(fontdb::Source::File(path.clone()));
            if ids.len() != asset.faces.len() {
                return Err(FontCatalogError::at(
                    &path,
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
                        &path,
                        format!(
                            "face {} must be {} / {} ({})",
                            expected.index,
                            expected.family,
                            expected.post_script_name,
                            expected.style
                        ),
                    ));
                }
            }
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
            if asset.scope == Scope::Fixture && !self.fixtures {
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
    reason = "A single fixed 64 KiB stack buffer bounds memory while streaming each font into SHA-256."
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
