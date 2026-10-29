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
//!   (a board drawing todos' rows), each package under its own authority;
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
        }
    }

    /// The bundle of the compiled module `wasm`, its ids lock `ids` and the compiled modules of
    /// the packages it uses (`imports`), as a package's payload holds them: what a tilolang host
    /// is handed for each instance.
    ///
    /// # Errors
    /// The module or an import carries no metadata.
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
        }
        .checked()
    }

    /// The bundle in the payload directory `dir`.
    ///
    /// # Errors
    /// A file is missing or unreadable, or the module carries no metadata.
    pub fn read(dir: &Path) -> Result<Self, BundleError> {
        let wasm_path = dir.join(WASM_FILE);
        let wasm = read(&wasm_path)?;
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
        Self {
            wasm,
            meta,
            ids,
            imports,
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
    /// `reads` those of the packages it uses.
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
        crate::surface::surface_toml_reading(&[&self.meta], &reads).map_err(|error| {
            BundleError::Surface {
                app: self.meta.app.clone(),
                reason: error.to_string(),
            }
        })
    }

    /// Write the bundle into `dir` (created when missing): module, ids lock, imports and
    /// surface. The icon is the caller's, since it is drawn by the platform.
    ///
    /// # Errors
    /// A file cannot be written.
    pub fn write(&self, dir: &Path) -> Result<(), BundleError> {
        let write = |path: PathBuf, bytes: &[u8]| {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| BundleError::Read {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            std::fs::write(&path, bytes).map_err(|source| BundleError::Read { path, source })
        };
        write(dir.join(WASM_FILE), &self.wasm)?;
        write(dir.join(IDS_FILE), self.ids.as_bytes())?;
        write(dir.join(SURFACE_FILE), &self.surface()?)?;
        for import in &self.imports {
            write(
                dir.join(IMPORTS_DIR)
                    .join(format!("{}.wasm", import.meta.app)),
                &import.wasm,
            )?;
        }
        Ok(())
    }
}
