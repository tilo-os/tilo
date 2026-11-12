use std::path::PathBuf;
use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    write_manifest()?;
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let tool = repository.join("tools/font-catalog.py");
    let source = repository.join("crates/sdk/ui-core/assets/fonts");
    let profile =
        profile_dir().ok_or("cargo sets OUT_DIR to <target>/<profile>/build/<unit>/out")?;
    // A published manifest-only crate uses installed/bundled fonts. Source
    // checkouts materialize their pinned catalog during the build, never in
    // an application's runtime or by writing into source directories.
    let root = if tool.is_file() && source.join("artifacts.json").is_file() {
        // The catalog is published once, named after the manifest it was prepared from, in the
        // profile directory's `font-catalogs`: beside the running program, so `checkout_root`
        // finds it above it, and outside `build/<unit>/out`, which cargo fills again whenever it
        // runs this script — the tree a concurrent test was reading used to be deleted with it.
        // Publishing is a rename of a verified tree, so a reader finds the catalog whole or not
        // at all, and a changed pin publishes a new name instead of replacing one in place.
        let catalogs = profile.join("font-catalogs");
        // What the catalog is prepared from: its pins, its notices and the artifact list. A
        // change to any of them publishes a catalog under a new name; a change to the artifact
        // cache, a prewarmed catalog (TILO_FONT_CATALOG_ROOT), the network or the interpreter
        // only decides where the pinned bytes are read from, and every byte is checked against
        // its pin, so it leaves the catalog as it was and doesn't run this script again.
        for prepared in [
            "manifest.toml",
            "PROVENANCE.md",
            "LICENSES/UPSTREAM.md",
            "artifacts.json",
        ] {
            println!(
                "cargo::rerun-if-changed={}",
                source.join(prepared).display()
            );
        }
        println!("cargo::rerun-if-changed={}", tool.display());
        let python = std::env::var_os("PYTHON").unwrap_or_else(|| python_with_tomllib().into());
        let mut command = Command::new(&python);
        command
            .arg(&tool)
            .arg("root")
            .arg(&source)
            .args(["--scope", "fixtures"])
            .arg("--catalogs")
            .arg(&catalogs);
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
        // The tool named the catalog it verified; the directory is the one this script asked it
        // to publish in, so the path stays inside this build's own view of the target directory.
        let published = PathBuf::from(std::str::from_utf8(&output.stdout)?.trim());
        let name = published
            .file_name()
            .ok_or("the tool names the catalog it prepared")?;
        let root = catalogs.join(name);
        // A build directory can hold the script's fingerprint without the tree it wrote: one
        // carried from another machine, or one whose profile directory was cleaned. Such a build
        // would otherwise stay fresh forever with no catalog compiled in, and every binary it
        // made would set text in the host's faces. Watching the catalog's manifest makes a build
        // that lost it run this script again (`cargo::rerun-if-changed` names a path that is not
        // there, which cargo reads as changed), and a published catalog is never rewritten, so a
        // build that has one runs nothing.
        println!(
            "cargo::rerun-if-changed={}",
            root.join("manifest.toml").display()
        );
        root
    } else {
        source
    };
    // A catalog is named from the profile directory up (`font-catalogs/<manifest>`), which
    // `checkout_root` finds above the running program: the target directory's own path is no
    // input of the crate, so the same sources name the same catalog wherever the build lands.
    let named = root.strip_prefix(&profile).unwrap_or(&root);
    println!(
        "cargo::rustc-env=TILO_CHECKOUT_FONT_ROOT={}",
        named.display()
    );
    Ok(())
}

/// The profile directory `OUT_DIR` sits under, `build/<unit>/out` below it: what the running
/// program's own directory sits under too, so a catalog named from it is found above the program.
fn profile_dir() -> Option<PathBuf> {
    let out = std::env::var_os("OUT_DIR").map(PathBuf::from)?;
    Some(out.parent()?.parent()?.parent()?.to_path_buf())
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
