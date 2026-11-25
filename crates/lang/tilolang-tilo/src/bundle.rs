//! A built tilolang package as the platform installs it: the payload of the app's `.tapp`.
//!
//! `tilolang build` writes one per package; the runtime binary, started by the launcher on the
//! tilolang runtime bundle, reads the one `TILO_APP_MODULE` names. Nothing in it is code the
//! platform trusts beyond the wasm sandbox:
//!
//! * `app.wasm`: the compiled module, its metadata (schema, roots, actions, views) in the
//!   `tilolang.meta` custom section;
//! * `app.ids`: the ids lock it was compiled with;
//! * `imports/<app>.wasm`: the compiled module of each package it uses, its metadata embedded
//!   the same way, so the runtime runs their views and actions on the resources it holds of them
//!   (a board drawing todos' rows), each package under its own authority. The standard library
//!   is not among them: the runtime the package names carries it (`lib/tilo.std.wasm`);
//! * `surface.toml`: the surface generated from the metadata, which receivers decide the app's
//!   changes against;
//! * `icon.svg`: the app's icon, when it declares one.

use std::path::{Path, PathBuf};

use tilolang_runtime::meta::Meta;

/// The variable the launcher names the payload directory with.
pub const MODULE_ENV: &str = "TILO_APP_MODULE";
pub const WASM_FILE: &str = "app.wasm";
pub const IDS_FILE: &str = "app.ids";
pub const SURFACE_FILE: &str = "surface.toml";
pub const ICON_FILE: &str = "icon.svg";
pub const IMPORTS_DIR: &str = "imports";

/// A file the package ships beside its sources, under `media/`, which a view or a scenario names
/// as `pkg:<name>`: a picture, a short sound, a piece of data. `name` is its path from the
/// package's directory (`media/first-light.wav`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub bytes: Vec<u8>,
    /// Only a scenario names it: an install leaves it out, since an install starts empty and
    /// stages no scenario.
    pub scenario_only: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("{path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}: not a tilolang module (no tilolang.meta section)")]
    NotAModule(PathBuf),
    #[error("{0}: not a tilolang module (no tilolang.meta section)")]
    BadImport(PathBuf),
    #[error("{app}: the package names types the platform cannot read: {types}")]
    UnreadableTypes { app: String, types: String },
    #[error("{app}: its surface could not be written: {reason}")]
    Surface { app: String, reason: String },
    #[error("{MODULE_ENV} is not set: the tilolang runtime runs the package the launcher names")]
    Unset,
}

/// One built package.
#[derive(Clone, Debug)]
pub struct Bundle {
    pub wasm: Vec<u8>,
    pub meta: Meta,
    /// The ids lock, as text.
    pub ids: String,
    /// The packages this one uses, each before those that use it.
    pub imports: Vec<Import>,
    /// The files the package ships under `media/`, which its views and its scenarios name as
    /// `pkg:<name>`.
    pub assets: Vec<Asset>,
}

/// A package another one uses: its compiled module and the metadata the module embeds.
#[derive(Clone, Debug)]
pub struct Import {
    pub wasm: Vec<u8>,
    pub meta: Meta,
}

impl Import {
    /// The package compiled as `compiled`.
    #[must_use]
    pub fn new(compiled: &tilolang::Compiled) -> Self {
        Self {
            wasm: compiled.wasm.clone(),
            meta: compiled.meta.clone(),
        }
    }

    /// The package whose compiled module is `wasm`, or `None` when it embeds no metadata.
    #[must_use]
    pub fn from_wasm(wasm: Vec<u8>) -> Option<Self> {
        let meta = Meta::from_wasm(&wasm)?;
        Some(Self { wasm, meta })
    }
}

fn read(path: &Path) -> Result<Vec<u8>, BundleError> {
    std::fs::read(path).map_err(|source| BundleError::Read {
        path: path.to_path_buf(),
        source,
    })
}

/// The bundle of each compiled package, in the order given: each carries the earlier packages of
/// the build it uses. The standard library is no package's to carry: the runtime keyed by the
/// library a package records ([`tilolang::PackageMeta::std`]) brings it.
#[must_use]
pub fn of_build(built: &[tilolang::source::BuiltPackage]) -> Vec<Bundle> {
    built
        .iter()
        .enumerate()
        .map(|(index, package)| {
            let used = &package.compiled.meta.uses;
            let imports = built
                .iter()
                .take(index)
                .map(|earlier| &earlier.compiled)
                .filter(|earlier| used.contains(&earlier.meta.app));
            let scenario_only = tilolang::source::scenario_only_assets(&package.source);
            let assets = package.source.assets.iter().map(|asset| Asset {
                name: asset.name.clone(),
                bytes: asset.bytes.clone(),
                scenario_only: scenario_only.contains(&asset.name),
            });
            Bundle::new(&package.compiled, imports).with_assets(assets)
        })
        .collect()
}

