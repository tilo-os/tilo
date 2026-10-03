//! A thread's scoped profile: every per-user folder resolves below its root
//! while the guard lives, on that thread only.

#![allow(clippy::unwrap_used, reason = "in a test these are the assertion")]

use std::path::Path;

use tilo_paths::{AppPaths, Location, scope_profile};

fn native(location: &Location) -> &Path {
    location.native_path().unwrap()
}

#[test]
fn a_scoped_thread_keeps_its_folders_below_the_root() {
    let root = Path::new("/agents/writer/profile");
    let outside = tilo_paths::data_dir();
    {
        let _scope = scope_profile(root);
        assert_eq!(tilo_paths::home().unwrap(), root);
        for dir in [
            tilo_paths::data_dir(),
            tilo_paths::config_dir(),
            tilo_paths::cache_dir(),
            tilo_paths::state_dir(),
        ] {
            assert!(dir.unwrap().starts_with(root));
        }
        assert_eq!(
            tilo_paths::env_dir("XDG_DATA_HOME").unwrap(),
            root.join(".local/share")
        );
        let app = AppPaths::for_app("so.tilo.totp");
        assert!(native(&app.data()).starts_with(root));
        assert!(native(&app.config()).starts_with(root));
        assert!(
            app.workspace_root().is_err(),
            "no host workspace is mounted"
        );

        let elsewhere = std::thread::spawn(tilo_paths::data_dir).join().unwrap();
        assert_eq!(
            elsewhere, outside,
            "other threads keep the process's folders"
        );
    }
    assert_eq!(
        tilo_paths::data_dir(),
        outside,
        "the scope ends with its guard"
    );
}

#[test]
fn a_scoped_thread_can_look_at_the_process_folders_for_a_moment() {
    let outside = tilo_paths::data_dir();
    let _scope = scope_profile("/agents/writer/profile");
    assert_eq!(tilo_paths::unscoped(tilo_paths::data_dir), outside);
    assert!(
        tilo_paths::data_dir()
            .unwrap()
            .starts_with("/agents/writer/profile")
    );
}
