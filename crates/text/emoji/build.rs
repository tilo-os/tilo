//! Deflates the checked-in translated names (`data/<locale>.txt`, every
//! locale but English) into `OUT_DIR`, and writes the table `src/lib.rs`
//! embeds. English and the Unicode structure are also deflated; each file
//! is inflated once, when a catalogue first needs it.

use std::fmt::Write as _;
use std::path::Path;

fn deflate(source: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut packed = Vec::new();
    zopfli::compress(
        zopfli::Options::default(),
        zopfli::Format::Deflate,
        source,
        &mut packed,
    )?;
    Ok(packed)
}

/// # Errors
///
/// Returns an error when `data/` or `OUT_DIR` cannot be read or written.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data = Path::new("data");
    println!("cargo:rerun-if-changed=data");
    let out = std::env::var("OUT_DIR")?;
    let out = Path::new(&out);

    for name in ["en", "emoji"] {
        let source = std::fs::read(data.join(format!("{name}.txt")))?;
        let packed = deflate(&source)?;
        std::fs::write(out.join(format!("{name}.txt.deflate")), packed)?;
    }

    let mut locales = Vec::new();
    for entry in std::fs::read_dir(data)? {
        let path = entry?.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let is_names = path.extension().is_some_and(|ext| ext == "txt")
            && !stem.is_empty()
            && stem.starts_with(|c: char| c.is_ascii_lowercase())
            && stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && stem != "en"
            && stem != "emoji";
        if is_names {
            locales.push(stem.to_owned());
        }
    }
    locales.sort();

    let mut table = String::from("&[\n");
    for locale in &locales {
        let source = data.join(format!("{locale}.txt"));
        println!("cargo:rerun-if-changed={}", source.display());
        let packed = deflate(&std::fs::read(&source)?)?;
        let name = format!("{locale}.txt.deflate");
        std::fs::write(out.join(&name), packed)?;
        writeln!(
            table,
            "    (\"{locale}\", include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{name}\"))),"
        )?;
    }
    table.push(']');
    std::fs::write(out.join("translations.rs"), table)?;
    Ok(())
}
