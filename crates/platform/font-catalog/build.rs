use std::path::PathBuf;
use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    write_manifest()?;
    write_notices()?;
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let tool = repository.join("tools/font-catalog.py");
    let source = repository.join("crates/ui/ui-core/assets/fonts");
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
        for input in [
            "manifest.toml",
            "PROVENANCE.md",
            "LICENSES/UPSTREAM.md",
            "artifacts.json",
        ] {
            println!("cargo::rerun-if-changed={}", source.join(input).display());
        }
        println!("cargo::rerun-if-changed={}", tool.display());
        let root = prepared(&tool, &source, "fixtures", &catalogs)?;
        if std::env::var_os("CARGO_FEATURE_WEB").is_some() {
            write_web_cuts(&prepared(&tool, &source, "web", &catalogs)?)?;
        }
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
        if std::env::var_os("CARGO_FEATURE_WEB").is_some() {
            return Err(
                "the `web` feature embeds the page's cuts, which only a source \
                        checkout's pinned catalog prepares"
                    .into(),
            );
        }
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

/// Prepare the catalog of `scope` from `source`, published in `catalogs`, and answer its root.
fn prepared(
    tool: &std::path::Path,
    source: &std::path::Path,
    scope: &str,
    catalogs: &std::path::Path,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let python = std::env::var_os("PYTHON").unwrap_or_else(|| python_with_tomllib().into());
    let mut command = Command::new(&python);
    command
        // Isolated, as the CI helpers in `tools/ci` are run: without it Python puts the
        // script's directory on `sys.path` and lists it looking its imports up, so this
        // build would read the whole `tools/` tree — and an edit to any file in it would
        // rebuild every package below this one. The tool imports nothing of its own.
        .arg("-I")
        .arg(tool)
        .arg("root")
        .arg(source)
        .args(["--scope", scope])
        .arg("--catalogs")
        .arg(catalogs);
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
            "font preparation failed: {}\n\
             the pinned fonts come from TILO_FONT_IMAGE_SEED (the font-seed check's output) or \
             TILO_FONT_ASSET_CACHE; a machine with neither fetches them from the store, which a \
             check's sandbox has no egress for. Prepare a cache with `{} -I {} root {} --scope \
             {scope} --cache <directory>` and give the build it as TILO_FONT_ASSET_CACHE",
            String::from_utf8_lossy(&output.stderr),
            python.to_string_lossy(),
            tool.display(),
            source.display(),
        )
        .into());
    }
    // The tool named the catalog it verified; the directory is the one this script asked it
    // to publish in, so the path stays inside this build's own view of the target directory.
    let published = PathBuf::from(std::str::from_utf8(&output.stdout)?.trim());
    let name = published
        .file_name()
        .ok_or("the tool names the catalog it prepared")?;
    Ok(catalogs.join(name))
}

/// The page's cuts as a Rust table (`OUT_DIR/web.rs`, which [`web::CUTS`] reads): each file's
/// name and its bytes, from the verified web catalog at `root`.
fn write_web_cuts(root: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::fmt::Write as _;
    use toml::Value;
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("manifest.toml");
    let table: toml::Table = std::fs::read_to_string(&path)?.parse()?;
    let mut out = String::from("&[\n");
    for font in table
        .get("font")
        .and_then(Value::as_array)
        .ok_or("manifest.toml: no fonts")?
        .iter()
        .filter(|font| font.get("scope").and_then(Value::as_str) == Some("web"))
    {
        let file = font
            .get("file")
            .and_then(Value::as_str)
            .ok_or("manifest.toml: a font has no `file`")?;
        let bytes = root.join(file);
        println!("cargo::rerun-if-changed={}", bytes.display());
        writeln!(
            out,
            "({file:?}, include_bytes!({:?})),",
            bytes.display().to_string()
        )?;
    }
    out += "]\n";
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("cargo sets OUT_DIR")?);
    std::fs::write(out_dir.join("web.rs"), out)?;
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
            Some("web") => "Web",
            other => return Err(format!("manifest.toml: unknown scope {other:?}").into()),
        };
        let mut faces = Vec::new();
        for face in font
            .get("faces")
            .and_then(Value::as_array)
            .ok_or("manifest.toml: no faces")?
        {
            faces.push(format!(
                "({}, {}, {}, {}, {}, {})",
                field(face, "index")?,
                field(face, "family")?,
                field(face, "post_script_name")?,
                field(face, "style")?,
                face.get("weight")
                    .and_then(Value::as_integer)
                    .unwrap_or(400),
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

/// The notices every prepared catalog carries beside its fonts, as a Rust table
/// (`OUT_DIR/notices.rs`, which `prepare::NOTICES` reads): each file's path in the catalog, the
/// SHA-256 the manifest pins it to (the provenance document and the upstream note are pinned by
/// being this crate's own copies), and its bytes, from this crate's `catalog/` directory.
fn write_notices() -> Result<(), Box<dyn std::error::Error>> {
    use std::fmt::Write as _;
    use toml::Value;
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let table: toml::Table = std::fs::read_to_string(crate_dir.join("manifest.toml"))?.parse()?;
    let mut notices: Vec<(String, Option<String>)> = table
        .get("license")
        .and_then(Value::as_array)
        .ok_or("manifest.toml: no licenses")?
        .iter()
        .map(|license| {
            let text = |key: &str| {
                license
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| format!("manifest.toml: a license has no `{key}`"))
            };
            Ok((text("file")?, Some(text("sha256")?)))
        })
        .collect::<Result<_, String>>()?;
    notices.push(("PROVENANCE.md".to_owned(), None));
    notices.push(("LICENSES/UPSTREAM.md".to_owned(), None));
    let mut out = String::from("&[\n");
    for (file, sha256) in notices {
        let path = crate_dir.join("catalog").join(&file);
        println!("cargo::rerun-if-changed={}", path.display());
        if !path.is_file() {
            return Err(format!("{} is missing: the catalog's notices", path.display()).into());
        }
        writeln!(
            out,
            "({file:?}, {sha256:?}, include_bytes!({:?})),",
            path.display().to_string()
        )?;
    }
    out += "]\n";
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("cargo sets OUT_DIR")?);
    std::fs::write(out_dir.join("notices.rs"), out)?;
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
