use std::path::PathBuf;
use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let tool = repository.join("tools/font-catalog.py");
    let source = repository.join("crates/sdk/ui-core/assets/fonts");
    // A published manifest-only crate uses installed/bundled fonts. Source
    // checkouts materialize their pinned catalog during the build, never in
    // an application's runtime or by writing into source directories.
    let root = if tool.is_file() && source.join("artifacts.json").is_file() {
        println!("cargo::rerun-if-changed={}", tool.display());
        println!("cargo::rerun-if-changed={}", source.display());
        println!("cargo::rerun-if-env-changed=TILO_FONT_ASSET_CACHE");
        println!("cargo::rerun-if-env-changed=TILO_FONT_CATALOG_ROOT");
        println!("cargo::rerun-if-env-changed=TILO_FONT_ASSETS_OFFLINE");
        println!("cargo::rerun-if-env-changed=CARGO_NET_OFFLINE");
        println!("cargo::rerun-if-env-changed=PYTHON");
        let python = std::env::var_os("PYTHON")
            .unwrap_or_else(|| if cfg!(windows) { "python" } else { "python3" }.into());
        let mut command = Command::new(&python);
        command
            .arg(&tool)
            .arg("root")
            .arg(&source)
            .args(["--scope", "fixtures"]);
        if std::env::var("CARGO_NET_OFFLINE").is_ok_and(|value| value == "true") {
            command.arg("--offline");
        }
        let output = command.output().map_err(|error| {
            format!(
                "could not launch `{}` to prepare fonts with {}: {error}; \
                 source builds require Python 3.11+ (Fedora: dnf install python3); \
                 set PYTHON to its executable if it is not on PATH",
                python.to_string_lossy(),
                tool.display()
            )
        })?;
        if !output.status.success() {
            return Err(format!(
                "font preparation failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        PathBuf::from(String::from_utf8(output.stdout)?.trim())
    } else {
        source
    };
    println!(
        "cargo::rustc-env=TILO_CHECKOUT_FONT_ROOT={}",
        root.display()
    );
    Ok(())
}
