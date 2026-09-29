//! An app generated outside the Tilo repository builds with no Tilo
//! workspace around it: `tilo-app new` must carry everything Cargo only reads
//! from the root workspace (the SDK's `[patch.crates-io]`, the toolchain, a
//! lock file with tested versions).
//!
//! It is a cold build of the whole SDK graph in a fresh directory, so it is
//! ignored by default. CI's SDK lane runs it:
//!
//! ```sh
//! cargo test -p tilo-app-cli --test outside_repo -- --ignored --nocapture
//! ```
//!
//! `TILO_SDK_SOURCE=git` takes the SDK from this checkout as a git
//! repository (`file://`, pinned to `HEAD`) instead of by path: the form a
//! developer without a checkout uses. `HEAD` must be committed.

#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::process::Command;

#[test]
#[ignore = "cold build of the SDK graph outside the workspace; run in CI's SDK lane"]
fn a_generated_app_builds_outside_the_tilo_repository() {
    let run = |command: &mut Command, what: &str| {
        // Never inherit a redirected target directory: the app has its own.
        command.env_remove("CARGO_TARGET_DIR");
        eprintln!("[outside-repo] {what}: {command:?}");
        let status = command.status().expect(what);
        assert!(status.success(), "{what} failed with {status}");
    };
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("checkout root")
        .canonicalize()
        .expect("canonical checkout");
    let parent = tempfile::Builder::new()
        .prefix("tilo-outside-")
        .tempdir_in(std::env::temp_dir())
        .expect("temporary directory");
    let parent_path = parent.path().canonicalize().expect("canonical temp dir");
    assert!(
        !parent_path.starts_with(&checkout),
        "the temporary directory must be outside the checkout"
    );
    let app = parent_path.join("hello");
    let tilo_app = env!("CARGO_BIN_EXE_tilo-app");

    let mut new = Command::new(tilo_app);
    new.args([
        "new",
        "hello",
        "--id",
        "com.example.hello",
        "--title",
        "Hello",
    ])
    .args(["--template", "starter", "--directory"])
    .arg(&app)
    .arg("--ui-path")
    .arg(checkout.join("crates/sdk/ui-core"))
    .current_dir(&parent_path);
    if std::env::var("TILO_SDK_SOURCE").as_deref() == Ok("git") {
        new.arg("--sdk-git")
            .arg(format!("file://{}", checkout.display()));
    }
    run(&mut new, "tilo-app new");

    let manifest = std::fs::read_to_string(app.join("Cargo.toml")).expect("generated manifest");
    assert!(manifest.contains("[patch.crates-io]"), "{manifest}");
    assert!(app.join("rust-toolchain.toml").is_file());
    assert!(app.join("Cargo.lock").is_file());

    let host = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    run(
        Command::new(tilo_app)
            .args(["build", "--target", host])
            .arg(&app)
            .current_dir(&app),
        "tilo-app build (native)",
    );
    run(
        Command::new(tilo_app)
            .args(["check", "--target", "web"])
            .arg(&app)
            .current_dir(&app),
        "tilo-app check --target web (wasm32)",
    );
    let built = app.join("target/tilo-app").join(host).join("hello");
    assert!(built.is_dir(), "no artifact under {}", built.display());
}