/// The bundles of the package in `dir` and the packages it uses, in compilation order: the last is
/// `dir`'s own. What a host that carries packages as assets packs, without writing ids locks.
///
/// # Errors
///
/// A package does not compile (the message lists the errors by file).
pub fn compiled(dir: &Path, search: &[PathBuf]) -> Result<Vec<Bundle>, String> {
    let (built, _) = crate::catalogue::compile(dir, search, &mut tilolang::OsEntropy::default())
        .map_err(|error| error.to_string())?;
    Ok(of_build(&built))
}

impl Bundle {
    /// A bundle of a compiled package and the packages it uses, each before those that use it.
    #[must_use]
    pub fn new<'a>(
        compiled: &tilolang::Compiled,
        imports: impl IntoIterator<Item = &'a tilolang::Compiled>,
    ) -> Self {
        Self {
            wasm: compiled.wasm.clone(),
            meta: compiled.meta.clone(),
            ids: compiled.ids.to_text(),
            imports: imports.into_iter().map(Import::new).collect(),
            assets: Vec::new(),
        }
    }

    /// The bundle carrying `assets`, as a package's directory ships them.
    #[must_use]
    pub fn with_assets(mut self, assets: impl IntoIterator<Item = Asset>) -> Self {
        self.assets = assets.into_iter().collect();
        self
    }

    /// The bundle of the compiled module `wasm`, its ids lock `ids` and the compiled modules of
    /// the packages it uses (`imports`), as a package's payload holds them: what a tilolang host
    /// is handed for each instance.
    pub fn from_parts(
        wasm: Vec<u8>,
        ids: String,
        imports: &[Vec<u8>],
    ) -> Result<Self, BundleError> {
        let meta = Meta::from_wasm(&wasm)
            .ok_or_else(|| BundleError::NotAModule(PathBuf::from(WASM_FILE)))?;
        let imports = imports
            .iter()
            .enumerate()
            .map(|(index, bytes)| {
                Import::from_wasm(bytes.clone()).ok_or_else(|| {
                    BundleError::BadImport(PathBuf::from(IMPORTS_DIR).join(format!("{index}.wasm")))
                })
            })
            .collect::<Result<Vec<Import>, _>>()?;
        Self {
            wasm,
            meta,
            ids,
            imports,
            assets: Vec::new(),
        }
        .checked()
    }

    /// The languages the package ships, as `(locale, Fluent text)`, the source language first:
    /// what its window chooses its one language among.
    #[must_use]
    pub fn catalogues(&self) -> Vec<(String, String)> {
        tilolang::meta::locales_from_wasm(&self.wasm)
    }

    /// The bundle in the payload directory `dir`.
    pub fn read(dir: &Path) -> Result<Self, BundleError> {
        let wasm_path = dir.join(WASM_FILE);
        let wasm = match read(&wasm_path) {
            // A payload that holds no module is a package of another kind — a Rust app's, which
            // ships its code in `bin/` — not a broken one: it has no views of its own.
            Err(BundleError::Read { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                return Err(BundleError::NotAModule(wasm_path));
            }
            other => other?,
        };
        let meta = Meta::from_wasm(&wasm).ok_or(BundleError::NotAModule(wasm_path))?;
        let ids = match std::fs::read_to_string(dir.join(IDS_FILE)) {
            Ok(ids) => ids,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(source) => {
                return Err(BundleError::Read {
                    path: dir.join(IDS_FILE),
                    source,
                });
            }
        };
        let mut imports = Vec::new();
        let imports_dir = dir.join(IMPORTS_DIR);
        let listed = |source| BundleError::Read {
            path: imports_dir.clone(),
            source,
        };
        match std::fs::read_dir(&imports_dir) {
            // A package that uses no other has no imports directory.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(listed(source)),
            Ok(entries) => {
                let mut paths = Vec::new();
                for entry in entries {
                    let path = entry.map_err(listed)?.path();
                    if path.extension().is_some_and(|ext| ext == "wasm") {
                        paths.push(path);
                    }
                }
                paths.sort();
                for path in paths {
                    let bytes = read(&path)?;
                    imports.push(Import::from_wasm(bytes).ok_or(BundleError::BadImport(path))?);
                }
            }
        }
        let assets = tilolang::source::read_assets(dir)
            .map_err(|message| BundleError::Read {
                path: dir.join("media"),
                source: std::io::Error::other(message),
            })?
            .into_iter()
            .map(|asset| Asset {
                name: asset.name,
                bytes: asset.bytes,
                scenario_only: false,
            })
            .collect();
        Self {
            wasm,
            meta,
            ids,
            imports,
            assets,
        }
        .checked()
    }

    /// The bundle, once every type its packages' metadata names parses
    /// ([`crate::meta::unreadable_types`]): what is derived from it later reads them all.
    fn checked(self) -> Result<Self, BundleError> {
        for meta in self
            .imports
            .iter()
            .map(|import| &import.meta)
            .chain([&self.meta])
        {
            let unreadable = crate::meta::unreadable_types(meta);
            if !unreadable.is_empty() {
                return Err(BundleError::UnreadableTypes {
                    app: meta.app.clone(),
                    types: unreadable.join("; "),
                });
            }
        }
        Ok(self)
    }

    /// The bundle the launcher named in [`MODULE_ENV`].
    ///
    /// # Errors
    /// The variable is unset, or [`Self::read`] fails.
    pub fn from_env() -> Result<Self, BundleError> {
        let dir = std::env::var_os(MODULE_ENV)
            .filter(|dir| !dir.is_empty())
            .ok_or(BundleError::Unset)?;
        Self::read(Path::new(&dir))
    }

    /// The `surface.toml` bytes generated from this package's metadata: its resources, and as
    /// `reads` those of the packages it uses. The bytes are part of the package's digest, so they
    /// say each name in its source words, never in the language of whoever built it.
    ///
    /// # Errors
    /// The surface could not be written out: never an empty surface in its place, which would
    /// have receivers decide the package's changes against nothing.
    pub fn surface(&self) -> Result<Vec<u8>, BundleError> {
        let reads: Vec<Meta> = self
            .imports
            .iter()
            .map(|import| import.meta.clone())
            .collect();
        crate::surface::surface_toml_reading(&[&self.meta], &reads, &tilolang_runtime::SourceWords)
            .map_err(|error| BundleError::Surface {
                app: self.meta.app.clone(),
                reason: error.to_string(),
            })
    }

    /// The surface of each package this one uses, by its app id, as [`Self::surface`] writes this
    /// one's: the records it keeps of them (a Tube session's Playback) are decided against their
    /// own types, which say when one of them is live.
    ///
    /// # Errors
    /// A surface could not be written out.
    pub fn import_surfaces(&self) -> Result<Vec<(String, Vec<u8>)>, BundleError> {
        self.imports
            .iter()
            .map(|import| {
                crate::surface::surface_toml_reading(
                    &[&import.meta],
                    &[],
                    &tilolang_runtime::SourceWords,
                )
                .map(|surface| (import.meta.app.clone(), surface))
                .map_err(|error| BundleError::Surface {
                    app: import.meta.app.clone(),
                    reason: error.to_string(),
                })
            })
            .collect()
    }

    /// Write the bundle into `dir` (created when missing): module, ids lock, imports, surface and
    /// every file under `media/` the package ships, scenario-only ones included. The icon is the
    /// caller's, since it is drawn by the platform. A development run writes this: the window it
    /// opens stages the app's scenarios, and they show what only they name.
    pub fn write(&self, dir: &Path) -> Result<(), BundleError> {
        self.write_payload(dir)?;
        for asset in &self.assets {
            write_file(&dir.join(&asset.name), &asset.bytes)?;
        }
        Ok(())
    }

    /// Write the bundle into `dir` as a person's install of it carries it: the same payload, but
    /// without the files only its scenarios name, which an install never shows.
    pub fn write_installed(&self, dir: &Path) -> Result<(), BundleError> {
        self.write_payload(dir)?;
        for asset in self.assets.iter().filter(|asset| !asset.scenario_only) {
            write_file(&dir.join(&asset.name), &asset.bytes)?;
        }
        Ok(())
    }

    fn write_payload(&self, dir: &Path) -> Result<(), BundleError> {
        write_file(&dir.join(WASM_FILE), &self.wasm)?;
        write_file(&dir.join(IDS_FILE), self.ids.as_bytes())?;
        write_file(&dir.join(SURFACE_FILE), &self.surface()?)?;
        for import in &self.imports {
            write_file(
                &dir.join(IMPORTS_DIR)
                    .join(format!("{}.wasm", import.meta.app)),
                &import.wasm,
            )?;
        }
        Ok(())
    }
}

/// Write `bytes` to `path`, making its directory when it is missing.
fn write_file(path: &Path, bytes: &[u8]) -> Result<(), BundleError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| BundleError::Read {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(path, bytes).map_err(|source| BundleError::Read {
        path: path.to_path_buf(),
        source,
    })
}
