//! `tilolang check`: every package in `apps/tl` checks clean, the platform's packages it uses
//! (`use so.tilo.device`) found as `build` and `layout-check` find them; what `--write-ids`
//! writes is what a check calls current.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::path::Path;
use std::process::Command;
use tilo_test_env::SpawnUnderEnvLock as _;

mod support;

use support::{copy_package, packages};

#[test]
fn every_package_in_apps_tl_checks_clean() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl");
    let mut dirs: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        // A tilolang package is a directory of `.tl` files; a native app beside them (the
        // session's committed `surface.toml`) is what packages `use`, not one to check.
        .filter(|path| tilolang::source::source_paths(path).is_ok_and(|files| !files.is_empty()))
        .collect();
    dirs.sort();
    assert!(dirs.iter().any(|dir| dir.ends_with("shell")), "{dirs:?}");
    let mut failed = Vec::new();
    for dir in &dirs {
        let output = Command::new(env!("CARGO_BIN_EXE_tilolang"))
            .arg("check")
            .arg(dir)
            .output_locked()
            .unwrap();
        if !output.status.success() {
            failed.push(format!(
                "{}: {}\n{}",
                dir.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// A message leaves the catalogue when its declaration leaves the source, so writing the
/// catalogue has to prune it: what `--write-ids` writes is what a check calls current, and the
/// id of what went is retired, never reused.
#[test]
fn writing_the_source_catalogue_prunes_the_messages_its_source_no_longer_has() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().join("counter");
    copy_package(&packages().join("counter"), &dir);
    let source = dir.join("app.tl");
    let text = std::fs::read_to_string(&source).unwrap();
    // The counter loses its decrement: the action, and the button that ran it.
    let action = "/// Take one away from the counter.\naction decrement(c: Counter) (icon: .minus) {\n    c.value -= 1\n}\n\n";
    assert!(
        text.contains(action),
        "the counter declares decrement:\n{text}"
    );
    let text = text.replacen(action, "", 1);
    let button = "        button(decrement(c)) as minus\n";
    assert!(text.contains(button), "a button runs decrement:\n{text}");
    std::fs::write(&source, text.replacen(button, "", 1)).unwrap();
    let key = format!("d-{}", id_of(&dir, "decrement"));
    let catalogue = dir.join("locales/en-US.ftl");
    let stale = std::fs::read_to_string(&catalogue).unwrap();
    assert!(
        stale.contains(&key),
        "the catalogue has decrement's message:\n{stale}"
    );

    let (passed, stderr) = check(&dir, false);
    assert!(!passed, "the catalogue left decrement's message:\n{stderr}");
    assert!(
        stderr.contains("the source catalogue (locales/en-US.ftl) is out of date"),
        "{stderr}"
    );
    assert!(stderr.contains("decrement"), "{stderr}");

    let (passed, stderr) = check(&dir, true);
    assert!(passed, "{stderr}");
    let written = std::fs::read_to_string(&catalogue).unwrap();
    assert!(!written.contains(&key), "writing prunes it:\n{written}");
    assert!(
        !written.contains("Decrement"),
        "writing prunes it:\n{written}"
    );
    assert!(
        written.contains(&format!("d-{}", id_of(&dir, "increment"))),
        "{written}"
    );
    let (passed, stderr) = check(&dir, false);
    assert!(passed, "what writing leaves is current:\n{stderr}");
}

/// A translation is the author's, so writing the source catalogue cannot prune it: a word the
/// source no longer has is still reported, and goes when its translator removes it.
#[test]
fn writing_the_source_catalogue_keeps_a_translations_stale_words_reported() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().join("counter");
    copy_package(&packages().join("counter"), &dir);
    let translation = dir.join("locales/de-DE.ftl");
    let increment = format!("d-{} = Erhöhen\n", id_of(&dir, "increment"));
    std::fs::write(
        &translation,
        format!("{increment}\nd-zzzzzzzz = Verringern\n"),
    )
    .unwrap();

    let (passed, stderr) = check(&dir, true);
    assert!(
        !passed,
        "a translation of a gone word is an error:\n{stderr}"
    );
    assert!(
        stderr.contains("locales/de-DE.ftl translates words the source no longer has: d-zzzzzzzz"),
        "{stderr}"
    );

    std::fs::write(&translation, increment).unwrap();
    let (passed, stderr) = check(&dir, true);
    assert!(passed, "every translated word is the source's:\n{stderr}");
}

/// An app's read effects are actions of its resource in the catalogue: a package calls one and
/// reads a field of the value it answers with, typed by the surface the app publishes.
#[test]
fn a_native_app_read_action_answers_with_its_published_type() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().join("reader");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(
        dir.join("app.tl"),
        "app so.tilo.test.reader \"Reader\"\n\nuse so.tilo.pdf-viewer as pdf\n\n\
         action file_of(d: pdf.Document) -> Text {\n    let form = pdf.read_form_fields(d)\n    form.file\n}\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tilolang"))
        .arg("check")
        .arg(&dir)
        .arg("--search")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps"))
        .output_locked()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // A field the PDF's form does not answer with is a compile error, as a typed value's.
    std::fs::write(
        dir.join("app.tl"),
        "app so.tilo.test.reader \"Reader\"\n\nuse so.tilo.pdf-viewer as pdf\n\n\
         action pages_of(d: pdf.Document) -> Int {\n    let form = pdf.read_form_fields(d)\n    form.pages\n}\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tilolang"))
        .arg("check")
        .arg(&dir)
        .arg("--search")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps"))
        .output_locked()
        .unwrap();
    assert!(!output.status.success(), "a form has no pages");
}

/// Runs `check` on `dir`, writing with `--write-ids` when `write`: whether it passed, and what
/// it said.
fn check(dir: &Path, write: bool) -> (bool, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tilolang"));
    command.arg("check").arg(dir);
    if write {
        command.arg("--write-ids");
    }
    let output = command.output_locked().unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// The id the package's lock gives the declaration `name`.
fn id_of(dir: &Path, name: &str) -> String {
    let ids = std::fs::read_to_string(dir.join("app.ids")).unwrap();
    let entry = format!("{name} = ");
    ids.lines()
        .find_map(|line| line.strip_prefix(&entry))
        .unwrap_or_else(|| panic!("`{name}` has an id in app.ids:\n{ids}"))
        .trim()
        .to_owned()
}
