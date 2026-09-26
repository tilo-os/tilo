use tilo_app_cli::ComponentTemplate;

/// Emit the component sources and the component registry `tilo-app add
/// component` writes into an app's `src/components/`, one component per
/// template. The display and form components are graph-compatible, so the
/// registry registers them with serialized screens as `--mount` does.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::path::PathBuf::from(
        std::env::var_os("OUT_DIR")
            .ok_or_else(|| std::io::Error::other("Cargo did not provide OUT_DIR"))?,
    )
    .join("components");
    std::fs::create_dir_all(&output)?;
    let components = [
        ("profile-card", ComponentTemplate::Form),
        ("activity-card", ComponentTemplate::Display),
        ("project-tree", ComponentTemplate::Tree),
        ("activity-list", ComponentTemplate::List),
        ("results-table", ComponentTemplate::Table),
        ("article-body", ComponentTemplate::Text),
    ];
    for (name, template) in components {
        let source = tilo_app_cli::render_component_source(name, template)?;
        std::fs::write(
            output.join(format!("{}.rs", name.replace('-', "_"))),
            source,
        )?;
    }
    let registry = tilo_app_cli::render_component_registry_source(&components)?;
    std::fs::write(output.join("mod.rs"), registry)?;
    println!("cargo:rerun-if-changed=../tilo-app-cli/src/lib.rs");
    Ok(())
}
