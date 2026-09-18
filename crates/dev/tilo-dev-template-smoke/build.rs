fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::path::PathBuf::from(
        std::env::var_os("OUT_DIR")
            .ok_or_else(|| std::io::Error::other("Cargo did not provide OUT_DIR"))?,
    );
    for (name, template, file) in [
        (
            "profile-card",
            tilo_dev::ComponentTemplate::Form,
            "profile_card.rs",
        ),
        (
            "activity-card",
            tilo_dev::ComponentTemplate::Display,
            "activity_card.rs",
        ),
        (
            "project-tree",
            tilo_dev::ComponentTemplate::Tree,
            "project_tree.rs",
        ),
        (
            "activity-list",
            tilo_dev::ComponentTemplate::List,
            "activity_list.rs",
        ),
        (
            "results-table",
            tilo_dev::ComponentTemplate::Table,
            "results_table.rs",
        ),
        (
            "article-body",
            tilo_dev::ComponentTemplate::Text,
            "article_body.rs",
        ),
    ] {
        let source = tilo_dev::render_component_source(name, template)?;
        std::fs::write(output.join(file), source)?;
    }
    println!("cargo:rerun-if-changed=../tilo-dev/src/lib.rs");
    Ok(())
}
