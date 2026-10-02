use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use cosmic_text::{FontSystem, fontdb};
#[cfg(test)]
use tilo_font_catalog::MANIFEST;
use tilo_font_catalog::{Asset, Face, Manifest, Scope};

#[cfg(all(target_os = "android", feature = "native"))]
mod android;
mod fallback;
#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows;

#[cfg(all(target_os = "android", feature = "native"))]
pub(crate) use android::android_font_system;

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

/// A font system over an already verified database, with Tilo's fallback.
#[cfg_attr(
    test,
    allow(dead_code, reason = "unit tests build the fixture font system instead")
)]
/// Keep one face per family, style, weight and stretch. A system can install
/// the same face twice (Fedora ships Noto Sans both static and variable), and
/// a query takes whichever was discovered first, which is directory order.
/// The face with the lowest source path stays.
pub(crate) fn drop_duplicate_faces(db: &mut fontdb::Database) {
    let mut kept: HashMap<_, (fontdb::ID, PathBuf, u32)> = HashMap::new();
    let mut remove = Vec::new();
    for face in db.faces() {
        let path = match &face.source {
            fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => path.clone(),
            fontdb::Source::Binary(_) => continue,
        };
        let key = (
            face.families
                .iter()
                .map(|(name, _)| name.to_lowercase())
                .collect::<Vec<_>>(),
            face.style,
            face.weight,
            face.stretch,
        );
        match kept.get_mut(&key) {
            Some(existing) if (&path, face.index) < (&existing.1, existing.2) => {
                remove.push(existing.0);
                *existing = (face.id, path, face.index);
            }
            Some(_) => remove.push(face.id),
            None => {
                kept.insert(key, (face.id, path, face.index));
            }
        }
    }
    for id in remove {
        db.remove_face(id);
    }
}

