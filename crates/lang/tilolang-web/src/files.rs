//! Packages as a page holds them: a tree of files by path, read the way the tool reads a directory
//! of package directories ([`tilolang::source`]), and compiled through the same ordered build.
//!
//! A directory with `.tl` files directly in it is a package: those files are its module, its
//! `locales/*.ftl` its catalogues, its `media/**` the files its views and scenarios name as
//! `pkg:media/…`, and its `app.ids` its ids lock. A file under `media` travels base64, since a
//! page's answer is JSON text. A directory with a `tilo-app.toml` and a `surface.toml` is a native
//! app, which a package imports as the metadata its surface publishes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use tilolang::source::{ASSETS_DIR, BuildError, BuiltPackage, IDS_FILE, PackageAsset};
use tilolang::{IdsLock, PackageMeta, PackageSource, SeededEntropy};
use tilolang_tilo::catalogue::{COMPILED_SURFACE_FILE, MANIFEST_FILE, NativeSurface};

/// The files of one directory: its own files by name, its catalogues by locale, and the files it
/// ships under `media/`.
#[derive(Default)]
struct Dir {
    files: BTreeMap<String, String>,
    locales: Vec<(String, String)>,
    assets: Vec<PackageAsset>,
}

/// Every package and native app a tree of files holds, by the directory each stands in.
pub struct Tree {
    packages: Vec<(PathBuf, PackageSource, IdsLock)>,
    natives: Vec<PackageMeta>,
}

/// The directory a package stands in, and the name its `pkg:` address gives the file, for a file
/// the package ships under `media/`: `apps/tl/music/media/first-light.wav` is
/// `apps/tl/music` and `media/first-light.wav`.
fn asset_path(path: &Path) -> Option<(PathBuf, String)> {
    let parts: Vec<&std::ffi::OsStr> = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();
    let at = parts.iter().position(|part| *part == ASSETS_DIR)?;
    let (before, shipped) = parts.split_at_checked(at)?;
    let package = before.iter().collect();
    let asset = shipped
        .iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    Some((package, asset))
}

/// A failure to read or build what a tree holds, as a page shows it: the file and the place in it
/// for each diagnostic.
#[derive(Debug, serde::Serialize)]
pub struct Failure {
    pub message: String,
    pub diagnostics: Vec<serde_json::Value>,
}

impl Failure {
    fn read(message: String) -> Self {
        Self {
            message,
            diagnostics: Vec::new(),
        }
    }
}

