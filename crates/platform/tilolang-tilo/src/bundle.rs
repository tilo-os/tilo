//! A built tilolang package as the platform installs it: the payload of the app's `.tapp`.
//!
//! `tilolang build` writes one per package; the runtime binary, started by the launcher on the
//! tilolang runtime bundle, reads the one `TILO_APP_MODULE` names. Nothing in it is code the
//! platform trusts beyond the wasm sandbox:
//!
//! * `app.wasm`: the compiled module, its metadata (schema, roots, actions, views) in the
//!   `tilolang.meta` custom section;
//! * `app.ids`: the ids lock it was compiled with;
//! * `imports/<app>.json`: the metadata of each package it uses, so the runtime knows the record
//!   schemas of resources it reads from them;
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
    #[error("{path}: not package metadata: {source}")]
    BadImport {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
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
    /// The metadata of the packages this one uses, by their app ids.
    pub imports: Vec<Meta>,
}

fn read(path: &Path) -> Result<Vec<u8>, BundleError> {
    std::fs::read(path).map_err(|source| BundleError::Read {
        path: path.to_path_buf(),
        source,
    })
}

impl Bundle {
    /// A bundle of a compiled package and the metadata of the packages it uses.
    #[must_use]
    pub fn new(compiled: &tilolang::Compiled, imports: Vec<Meta>) -> Self {
        Self {
            wasm: compiled.wasm.clone(),
            meta: compiled.meta.clone(),
            ids: compiled.ids.to_text(),
            imports,
        }
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
        if let Ok(entries) = std::fs::read_dir(&imports_dir) {
            let mut paths: Vec<PathBuf> = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                .collect();
            paths.sort();
            for path in paths {
                let bytes = read(&path)?;
                let meta = serde_json::from_slice(&bytes)
                    .map_err(|source| BundleError::BadImport { path, source })?;
                imports.push(meta);
            }
        }
        Ok(Self {
            wasm,
            meta,
            ids,
            imports,
        })
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
    #[must_use]
    pub fn surface(&self) -> Vec<u8> {
        crate::surface::surface_toml_reading(&[&self.meta], &self.imports).unwrap_or_default()
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
        write(dir.join(SURFACE_FILE), &self.surface())?;
        for import in &self.imports {
            write(
                dir.join(IMPORTS_DIR).join(format!("{}.json", import.app)),
                &import.to_json_bytes(),
            )?;
        }
        Ok(())
    }
}
