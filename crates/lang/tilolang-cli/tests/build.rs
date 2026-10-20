//! `tilolang build`: every package of a build is a signed Tilo app of its own.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use std::path::Path;

use tilo_app_package::manifest::Manifest;
use tilolang_cli::package::{self, BuildOptions};

fn packages() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl")
}

#[test]
fn each_package_is_a_signed_app_that_runs_on_the_tilolang_runtime() {
    let scratch = tempfile::tempdir().unwrap();
    // The packages are copied: a build writes new ids into their locks.
    let source = scratch.path().join("tl");
    for name in ["board", "notes", "todo", "chat"] {
        let dir = source.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for entry in std::fs::read_dir(packages().join(name)).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), dir.join(entry.file_name())).unwrap();
        }
    }
    let identity = tilolang_cli::identity_at(&scratch.path().join("publisher.json")).unwrap();
    let options = BuildOptions {
        dir: source.join("board"),
        search: Vec::new(),
        out: scratch.path().join("out"),
        tilo_runtime: "tilo-runtime/2026.07".into(),
        tilolang_runtime: package::DEFAULT_TILOLANG_RUNTIME.into(),
        version: "0.1.0".into(),
    };
    let apps = package::build(&options, &identity).unwrap();
    let ids: Vec<&str> = apps.iter().map(|app| app.app.as_str()).collect();
    assert_eq!(
        ids.last(),
        Some(&"so.tilo.lang.board"),
        "the package last, after what it uses"
    );
    for used in [
        "so.tilo.lang.notes",
        "so.tilo.lang.todo",
        "so.tilo.lang.chat",
    ] {
        assert!(ids.contains(&used), "{used} is an app of its own: {ids:?}");
    }
    for app in &apps {
        assert!(app.tapp.is_file(), "{} has its .tapp", app.app);
        let manifest =
            Manifest::from_toml_str(&std::fs::read_to_string(&app.manifest).unwrap()).unwrap();
        assert_eq!(manifest.package.id, app.app, "the app id is the package's");
        let entrypoint = manifest.entrypoint.as_ref().unwrap();
        assert_eq!(
            entrypoint.runtime.as_deref(),
            Some(package::DEFAULT_TILOLANG_RUNTIME),
            "it runs on the tilolang runtime bundle"
        );
        assert_eq!(entrypoint.bin, Path::new(package::RUNTIME_BIN));
        assert_eq!(manifest.package.icon.as_deref(), Some("icon.svg"));
        assert!(app.payload.join("icon.svg").is_file());
        assert!(app.payload.join("app.wasm").is_file());
    }
    let board = apps.last().unwrap();
    let notes = apps
        .iter()
        .find(|app| app.app == "so.tilo.lang.notes")
        .unwrap();
    let board_manifest =
        Manifest::from_toml_str(&std::fs::read_to_string(&board.manifest).unwrap()).unwrap();
    assert_eq!(board_manifest.package.name.as_deref(), Some("Board"));
    assert_eq!(board_manifest.package.category.as_deref(), Some("Office"));
    // Board uses Notes: it reads their records, declared, and carries their schema.
    let surface = std::fs::read(board.payload.join("surface.toml")).unwrap();
    let surface = collab_types_surface(&surface);
    let notes_surface =
        collab_types_surface(&std::fs::read(notes.payload.join("surface.toml")).unwrap());
    assert!(
        surface.reads_any_of(&notes_surface),
        "Board's surface declares that it reads Notes' kinds"
    );
    assert!(
        board
            .payload
            .join("imports/so.tilo.lang.notes.json")
            .is_file()
    );
    let installed = tilolang_tilo::bundle::Bundle::read(&board.payload).unwrap();
    assert_eq!(installed.meta.app, "so.tilo.lang.board");
    assert!(
        installed
            .imports
            .iter()
            .any(|meta| meta.app == "so.tilo.lang.notes")
    );
}

fn collab_types_surface(bytes: &[u8]) -> collab_types::surface::SurfaceFile {
    collab_types::surface::SurfaceFile::parse(bytes).unwrap()
}

fn setenv<'a>(argv: &'a [String], key: &str) -> Option<&'a str> {
    argv.windows(3)
        .find(|window| window[0] == "--setenv" && window[1] == key)
        .map(|window| window[2].as_str())
}

/// The launcher runs a built app as the tilolang runtime's binary over the app's payload, under
/// the app's own id, with its icon and its surface.
#[test]
fn the_launcher_runs_a_built_app_on_the_runtime_binary_over_its_payload() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().join("tl/counter");
    std::fs::create_dir_all(&dir).unwrap();
    for entry in std::fs::read_dir(packages().join("counter")).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), dir.join(entry.file_name())).unwrap();
    }
    let identity = tilolang_cli::identity_at(&scratch.path().join("publisher.json")).unwrap();
    let options = BuildOptions {
        dir,
        search: Vec::new(),
        out: scratch.path().join("out"),
        tilo_runtime: "tilo-runtime/2026.07".into(),
        tilolang_runtime: "tilolang/0.1.0+tilolang".into(),
        version: "0.1.0".into(),
    };
    let apps = package::build(&options, &identity).unwrap();
    let [counter] = apps.as_slice() else {
        panic!("one app: {apps:?}");
    };
    let manifest =
        Manifest::from_toml_str(&std::fs::read_to_string(&counter.manifest).unwrap()).unwrap();
    let view = launcher::manifest::ManifestView::try_from(manifest).unwrap();
    let argv = launcher::sandbox::build_bwrap_argv(
        &view,
        &counter.payload,
        None,
        &[],
        None,
        None,
        &launcher::capabilities::CapabilityPlan::empty(),
    );
    assert_eq!(
        argv.last().map(String::as_str),
        Some("/run/tilo-runtime/0.1.0+tilolang/bin/tilolang-app"),
        "the runtime bundle's binary runs"
    );
    let payload = launcher::sandbox::payload_mount_for("so.tilo.lang.counter");
    assert_eq!(
        setenv(&argv, "TILO_APP_MODULE").map(Path::new),
        Some(payload.as_path()),
        "pointed at the app's payload"
    );
    assert_eq!(
        setenv(&argv, "TILO_PACKAGE_ID"),
        Some("so.tilo.lang.counter")
    );
    assert_eq!(
        setenv(&argv, "TILO_APP_ICON").map(Path::new),
        Some(payload.join("icon.svg").as_path()),
        "its signed icon"
    );
    assert!(
        setenv(&argv, "TILO_SURFACE").is_some(),
        "its surface: {argv:?}"
    );
}
