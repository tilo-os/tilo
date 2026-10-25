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
        // The catalog lives in this build's OUT_DIR, which travels with the build's other
        // outputs; a path anywhere else would be compiled into the crate and missing wherever
        // the build is reused without it. The artifact cache, a prewarmed catalog
        // (TILO_FONT_CATALOG_ROOT), the network and the interpreter only decide where the
        // pinned bytes are read from, and every byte is checked against its pin, so a change
        // to them leaves the catalog as it was and doesn't run this script again.
        let out = std::env::var_os("OUT_DIR").ok_or("cargo sets OUT_DIR for build scripts")?;
        let root = PathBuf::from(out).join("fonts");
        // `prepare` publishes only into a new directory; this is the previous run's catalog.
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        let python = std::env::var_os("PYTHON").unwrap_or_else(|| python_with_tomllib().into());
        let mut command = Command::new(&python);
        command
            .arg(&tool)
            .arg("prepare")
            .arg(&source)
            .arg(&root)
            .args(["--scope", "fixtures"]);
        // Offline only when asked by TILO_FONT_ASSETS_OFFLINE, which the tool reads itself.
        // CARGO_NET_OFFLINE says cargo has its crates already; it says nothing about whether
        // the pinned fonts are in the cache, and a build with network access fetches them.
        let output = command.output().map_err(|error| {
            format!(
                "could not launch `{}` to prepare fonts with {}: {error}; \
                 source builds require Python 3.11+ (Fedora: dnf install python3, \
                 macOS: brew install python); set PYTHON to its executable if it is not on PATH",
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
        root
    } else {
        source
    };
    println!(
        "cargo::rustc-env=TILO_CHECKOUT_FONT_ROOT={}",
        root.display()
    );
    Ok(())
}

/// The first Python on this machine that the tool runs on (3.11+, which has
/// `tomllib`): `python3` on PATH, else a versioned one, else Homebrew's. A
/// Mac's own `/usr/bin/python3` is 3.9 and comes first on PATH.
fn python_with_tomllib() -> &'static str {
    const CANDIDATES: &[&str] = if cfg!(windows) {
        &["python", "py"]
    } else {
        &[
            "python3",
            "python3.14",
            "python3.13",
            "python3.12",
            "python3.11",
            "/opt/homebrew/bin/python3",
            "/usr/local/bin/python3",
        ]
    };
    CANDIDATES
        .iter()
        .copied()
        .find(|python| {
            Command::new(python)
                .args(["-c", "import tomllib"])
                .output()
                .is_ok_and(|output| output.status.success())
        })
        .unwrap_or("python3")
}
