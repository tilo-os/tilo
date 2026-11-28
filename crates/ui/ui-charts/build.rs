//! The set of described charts this crate declares, generated from where each is declared
//! (`tilo-component-scan`): a chart is in it because its source says `impl Described`, never
//! because a list names it.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let scan = tilo_component_scan::scan(std::path::Path::new("src"))?;
    // Each source the scan read, not the directory: an edit to a test does not rebuild the crate.
    for file in &scan.files {
        println!("cargo::rerun-if-changed={}", file.display());
    }
    let output =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").ok_or("missing build output")?);
    std::fs::write(
        output.join("component_registry.rs"),
        tilo_component_scan::registry_source(
            &scan,
            "::tilo_ui_base",
            tilo_component_scan::Exports::Root,
        ),
    )?;
    Ok(())
}