pub(crate) fn system_over(locale: String, db: fontdb::Database) -> FontSystem {
    FontSystem::new_with_locale_and_db_and_fallback(locale, db, fallback::TiloFallback)
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

impl From<tilo_font_catalog::Error> for FontCatalogError {
    fn from(error: tilo_font_catalog::Error) -> Self {
        Self {
            path: error.path,
            problem: error.problem,
        }
    }
}

pub fn catalog_manifest_sha256() -> String {
    tilo_font_catalog::manifest_sha256()
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
#[allow(
    clippy::expect_used,
    reason = "test support only: the checked-in fixture fonts always load, and a test cannot run without them"
)]
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
    fn load_with_system(
        &self,
        discover: impl FnOnce(&mut fontdb::Database),
    ) -> Result<FontSystem, FontCatalogError> {
        let root = self
            .root
            .canonicalize()
            .map_err(|e| FontCatalogError::at(&self.root, e))?;
        let bundle = verified_bundle(&root, self.fixtures)?;
        let mut db = bundle.db.clone();
        if self.system {
            discover(&mut db);
            drop_duplicate_faces(&mut db);
            let remove: Vec<_> = db
                .faces()
                .filter(|face| {
                    !bundle.canonical.contains(&face.id)
                        && face
                            .families
                            .iter()
                            .any(|name| bundle.reserved.contains(&name.0.to_lowercase()))
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
        for (family, style) in &bundle.required {
            let query = fontdb::Query {
                families: &[fontdb::Family::Name(family)],
                style: *style,
                ..fontdb::Query::default()
            };
            if !db
                .query(&query)
                .is_some_and(|id| bundle.canonical.contains(&id))
            {
                return Err(FontCatalogError::at(
                    &root,
                    format!("required canonical family is unavailable: {family}"),
                ));
            }
        }
        Ok(FontSystem::new_with_locale_and_db_and_fallback(
            self.locale.clone(),
            db,
            fallback::TiloFallback,
        ))
    }

    /// Load canonical assets from host-provided bytes, with no filesystem or ambient fonts.
    /// Every asset is size/hash/face validated before a font system is returned.
    /// # Errors
    /// Rejects missing, corrupt, or mismatched assets.
    pub fn load_bytes(
        &self,
        mut bytes: impl FnMut(&str) -> Option<Vec<u8>>,
    ) -> Result<FontSystem, FontCatalogError> {
        self.build_from_bytes(
            Path::new("canonical-fonts"),
            |_| true,
            |asset| {
                let data = bytes(&asset.file)
                    .ok_or_else(|| FontCatalogError::at(&asset.file, "missing font bytes"))?;
                verify_asset_bytes(asset, &data)?;
                Ok(fontdb::Source::Binary(Arc::new(data)))
            },
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
        self.build_from_bytes(
            Path::new("canonical-browser-fonts"),
            |asset| asset.scope == Scope::Product && asset.browser_startup,
            |asset| {
                let data = bytes(&asset.file)
                    .ok_or_else(|| FontCatalogError::at(&asset.file, "missing font bytes"))?;
                verify_asset_bytes(asset, &data)?;
                Ok(fontdb::Source::Binary(Arc::new(data)))
            },
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
        let source = fontdb::Source::Binary(Arc::new(data));
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

    /// Build a portable font system from host-supplied, already verified
    /// asset sources. No ambient/system fonts are ever discovered here.
    fn build_from_bytes(
        &self,
        root: &Path,
        include: impl Fn(&Asset) -> bool,
        mut source: impl FnMut(&Asset) -> Result<fontdb::Source, FontCatalogError>,
    ) -> Result<FontSystem, FontCatalogError> {
        let manifest_path = root.join("manifest.toml");
        let manifest = parse_manifest(&manifest_path)?;
        let mut db = fontdb::Database::new();
        let mut canonical = HashSet::new();
        let mut required = Vec::new();
        for asset in &manifest.font {
            if (asset.scope == Scope::Fixture && !self.fixtures) || !include(asset) {
                continue;
            }
            let path = root.join(&asset.file);
            let ids = db.load_font_source(source(asset)?);
            validate_faces(&db, asset, &ids, &path)?;
            canonical.extend(ids);
            required.extend(
                asset
                    .faces
                    .iter()
                    .map(|face| (face.family.clone(), face_style(face))),
            );
        }
        db.set_sans_serif_family(crate::text_render::UI_SANS_FAMILY);
        db.set_serif_family(crate::text_render::UI_SERIF_FAMILY);
        db.set_monospace_family(crate::text_render::UI_MONOSPACE_FAMILY);
        for (family, style) in &required {
            let query = fontdb::Query {
                families: &[fontdb::Family::Name(family)],
                style: *style,
                ..fontdb::Query::default()
            };
            if !db.query(&query).is_some_and(|id| canonical.contains(&id)) {
                return Err(FontCatalogError::at(
                    root,
                    format!("required canonical family is unavailable: {family}"),
                ));
            }
        }
        Ok(FontSystem::new_with_locale_and_db_and_fallback(
            self.locale.clone(),
            db,
            fallback::TiloFallback,
        ))
    }
}

fn face_style(face: &Face) -> fontdb::Style {
    if face.style == "italic" {
        fontdb::Style::Italic
    } else {
        fontdb::Style::Normal
    }
}

/// The compiled-in catalog manifest, reported against `path` on failure.
fn parse_manifest(path: &Path) -> Result<&'static Manifest, FontCatalogError> {
    tilo_font_catalog::manifest().map_err(|error| FontCatalogError::at(path, error.problem))
}

fn verify_asset_bytes(asset: &Asset, data: &[u8]) -> Result<(), FontCatalogError> {
    Ok(asset.verify_bytes(Path::new(&asset.file), data)?)
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
                && face.style == face_style(expected)
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

/// The catalog's own faces after their manifest, sizes, hashes and face
/// metadata have been checked, before any host font is added.
struct VerifiedBundle {
    db: fontdb::Database,
    canonical: HashSet<fontdb::ID>,
    reserved: HashSet<String>,
    required: Vec<(String, fontdb::Style)>,
}

type VerifiedBundles = HashMap<(PathBuf, bool), Arc<VerifiedBundle>>;

/// Verify a catalog root once per process and hand every later load a clone
/// of the resulting database. Hashing the ~43 MB of shipped fonts dominates
/// loading (about 1.5 s in a debug build), while cloning the database copies
/// only face metadata: the faces stay file-backed. A root that fails
/// verification is not remembered, so a repaired installation loads on the
/// next attempt. The lock is held across verification so concurrent first
/// loads hash the fonts once rather than once per caller.
fn verified_bundle(root: &Path, fixtures: bool) -> Result<Arc<VerifiedBundle>, FontCatalogError> {
    static VERIFIED: OnceLock<Mutex<VerifiedBundles>> = OnceLock::new();
    let mut verified = VERIFIED
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let key = (root.to_path_buf(), fixtures);
    if let Some(bundle) = verified.get(&key) {
        return Ok(Arc::clone(bundle));
    }
    let bundle = Arc::new(verify_bundle(root, fixtures)?);
    verified.insert(key, Arc::clone(&bundle));
    Ok(bundle)
}

#[allow(
    clippy::too_many_lines,
    reason = "Bundle verification keeps manifest, size, hash and face validation in one sequence."
)]
fn verify_bundle(root: &Path, fixtures: bool) -> Result<VerifiedBundle, FontCatalogError> {
    tilo_font_catalog::check_manifest(root)?;
    let manifest = tilo_font_catalog::manifest()?;
    let mut db = fontdb::Database::new();
    let mut canonical = HashSet::new();
    let mut reserved = HashSet::new();
    let mut required = Vec::new();
    for asset in &manifest.font {
        if asset.scope == Scope::Fixture && !fixtures {
            continue;
        }
        // Only this catalog's canonical faces reserve names. Product mode
        // can still discover an ambient Adwaita as a user-named font.
        reserved.extend(asset.reserved_aliases.iter().map(|s| s.to_lowercase()));
        let path = verify_asset(root, asset)?;
        let ids = db.load_font_source(fontdb::Source::File(path.clone()));
        if ids.len() != asset.faces.len() {
            return Err(FontCatalogError::at(
                &path,
                format!("expected {} faces, loaded {}", asset.faces.len(), ids.len()),
            ));
        }
        for expected in &asset.faces {
            let style = if expected.style == "italic" {
                fontdb::Style::Italic
            } else {
                fontdb::Style::Normal
            };
            let found = ids
                .iter()
                .filter_map(|id| db.face(*id))
                .find(|face| face.index == expected.index);
            let valid = found.is_some_and(|face| {
                face.families.iter().any(|name| name.0 == expected.family)
                    && face.post_script_name == expected.post_script_name
                    && face.style == style
            });
            if !valid {
                return Err(FontCatalogError::at(
                    &path,
                    format!(
                        "face {} must be {} / {} ({})",
                        expected.index, expected.family, expected.post_script_name, expected.style
                    ),
                ));
            }
            required.push((expected.family.clone(), style));
        }
        canonical.extend(ids);
    }
    Ok(VerifiedBundle {
        db,
        canonical,
        reserved,
        required,
    })
}

/// Check one catalog file against its pinned size and SHA-256.
#[cfg(not(windows))]
fn verify_asset(
    root: &Path,
    asset: &tilo_font_catalog::Asset,
) -> Result<PathBuf, FontCatalogError> {
    Ok(asset.verify(root)?)
}

/// Windows hashes through the platform provider (see [`windows::digest`]).
#[cfg(windows)]
fn verify_asset(
    root: &Path,
    asset: &tilo_font_catalog::Asset,
) -> Result<PathBuf, FontCatalogError> {
    let path = asset.path_in(root)?;
    let metadata = std::fs::metadata(&path).map_err(|e| FontCatalogError::at(&path, e))?;
    if !metadata.is_file() || metadata.len() != asset.bytes {
        return Err(FontCatalogError::at(
            &path,
            format!("expected regular file of {} bytes", asset.bytes),
        ));
    }
    let file = std::fs::File::open(&path).map_err(|error| FontCatalogError::at(&path, error))?;
    let actual = windows::digest(file)
        .map(hex::encode)
        .map_err(|error| FontCatalogError::at(&path, error))?;
    asset.check_digest(&path, &actual)?;
    Ok(path)
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
