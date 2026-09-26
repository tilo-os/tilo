use super::*;

#[test]
fn humanize_ident_titlecases_and_splits() {
    assert_eq!(humanize_ident("notes"), "Notes");
    assert_eq!(humanize_ident("file-explorer"), "File Explorer");
    assert_eq!(humanize_ident("word-processor"), "Word Processor");
    assert_eq!(humanize_ident("db_client"), "Db Client");
    assert_eq!(humanize_ident("notes"), "Notes");
    assert_eq!(humanize_ident(""), "");
}

#[test]
fn explicit_title_wins_over_everything() {
    let t = resolve_title_from(
        Some("My Doc — Notes"),
        Some("Notes"),
        Some("so.tilo.notes"),
        Some("notes"),
    );
    assert_eq!(t, "My Doc — Notes");
}

#[test]
fn app_name_env_resolves_to_the_manifest_name() {
    let t = resolve_title_from(None, Some("Notes"), Some("so.tilo.notes"), Some("notes"));
    assert_eq!(t, "Notes");
    assert_ne!(t, "QuickUI App");
}

#[test]
fn package_id_falls_back_to_humanized_last_segment() {
    let t = resolve_title_from(
        None,
        None,
        Some("so.tilo.file-explorer"),
        Some("file-explorer"),
    );
    assert_eq!(t, "File Explorer");
    assert_ne!(t, "QuickUI App");
}

#[test]
fn binary_name_is_the_dev_run_fallback() {
    let t = resolve_title_from(None, None, None, Some("terminal"));
    assert_eq!(t, "Terminal");
    assert_ne!(t, "QuickUI App");
}

#[test]
fn blank_candidates_are_skipped_not_used() {
    let t = resolve_title_from(Some("  "), Some(""), Some("so.tilo.notes"), Some("notes"));
    assert_eq!(t, "Notes");
}

#[test]
fn generic_default_is_tilo_app_never_quickui_app() {
    let t = resolve_title_from(None, None, None, None);
    assert_eq!(t, "Tilo App");
    assert_ne!(t, "QuickUI App");
}

#[test]
fn the_app_id_is_never_the_generic_one_when_anything_identifies_the_app() {
    assert_eq!(
        resolve_app_id(Some("so.tilo.file-explorer"), Some("file-explorer")),
        "so.tilo.file-explorer",
        "the package id is the identity everything else is keyed on"
    );
    assert_eq!(
        resolve_app_id(None, Some("file-explorer")),
        "file-explorer",
        "outside a sandbox the binary still names this app, and it \
             matches the trailing-component convention package ids use"
    );
    assert_eq!(
        resolve_app_id(Some("   "), Some("terminal")),
        "terminal",
        "a blank package id is no package id"
    );
    assert_eq!(
        resolve_app_id(None, None),
        "org.tilo.app",
        "the generic id survives only when there is nothing to say"
    );
    assert_ne!(resolve_app_id(None, Some("file-explorer")), "org.tilo.app");
}

#[test]
fn env_path_end_to_end_prefers_app_name() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_APP_NAME", "Weather")
        .set("TILO_PACKAGE_ID", "so.tilo.weather");
    let resolved = resolve_window_title(None);
    drop(env);
    assert_eq!(resolved, "Weather");
    assert_ne!(resolved, "QuickUI App");
}

#[test]
fn env_path_falls_back_to_package_id_without_app_name() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove("TILO_APP_NAME")
        .set("TILO_PACKAGE_ID", "so.tilo.calculator");
    let resolved = resolve_window_title(None);
    drop(env);
    assert_eq!(resolved, "Calculator");
}
