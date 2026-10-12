use super::*;
use browser_continuity::{Profile, Provider};
use std::os::unix::fs::symlink;

struct Fixture {
    _root: tempfile::TempDir,
    home: std::path::PathBuf,
    projection: std::path::PathBuf,
    profile: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("user");
        let projection = home
            .join(".local/state/tilo/browser-continuity")
            .join("00000000-0000-0000-0000-000000000001")
            .join("browser-import-tested/import/home");
        fs::create_dir_all(&projection).unwrap();
        let mut next = projection.as_path();
        while next.starts_with(&home) {
            fs::set_permissions(next, fs::Permissions::from_mode(0o700)).unwrap();
            next = next.parent().unwrap();
        }
        let profile = projection.join(PROFILES_DIRECTORY).join("a".repeat(64));
        mkdir(&projection.join(PROFILES_DIRECTORY)).unwrap();
        mkdir(&profile).unwrap();
        let plan = Plan::new(
            "setup:approved".into(),
            [7; 32],
            vec![
                Profile::new(
                    "a".repeat(64),
                    Provider::Chromium,
                    vec!["Default".into()],
                    true,
                    false,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        write_private(&projection.join(PLAN_FILE), &plan.encode().unwrap()).unwrap();
        write_private(&profile.join("Bookmarks"), b"actual selected bookmarks").unwrap();
        Self {
            _root: root,
            home,
            projection,
            profile,
        }
    }
}
fn manifest(id: &str) -> ManifestView {
    toml::from_str(&format!(
        "[package]\nid = \"{id}\"\nversion = \"1\"\n[entrypoint]\nbin = \"browser\"\n"
    ))
    .unwrap()
}

#[test]
fn browser_gui_gets_only_bounded_readonly_selected_projection() {
    let fixture = Fixture::new();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("HOME", &fixture.home);
    let opts = RunOptions {
        browser_continuity_host_path: Some(fixture.projection.clone()),
        ..RunOptions::default()
    };
    let mut argv = vec!["bwrap".into(), "/app/browser".into()];
    let guard = splice(&mut argv, &manifest("so.tilo.browser"), &opts)
        .unwrap()
        .unwrap();
    assert_eq!(
        argv,
        [
            "bwrap",
            "--ro-bind",
            guard.path().to_str().unwrap(),
            browser_continuity::PROJECTION_DIRECTORY,
            "/app/browser"
        ]
    );
    let copy = guard
        .path()
        .join(PROFILES_DIRECTORY)
        .join("a".repeat(64))
        .join("Bookmarks");
    assert_eq!(fs::read(&copy).unwrap(), b"actual selected bookmarks");
    fs::write(fixture.profile.join("Bookmarks"), b"later original change").unwrap();
    assert_eq!(fs::read(&copy).unwrap(), b"actual selected bookmarks");
    let before =
        crate::run::resolve_capabilities(&manifest("so.tilo.browser"), &RunOptions::default());
    let after = crate::run::resolve_capabilities(&manifest("so.tilo.browser"), &opts);
    assert_eq!(before.ro_paths, after.ro_paths);
    assert_eq!(before.rw_paths, after.rw_paths);
    let mut rules = Vec::new();
    crate::run::push_projection_landlock_rules(&mut rules, &opts, &manifest("so.tilo.browser"));
    assert_eq!(rules, ["--ro", browser_continuity::PROJECTION_DIRECTORY]);
}

#[test]
fn other_apps_and_browser_helpers_never_receive_profile_or_home_access() {
    let opts = RunOptions {
        browser_continuity_host_path: Some("/not/accessible".into()),
        ..RunOptions::default()
    };
    for id in [
        "so.tilo.file-explorer",
        "so.tilo.chat",
        "so.tilo.browser-engine",
    ] {
        let mut argv = vec!["/app/helper".into()];
        assert!(splice(&mut argv, &manifest(id), &opts).unwrap().is_none());
        assert_eq!(argv, ["/app/helper"]);
        let mut rules = Vec::new();
        crate::run::push_projection_landlock_rules(&mut rules, &opts, &manifest(id));
        assert!(rules.is_empty());
    }
    for mode in [
        LaunchMode::Service {
            passthrough_argv: Vec::new(),
        },
        LaunchMode::Cli {
            command: "engine".into(),
            passthrough_argv: Vec::new(),
        },
    ] {
        let helper = RunOptions {
            mode,
            ..opts.clone()
        };
        let mut argv = vec!["/app/helper".into()];
        assert!(
            splice(&mut argv, &manifest("so.tilo.browser"), &helper)
                .unwrap()
                .is_none()
        );
        assert_eq!(argv, ["/app/helper"]);
        let mut rules = Vec::new();
        crate::run::push_projection_landlock_rules(
            &mut rules,
            &helper,
            &manifest("so.tilo.browser"),
        );
        assert!(rules.is_empty());
    }
}

#[test]
fn outside_paths_extra_credentials_links_and_public_files_are_refused() {
    let fixture = Fixture::new();
    assert!(materialize(&fixture.home, &fixture.home).is_err());
    write_private(&fixture.profile.join("Cookies"), b"private credentials").unwrap();
    assert!(materialize(&fixture.projection, &fixture.home).is_err());
    fs::remove_file(fixture.profile.join("Cookies")).unwrap();
    let bookmark = fixture.profile.join("Bookmarks");
    fs::set_permissions(&bookmark, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(materialize(&fixture.projection, &fixture.home).is_err());
    fs::remove_file(&bookmark).unwrap();
    symlink("/etc/passwd", &bookmark).unwrap();
    assert!(materialize(&fixture.projection, &fixture.home).is_err());
    fs::remove_file(&bookmark).unwrap();
    write_private(&bookmark, b"bookmarks").unwrap();
    let profiles = fixture.projection.join(PROFILES_DIRECTORY);
    let moved = fixture.projection.join("moved");
    fs::rename(&profiles, &moved).unwrap();
    symlink(&moved, &profiles).unwrap();
    assert!(materialize(&fixture.projection, &fixture.home).is_err());
}

#[test]
fn ancestor_links_and_unbounded_or_malformed_plan_are_refused() {
    let fixture = Fixture::new();
    let plan = fixture.projection.join(PLAN_FILE);
    fs::write(&plan, vec![b' '; MAX_PLAN_BYTES + 1]).unwrap();
    assert!(materialize(&fixture.projection, &fixture.home).is_err());
    fs::write(&plan, b"{}").unwrap();
    assert!(materialize(&fixture.projection, &fixture.home).is_err());
    let local = fixture.home.join(".local");
    let relocated = fixture.home.join("relocated");
    fs::rename(&local, &relocated).unwrap();
    symlink(&relocated, &local).unwrap();
    assert!(held_root(&fixture.projection, &fixture.home).is_err());
}
