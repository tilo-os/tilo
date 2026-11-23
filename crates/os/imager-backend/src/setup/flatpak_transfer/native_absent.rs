//! No Flatpak installation is read on this platform.
use std::path::Path;

use super::Runner;

/// No Flatpak installation is read on this platform.
pub struct NativeRunner;

impl Runner for NativeRunner {
    fn run(&self, _home: &Path, _arguments: &[String]) -> Result<String, String> {
        Err("no Flatpak installation is read on this platform".into())
    }
}
