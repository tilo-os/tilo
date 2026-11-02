//! The desktop loop publishes each saved build where the running app reads it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use std::path::Path;

use tilolang_cli::package::{self, BuildOptions};
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::upgrade::{Upgrade, compare};

fn packages() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl")
}

/// Copy the package at `from` into `to`, created.
fn copy_package(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_package(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn a_saved_build_is_the_generation_the_app_reads() {
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("counter");
    copy_package(&packages().join("counter"), &source);
    let identity = tilolang_cli::identity_at(&scratch.path().join("publisher.json")).unwrap();
    let out = scratch.path().join("out");
    let options = BuildOptions {
        dir: source.clone(),
        search: Vec::new(),
        out,
        tilo_runtime: package::DEFAULT_TILO_RUNTIME.into(),
        tilolang_runtime: package::DEFAULT_TILOLANG_RUNTIME.into(),
        version: "0.1.0".into(),
    };
    let publish = |app: &package::Published, updates: &Path| {
        tilo_app_module::publish_with(updates, |generation| {
            app.bundle
                .write(generation)
                .map_err(|error| std::io::Error::other(error.to_string()))
        })
        .unwrap()
    };
    let first = package::published(&options, &identity).unwrap();
    let app = first.last().unwrap();
    let updates = scratch.path().join("updates");
    assert_eq!(publish(app, &updates), 1);
    assert_eq!(tilo_app_module::current(&updates).unwrap(), Some(1));
    let read = Bundle::read(&updates.join("1")).unwrap();
    assert_eq!(read.meta.app, app.app, "the app reads its own package");
    assert_eq!(read.meta.title, app.meta.title);
    assert!(
        !options.out.exists() || std::fs::read_dir(&options.out).unwrap().next().is_none(),
        "a desktop build stages, packs and signs nothing"
    );
    assert_eq!(
        package::build(&options, &identity)
            .unwrap()
            .last()
            .unwrap()
            .digest,
        app.digest,
        "the same build has the same digest whether or not it is installed"
    );

    // A save inside the package's views: the next build is published as the next generation,
    // and the one the running app still reads stays.
    let file = source.join("app.tl");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(
        &file,
        text.replace(
            "/// Add one to the counter.",
            "/// Add one to the counter, whatever it shows.",
        ),
    )
    .unwrap();
    let second = package::published(&options, &identity).unwrap();
    let app = second.last().unwrap();
    assert_eq!(
        compare(&first.last().unwrap().meta, &app.meta),
        Upgrade::InPlace,
        "a view's text is taken in place"
    );
    assert_eq!(publish(app, &updates), 2);
    assert!(updates.join("1").is_dir(), "what the app runs stays");
    let read = Bundle::read(&updates.join("2")).unwrap();
    assert_eq!(read.meta.app, app.app);
}
