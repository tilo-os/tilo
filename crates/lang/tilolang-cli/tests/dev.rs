//! A saved build is staged for the session's daemon as the app's manifest and payload, unsigned:
//! the device marks it, packs it, signs it with its own key and runs it under the id its marking
//! derives.

use tilolang_cli::package::{self, BuildOptions};

use crate::support::{copy_package, packages};

#[test]
fn a_saved_build_is_staged_unsigned_for_the_device() {
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("counter");
    copy_package(&packages().join("counter"), &source);
    let options = BuildOptions {
        dir: source,
        search: Vec::new(),
        out: scratch.path().join("out"),
        tilo_runtime: package::DEFAULT_TILO_RUNTIME.into(),
        tilolang_runtime: package::default_tilolang_runtime(),
        version: "0.1.0".into(),
        locked: false,
    };
    let staged = package::stage_dev(&options).unwrap().pop().unwrap();
    let manifest = tilo_app_package::manifest::Manifest::from_toml_str(
        &std::fs::read_to_string(&staged.manifest).unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest.package.id, staged.app,
        "the app's own id, unsigned"
    );
    assert_eq!(manifest.publisher.identity, "", "no publisher signs it");
    assert!(manifest.publisher.signature.is_empty());
    assert!(
        manifest.package.dev.is_none(),
        "the device marks it, not the tool"
    );
    // The device reads the manifest apart from the payload it packs.
    assert!(
        staged
            .payload
            .iter()
            .all(|(path, _)| path != "tilo-app.toml"),
        "{:?}",
        staged.payload
    );
    assert!(
        staged
            .payload
            .iter()
            .any(|(path, _)| path == tilolang_tilo::bundle::WASM_FILE),
        "the build's module is in the payload: {:?}",
        staged.payload
    );
    assert!(
        staged.payload.iter().all(|(_, source)| source.is_file()),
        "every payload file is on disk where the manifest names it"
    );
    // Nothing is packed: the device packs the `.tapp` it signs.
    let mut pending = vec![staged.dir.clone()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                pending.push(path);
            } else {
                assert_ne!(
                    path.extension().and_then(|extension| extension.to_str()),
                    Some("tapp"),
                    "{} is a package the tool would have signed",
                    path.display()
                );
            }
        }
    }
    // A save is compared by its digest: the same sources stage to the same one.
    let again = package::stage_dev(&options).unwrap().pop().unwrap();
    assert_eq!(again.digest, staged.digest);
}
