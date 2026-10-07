//! Prepare the selected SDK's font catalog before packaging.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};

pub(crate) fn prepared_fonts(sdk: &Path, scope: &str) -> Result<PathBuf> {
    let source = sdk.join("crates/sdk/ui-core/assets/fonts");
    // Historical SDK revisions still carry their pinned source files.
    if !source.join("artifacts.json").is_file() {
        return Ok(source);
    }
    let python = std::env::var_os("PYTHON")
        .unwrap_or_else(|| if cfg!(windows) { "python" } else { "python3" }.into());
    let tool = sdk.join("tools/font-catalog.py");
    let modern = std::fs::read_to_string(&tool)
        .context("reading the selected SDK's font preparation tool")?
        .contains("def prepared_root(");
    let output = Command::new(python)
        .arg(tool)
        .arg(if modern { "root" } else { "verify" })
        .arg(&source)
        .args(["--scope", scope])
        .output()
        .context("preparing the SDK's pinned font artifacts")?;
    if !output.status.success() {
        bail!(
            "font preparation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    if !modern {
        return Ok(source);
    }
    let path = PathBuf::from(
        String::from_utf8(output.stdout)
            .context("font catalog path is UTF-8")?
            .trim(),
    );
    if !path.is_dir() {
        bail!("font preparation returned no catalog directory");
    }
    Ok(path)
}
