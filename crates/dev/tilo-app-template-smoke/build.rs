fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::path::PathBuf::from(
        std::env::var_os("OUT_DIR")
            .ok_or_else(|| std::io::Error::other("Cargo did not provide OUT_DIR"))?,
    );
    for (name, template, file) in [
        (
            "profile-card",
            tilo_app_cli::ComponentTemplate::Form,
            "profile_card.rs",
        ),
        (
            "activity-card",
            tilo_app_cli::ComponentTemplate::Display,
            "activity_card.rs",
        ),
        (
            "project-tree",
            tilo_app_cli::ComponentTemplate::Tree,
            "project_tree.rs",
        ),
        (
            "activity-list",
            tilo_app_cli::ComponentTemplate::List,
            "activity_list.rs",
        ),
        (
            "results-table",
            tilo_app_cli::ComponentTemplate::Table,
            "results_table.rs",
        ),
        (
            "article-body",
            tilo_app_cli::ComponentTemplate::Text,
            "article_body.rs",
        ),
    ] {
        let source = tilo_app_cli::render_component_source(name, template)?;
        std::fs::write(output.join(file), source)?;
    }
    println!("cargo:rerun-if-changed=../tilo-app-cli/src/lib.rs");
    Ok(())
}
