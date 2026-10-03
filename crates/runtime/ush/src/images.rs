//! Turning image ids into read-only trees.
//!
//! An image is a tapp package of kind `env-runtime` or `tool`, materialised
//! by the package store as a plain read-only directory tree whose `usr/` and
//! `etc/` become the guest's. The supervisor needs no privilege to use one:
//! `bwrap` stacks the trees as overlay lowers inside the guest's own user
//! namespace.

use std::path::PathBuf;

use tilo_environment::PackageId;

/// Where images come from.
pub trait Images: Send + Sync {
    /// The read-only root of an image, fetching and materialising it first
    /// when it is not on this device.
    ///
    /// # Errors
    /// Returns why the image is unavailable.
    fn tree(&self, id: &PackageId) -> Result<PathBuf, String>;

    /// The `env-runtime` a new environment starts on: the newest one the
    /// system store ships.
    ///
    /// # Errors
    /// Returns why there is none.
    fn current_runtime(&self) -> Result<PackageId, String>;
}

/// Images kept as `<root>/<hex id>/` trees, with the current runtime's id in
/// `<root>/current-runtime`.
#[derive(Debug, Clone)]
pub struct DirImages {
    root: PathBuf,
}

impl DirImages {
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl Images for DirImages {
    fn tree(&self, id: &PackageId) -> Result<PathBuf, String> {
        let tree = self.root.join(id.to_hex());
        if tree.is_dir() {
            Ok(tree)
        } else {
            Err(format!("image {id} is not on this device"))
        }
    }

    fn current_runtime(&self) -> Result<PackageId, String> {
        let raw = std::fs::read_to_string(self.root.join("current-runtime"))
            .map_err(|e| format!("no env-runtime is installed: {e}"))?;
        tilo_environment::parse_image(raw.trim())
    }
}
