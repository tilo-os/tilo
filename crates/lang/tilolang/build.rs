//! The platform's icon names, read from the toolkit's icon sources (one SVG per icon, named by its
//! file), so the language accepts exactly the icons the toolkit draws.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    const SOURCES: &str = "../../sdk/ui-core/assets/icons";
    println!("cargo::rerun-if-changed={SOURCES}");
    let mut names = std::fs::read_dir(SOURCES)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "svg"))
        .filter_map(|path| Some(path.file_stem()?.to_str()?.to_owned()))
        .collect::<Vec<_>>();
    names.sort();
    let output =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").ok_or("missing build output")?);
    std::fs::write(
        output.join("icons.rs"),
        format!("/// The platform's icon names.\npub const ICONS: &[&str] = &{names:?};\n"),
    )?;
    Ok(())
}
