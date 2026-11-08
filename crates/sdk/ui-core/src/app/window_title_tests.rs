use super::*;

#[test]
fn window_title_and_app_id_resolve_from_the_most_specific_identity() {
    {
        assert_eq!(humanize_ident("notes"), "Notes");
        assert_eq!(humanize_ident("file-explorer"), "File Explorer");
        assert_eq!(humanize_ident("word-processor"), "Word Processor");
        assert_eq!(humanize_ident("db_client"), "Db Client");
        assert_eq!(humanize_ident("notes"), "Notes");
        assert_eq!(humanize_ident(""), "");
    }

    {
        let t = resolve_title_from(
            Some("My Doc — Notes"),
            Some("Notes"),
            Some("so.tilo.notes"),
            Some("notes"),
        );
        assert_eq!(t, "My Doc — Notes");
    }

    {
        let t = resolve_title_from(None, Some("Notes"), Some("so.tilo.notes"), Some("notes"));
        assert_eq!(t, "Notes");
        assert_ne!(t, "QuickUI App");
    }

    {
        let t = resolve_title_from(
            None,
            None,
            Some("so.tilo.file-explorer"),
            Some("file-explorer"),
        );
        assert_eq!(t, "File Explorer");
        assert_ne!(t, "QuickUI App");
    }

    {
        let t = resolve_title_from(None, None, None, Some("terminal"));
        assert_eq!(t, "Terminal");
        assert_ne!(t, "QuickUI App");
    }

    {
        let t = resolve_title_from(Some("  "), Some(""), Some("so.tilo.notes"), Some("notes"));
        assert_eq!(t, "Notes");
    }

    {
        let t = resolve_title_from(None, None, None, None);
        assert_eq!(t, "Tilo App");
        assert_ne!(t, "QuickUI App");
    }

    {
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

    {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APP_NAME", "Weather")
            .set("TILO_PACKAGE_ID", "so.tilo.weather");
        let resolved = resolve_window_title(None);
        drop(env);
        assert_eq!(resolved, "Weather");
        assert_ne!(resolved, "QuickUI App");
    }

    {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.remove("TILO_APP_NAME")
            .set("TILO_PACKAGE_ID", "so.tilo.calculator");
        let resolved = resolve_window_title(None);
        drop(env);
        assert_eq!(resolved, "Calculator");
    }
}