impl Tree {
    /// The packages and native apps among `files`, `(path, text)`, paths `/`-separated.
    ///
    /// # Errors
    /// An ids lock, a manifest or a surface that does not read.
    pub fn read(files: impl IntoIterator<Item = (String, String)>) -> Result<Self, Failure> {
        let mut dirs: BTreeMap<PathBuf, Dir> = BTreeMap::new();
        for (path, text) in files {
            let path = Path::new(path.trim_start_matches("./"));
            let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
                continue;
            };
            let name = name.to_string_lossy().into_owned();
            // A package's own file under `media/`: the page's map carries it base64, since its
            // answer is JSON text.
            if let Some((package, asset)) = asset_path(path) {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(text.trim())
                    .map_err(|error| {
                        Failure::read(format!("{}: not base64: {error}", path.display()))
                    })?;
                dirs.entry(package)
                    .or_default()
                    .assets
                    .push(PackageAsset { name: asset, bytes });
                continue;
            }
            let in_locales = parent.file_name().and_then(|n| n.to_str())
                == Some(tilolang::messages::LOCALES_DIR);
            if in_locales && let Some(locale) = name.strip_suffix(".ftl") {
                let package = parent.parent().unwrap_or_else(|| Path::new(""));
                dirs.entry(package.to_path_buf())
                    .or_default()
                    .locales
                    .push((locale.to_owned(), text));
                continue;
            }
            dirs.entry(parent.to_path_buf())
                .or_default()
                .files
                .insert(name, text);
        }
        let mut packages = Vec::new();
        let mut natives = Vec::new();
        for (dir, mut held) in dirs {
            let shown = |name: &str| dir.join(name).display().to_string();
            if let (Some(manifest), Some(surface)) = (
                held.files.get(MANIFEST_FILE),
                held.files.get(COMPILED_SURFACE_FILE),
            ) {
                let package = NativeSurface::read(manifest, surface.clone())
                    .and_then(|native| native.package())
                    .map_err(|error| Failure::read(format!("{}: {error}", shown(MANIFEST_FILE))))?;
                natives.push(package);
            }
            let sources: Vec<(String, String)> = held
                .files
                .iter()
                .filter(|(name, _)| {
                    Path::new(name).extension().and_then(|x| x.to_str())
                        == Some(tilolang::source::EXTENSION)
                })
                .map(|(name, text)| (shown(name), text.clone()))
                .collect();
            if sources.is_empty() {
                continue;
            }
            let ids = match held.files.get(IDS_FILE) {
                Some(text) => IdsLock::parse(text)
                    .map_err(|error| Failure::read(format!("{}: {error}", shown(IDS_FILE))))?,
                None => IdsLock::new(),
            };
            held.locales.sort();
            held.assets.sort_by(|a, b| a.name.cmp(&b.name));
            let source = PackageSource::new(sources)
                .with_locales(held.locales)
                .with_assets(held.assets);
            packages.push((dir, source, ids));
        }
        Ok(Self { packages, natives })
    }

    /// The package in `root` and what it uses, compiled in dependency order against what the
    /// platform and the tree's native apps publish, as `tilolang build` compiles a directory
    /// (`tilolang_cli::compile`); and that published metadata.
    ///
    /// # Errors
    /// No package stands in `root`, an import no package declares, or a package with errors.
    pub fn compile(&self, root: &str) -> Result<(Vec<BuiltPackage>, Vec<PackageMeta>), Failure> {
        let root = Path::new(root.trim_start_matches("./").trim_end_matches('/'));
        let published =
            tilolang_tilo::catalogue::published_packages_with(self.natives.iter().cloned())
                .map_err(Failure::read)?;
        let Some(first) = self.packages.iter().find(|(dir, ..)| dir == root) else {
            return Err(Failure::read(format!(
                "{}: no .tl files; a package is a directory of .tl files",
                root.display()
            )));
        };
        let mut ordered: Vec<(PathBuf, PackageSource)> = vec![(first.0.clone(), first.1.clone())];
        ordered.extend(
            self.packages
                .iter()
                .filter(|(dir, ..)| dir != root)
                .map(|(dir, source, _)| (dir.clone(), source.clone())),
        );
        let order = tilolang::source::order_of(
            &ordered,
            &tilolang::source::apps_of(&published),
            "the files given",
        )
        .map_err(Failure::read)?;
        let built = tilolang::source::compile_in_order(
            order.iter().map(|dir| {
                let (_, source, ids) = self
                    .packages
                    .iter()
                    .find(|(held, ..)| held == dir)
                    .ok_or_else(|| BuildError::Read(format!("{}: not given", dir.display())))?;
                Ok((dir.clone(), source.clone(), ids.clone()))
            }),
            &published,
            &mut SeededEntropy(1),
        )
        .map_err(|error| failure_of(&error, &self.packages))?;
        Ok((built, published))
    }
}

/// `error` as a page shows it: each diagnostic located in the file it is in.
fn failure_of(error: &BuildError, packages: &[(PathBuf, PackageSource, IdsLock)]) -> Failure {
    match error {
        BuildError::Read(message) => Failure::read(message.clone()),
        BuildError::Diagnostics { dir, diags } => {
            let source = packages
                .iter()
                .find(|(held, ..)| held == dir)
                .map(|(_, source, _)| source);
            let diagnostics = diags
                .iter()
                .map(|diag| {
                    let mut json = diag.to_json();
                    if let (Some(source), Some(object)) = (source, json.as_object_mut()) {
                        located(diag, source, object);
                    }
                    json
                })
                .collect();
            Failure {
                message: error.to_string(),
                diagnostics,
            }
        }
    }
}

/// Where `diag` stands in its file in UTF-16 code units, as a page's editor counts (`from`, `to`),
/// beside the byte offsets the compiler reports (`start`, `end`).
fn located(
    diag: &tilolang::Diagnostic,
    source: &PackageSource,
    json: &mut serde_json::Map<String, serde_json::Value>,
) {
    let text = match &diag.file {
        Some(file) => source.files.iter().find(|f| &f.path == file),
        None => source.files.first(),
    };
    let Some(text) = text.map(|f| f.text.as_str()) else {
        return;
    };
    let units = |at: usize| -> usize {
        let at = at.min(text.len());
        let at = (0..=at)
            .rev()
            .find(|i| text.is_char_boundary(*i))
            .unwrap_or(0);
        text.get(..at).unwrap_or_default().encode_utf16().count()
    };
    json.insert("from".into(), units(diag.span.start).into());
    json.insert("to".into(), units(diag.span.end).into());
}
