use super::*;

#[test]
fn breadcrumb_home_is_the_granted_user_home_not_the_process_home() {
    let granted = tempfile::tempdir().expect("tempdir");
    let granted_path = granted.path().to_path_buf();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_USER_HOME", &granted_path);

    let resolved = crate::model::user_home();
    let widget_home = FileExplorerWidget::new()
        .state()
        .home_path()
        .map(std::path::Path::to_path_buf);

    drop(env);

    assert_eq!(
        resolved.as_deref(),
        Some(granted_path.as_path()),
        "the granted home wins over the process's own HOME",
    );
    assert_ne!(
        tilo_paths::home().as_deref(),
        Some(granted_path.as_path()),
        "this test only means something while the two disagree",
    );
    assert_eq!(
        widget_home.as_deref(),
        Some(granted_path.as_path()),
        "the widget measures its breadcrumb against the home it opens on",
    );

    let deep = granted_path.join("Documents").join("Investors");
    let labels: Vec<String> =
        crate::breadcrumbs::breadcrumb_components(&deep, widget_home.as_deref(), &[])
            .into_iter()
            .map(|(label, _)| label)
            .collect();
    assert_eq!(
        labels,
        vec!["Home", "Documents", "Investors"],
        "the trail is rooted at Home, not at /",
    );

    assert_eq!(
        crate::model::format_path(&granted_path.join("Documents"), widget_home.as_deref()),
        "~/Documents",
    );
    assert_eq!(
        crate::model::format_path(&granted_path, widget_home.as_deref()),
        "~",
    );
}
