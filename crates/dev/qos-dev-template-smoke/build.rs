fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::path::PathBuf::from(
        std::env::var_os("OUT_DIR")
            .ok_or_else(|| std::io::Error::other("Cargo did not provide OUT_DIR"))?,
    );
    for (name, template, file) in [
        (
            "profile-card",
            qos_dev::ComponentTemplate::Form,
            "profile_card.rs",
        ),
        (
            "activity-card",
            qos_dev::ComponentTemplate::Display,
            "activity_card.rs",
        ),
        (
            "project-tree",
            qos_dev::ComponentTemplate::Tree,
            "project_tree.rs",
        ),
    ] {
        let source = qos_dev::render_component_source(name, template)?;
        std::fs::write(output.join(file), source)?;
    }
    println!("cargo:rerun-if-changed=../qos-dev/src/lib.rs");
    Ok(())
}
