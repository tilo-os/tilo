use std::path::PathBuf;
use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    write_manifest()?;
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
        // The catalog is this script's own output, and a build directory can hold the script's
        // fingerprint without the tree it wrote: a build carried from another machine, or one
        // whose outputs were cached before the catalog was there. Such a build would otherwise
        // stay fresh forever with no catalog compiled in, and every binary it made would set
        // text in the host's faces. Watching the catalog's manifest makes a build that lost it
        // run this script again (`cargo::rerun-if-changed` names a path that is not there, which
        // cargo reads as changed), and the manifest of a catalog that is there keeps its mtime,
        // so a build that has one runs nothing.
        println!(
            "cargo::rerun-if-changed={}",
            root.join("manifest.toml").display()
        );
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
    // A catalog in OUT_DIR is named from the profile directory up (`build/<unit>/out/fonts`),
    // which `checkout_root` finds above the running program: the build directory's own
    // path is no input of the crate, so the same sources build the same bytes wherever the
    // build directory is.
    let out = std::env::var_os("OUT_DIR").map(PathBuf::from);
    let named = out
        .as_deref()
        .and_then(|out| out.parent()?.parent()?.parent())
        .and_then(|profile| root.strip_prefix(profile).ok())
        .unwrap_or(&root);
    println!(
        "cargo::rustc-env=TILO_CHECKOUT_FONT_ROOT={}",
        named.display()
    );
    Ok(())
}

/// The manifest as a Rust table (`OUT_DIR/manifest.rs`, which `manifest()` reads), so
/// no program parses TOML for it: a browser's start module carried a TOML
/// parser for this alone. A manifest of another schema, or missing a field,
/// fails the build.
fn write_manifest() -> Result<(), Box<dyn std::error::Error>> {
    use std::fmt::Write as _;
    use toml::Value;
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("manifest.toml");
    println!("cargo::rerun-if-changed={}", path.display());
    let table: toml::Table = std::fs::read_to_string(&path)?.parse()?;
    if table.get("schema").and_then(Value::as_integer) != Some(1) {
        return Err("manifest.toml: unsupported catalog schema".into());
    }
    // A string as a Rust literal, a number as itself.
    let field = |of: &Value, key: &str| -> Result<String, String> {
        match of.get(key) {
            Some(Value::String(text)) => Ok(format!("{text:?}")),
            Some(Value::Integer(number)) => Ok(number.to_string()),
            _ => Err(format!("manifest.toml: a font has no `{key}`")),
        }
    };
    let names = |of: &Value, key: &str| -> String {
        let names = of
            .get(key)
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        let names: Vec<String> = names
            .iter()
            .filter_map(Value::as_str)
            .map(|name| format!("{name:?}"))
            .collect();
        format!("&[{}]", names.join(", "))
    };
    let mut out = String::from("&[\n");
    for font in table
        .get("font")
        .and_then(Value::as_array)
        .ok_or("manifest.toml: no fonts")?
    {
        let scope = match font.get("scope").and_then(Value::as_str) {
            Some("product") => "Product",
            Some("os") => "Os",
            Some("fixture") => "Fixture",
            other => return Err(format!("manifest.toml: unknown scope {other:?}").into()),
        };
        let mut faces = Vec::new();
        for face in font
            .get("faces")
            .and_then(Value::as_array)
            .ok_or("manifest.toml: no faces")?
        {
            faces.push(format!(
                "({}, {}, {}, {}, {})",
                field(face, "index")?,
                field(face, "family")?,
                field(face, "post_script_name")?,
                field(face, "style")?,
                names(face, "tilo_families"),
            ));
        }
        writeln!(
            out,
            "(Scope::{scope}, {}, {}, {}, {}, {}, &[{}]),",
            font.get("browser_startup")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            field(font, "file")?,
            field(font, "bytes")?,
            field(font, "sha256")?,
            names(font, "reserved_aliases"),
            faces.join(", "),
        )?;
    }
    out += "]\n";
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("cargo sets OUT_DIR")?);
    std::fs::write(out_dir.join("manifest.rs"), out)?;
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
