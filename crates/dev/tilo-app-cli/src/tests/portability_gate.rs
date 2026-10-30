//! The repository's app portability gate: every app, app model crate and
//! portable SDK crate counted against the shrink-only baseline.

use super::*;

/// A workspace with one platform-specific app, its baseline written from the
/// gate's own table.
fn baselined_workspace(root: &Path) {
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"apps/*\"]\n",
    )
    .unwrap();
    let app = root.join("apps/serial");
    minimal_app(&app);
    std::fs::write(
        app.join("tilo-app.toml"),
        "[package]\nid = \"so.tilo.serial\"\nversion = \"0.1.0\"\nkind = \"app\"\n\
         platforms = [\"linux\"]\n\
         platform-reason = \"Reads the machine's serial number\"\n\n\
         [publisher]\n\
         identity = \"ed25519:0000000000000000000000000000000000000000000000000000000000000000\"\n\
         signature = \"\"\n\n\
         [sandbox]\nnamespaces = [\"user\", \"pid\", \"uts\", \"ipc\", \"mount\"]\n\
         seccomp = \"tilo-default\"\nlandlock = \"strict\"\n\n\
         [sandbox.net]\nmode = \"off\"\n\n\
         [entrypoint]\nbin = \"bin/serial\"\ntype = \"gui\"\n",
    )
    .unwrap();
    std::fs::write(
        app.join("src/lib.rs"),
        "pub fn unix() -> &'static str { std::os::unix::fs::SEPARATOR }\n",
    )
    .unwrap();
}

fn write_baseline(root: &Path, table: &str) {
    let path = root.join("tools/policy");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("app_portability_baseline.toml"), table).unwrap();
}

#[test]
fn the_portability_gate_counts_every_app_against_its_shrink_only_baseline() {
    let root = tempfile::tempdir().unwrap();
    baselined_workspace(root.path());
    let gate = portability::gate(root.path()).unwrap();

    let grown = gate.render();
    assert!(!gate.passed(), "{grown}");
    assert!(
        grown.contains(
            "app portability FAILED: platform code grew\n  apps.serial: 1 R2 finding(s); \
             a package without a baseline entry must be clean"
        ),
        "{grown}"
    );

    write_baseline(root.path(), &gate.baseline_table());
    let gate = portability::gate(root.path()).unwrap();
    assert!(gate.passed(), "{}", gate.render());
    assert!(
        gate.render()
            .starts_with("app portability OK (1 baselined app site(s) in 1 app(s); 0 in the SDK's"),
        "{}",
        gate.render()
    );

    let app = root.path().join("apps/serial");
    let source = std::fs::read_to_string(app.join("src/lib.rs")).unwrap();
    std::fs::write(
        app.join("src/lib.rs"),
        format!("{source}pub fn fd() -> std::os::fd::RawFd {{ 0 }}\n"),
    )
    .unwrap();
    let gate = portability::gate(root.path()).unwrap();
    assert!(!gate.passed());
    assert!(
        gate.render()
            .contains("apps.serial: 2 R2 finding(s), baseline allows 1"),
        "{}",
        gate.render()
    );

    std::fs::write(app.join("src/lib.rs"), "pub fn run() {}\n").unwrap();
    let gate = portability::gate(root.path()).unwrap();
    let fallen = gate.render();
    assert!(!gate.passed(), "{fallen}");
    assert!(
        fallen.contains("counts fell; lock the improvement in\n  apps.serial: R2 fell from 1 to 0"),
        "{fallen}"
    );
    assert!(
        fallen.contains("Replace tools/policy/app_portability_baseline.toml with:"),
        "{fallen}"
    );
    assert!(!fallen.contains("[apps.serial]"), "{fallen}");
}

#[test]
fn the_workspace_holds_no_more_platform_code_than_its_baseline_allows() {
    let gate = portability::gate(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    assert!(gate.passed(), "{}", gate.render());
    assert!(
        gate.render().starts_with("app portability OK ("),
        "{}",
        gate.render()
    );
    assert!(gate.apps().len() >= 50, "{} apps", gate.apps().len());
}
