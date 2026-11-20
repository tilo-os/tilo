//! A portable set of installed applications is reviewed from a Flatpak
//! installation, which only the Linux installer has.
use std::path::Path;

/// Never constructed: installed applications are reviewed only on Linux.
pub enum PortableFlatpakSource {}

/// No Flatpak installation is read on this platform.
pub struct NativeRunner;

impl super::Runner for NativeRunner {
    fn run(&self, _home: &Path, _arguments: &[String]) -> Result<String, String> {
        Err("no Flatpak installation is read on this platform".into())
    }
}
