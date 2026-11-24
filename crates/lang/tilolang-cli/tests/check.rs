//! `tilolang check`: every package in `apps/tl` checks clean, the platform's packages it uses
//! (`use so.tilo.device`) found as `build` and `layout-check` find them; what `--write-ids`
//! writes is what a check calls current.

use std::path::Path;
use std::process::Command;
use tilo_test_env::SpawnUnderEnvLock as _;

use crate::support::{copy_package, packages};

#[test]
fn every_package_in_apps_tl_checks_clean() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl");
    let mut dirs: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        // A tilolang package is a directory of `.tl` files; a native app beside them (the session's
        // committed `surface.toml`) is what packages `use`, not one to check.
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

/// A workspace's own actions are an app a package `use`s, read from the surface its service
/// publishes: `check` compiles a call of one with no workspace running.
#[test]
fn a_workspace_action_checks_without_a_workspace_running() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().join("inviter");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(
        dir.join("app.tl"),
        "app test.inviter \"Inviter\"\n\nuse tilo.workspace as ws\n\n\
         action invite_editor() {\n    ws.invite(role: \"editor\")\n}\n",
    )
    .unwrap();
    // The ids lock and the source catalogue are written first, as a new package's are.
    let (written, said) = check(&dir, true);
    assert!(written, "{said}");
    let (passed, said) = check(&dir, false);
    assert!(passed, "{said}");
}

/// Packages load into one registry, which refuses an id two of them declare, so a check refuses
/// a lock holding an id a package beside it holds, naming the declaration and where the id is
/// already taken.
#[test]
fn a_check_refuses_an_id_a_package_beside_it_holds() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().join("counter");
    copy_package(&packages().join("counter"), &dir);
    let taken = id_of(&dir, "increment@Counter");
    let beside = scratch.path().join("beside");
    std::fs::create_dir(&beside).unwrap();
    std::fs::write(
        beside.join("app.ids"),
        format!("# tilolang ids: generated, commit it. Edit only through `tilolang ids --rename`.\nadd = {taken}\n"),
    )
    .unwrap();

    let (passed, said) = check(&dir, false);
    assert!(!passed, "the counter's increment shares its id:\n{said}");
    assert!(
        said.contains(&format!("id `{taken}` of `increment@Counter` is also {}'s", beside.display())),
        "{said}"
    );

    std::fs::remove_dir_all(&beside).unwrap();
    let (passed, said) = check(&dir, false);
    assert!(passed, "an id no other package holds checks:\n{said}");
}

/// A word the source stops saying is reported in every translation of it, and writing the ids lock
/// writes no catalogue: the build writes the source catalogue from the sources, and a translation is
/// its translator's to edit.
#[test]
fn a_word_the_source_dropped_is_reported_until_its_translation_drops_it() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().join("counter");
    copy_package(&packages().join("counter"), &dir);
    drop_decrement(&dir);
    // A package commits no source catalogue: its words are its sources' alone.
    let source = dir.join("locales/en-US.ftl");
    std::fs::remove_file(&source).expect("the package ships a catalogue to drop");
    let key = format!("d-{}", id_of(&dir, "decrement@Counter"));

    let (passed, stderr) = check(&dir, false);
    assert!(!passed, "the German translation still says it:\n{stderr}");
    assert!(
        stderr.contains(&format!(
            "locales/de-DE.ftl translates words the source does not have: {key}"
        )),
        "{stderr}"
    );

    // Writing the ids lock retires the action's id and writes no catalogue.
    let (passed, stderr) = check(&dir, true);
    assert!(!passed, "the translation still says it:\n{stderr}");
    assert!(
        !source.exists(),
        "writing writes no source catalogue:\n{stderr}"
    );

    // Its translator drops the word, and every word left is the source's.
    let german = dir.join("locales/de-DE.ftl");
    let translated = std::fs::read_to_string(&german).unwrap();
    let dropped: String = translated
        .split("\n\n")
        .filter(|block| !block.lines().any(|line| line.starts_with(&key)))
        .collect::<Vec<_>>()
        .join("\n\n");
    std::fs::write(&german, dropped).unwrap();
    let (passed, stderr) = check(&dir, false);
    assert!(passed, "every translated word is the source's:\n{stderr}");
}

/// A word no message of the source has ever had is its translator's own mistake: writing the ids
/// lock leaves it where its translator put it, and it stays reported until they remove it.
#[test]
fn writing_the_ids_lock_keeps_a_translations_own_word_reported() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().join("counter");
    copy_package(&packages().join("counter"), &dir);
    drop_decrement(&dir);
    let translation = dir.join("locales/de-DE.ftl");
    let increment = format!("d-{} = Erhöhen\n", id_of(&dir, "increment@Counter"));
    std::fs::write(
        &translation,
        format!("{increment}\nd-zzzzzzzz = Verringern\n"),
    )
    .unwrap();

    let (passed, stderr) = check(&dir, true);
    assert!(
        !passed,
        "a translation of a word no message has is an error:\n{stderr}"
    );
    assert!(
        stderr.contains("locales/de-DE.ftl translates words the source does not have: d-zzzzzzzz"),
        "{stderr}"
    );
    let written = std::fs::read_to_string(&translation).unwrap();
    assert!(
        written.contains("d-zzzzzzzz = Verringern"),
        "writing keeps the translator's own word:\n{written}"
    );
    assert!(written.contains(&increment), "{written}");

    std::fs::write(&translation, increment).unwrap();
    let (passed, stderr) = check(&dir, true);
    assert!(passed, "every translated word is the source's:\n{stderr}");
}

/// The standard library is read from its source like any package: run in a checkout, the tool
/// compiles against the checkout's, so an edit of it is what the next command sees, without
/// rebuilding the tool. Run outside one, it has the packaged copy.
#[test]
fn an_edit_of_the_standard_library_in_a_checkout_is_what_the_tool_compiles_against() {
    let scratch = tempfile::tempdir().unwrap();
    let tree = scratch.path().join("tree");
    let lib = tree.join(tilolang::stdlib::IN_TREE);
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(lib.join("app.ids"), tilolang::stdlib::IDS).unwrap();
    std::fs::write(
        lib.join("app.tl"),
        format!(
            "{}\n/// A number standing as the title of a page.\ntemplate view big_number(n: Int) {{\n    ui.Text(\"{{n}}\", role: .title, numeric: true)\n}}\n",
            tilolang::stdlib::SOURCE
        ),
    )
    .unwrap();
    let dir = tree.join("apps/tl/counter");
    copy_package(&packages().join("counter"), &dir);
    let source = dir.join("app.tl");
    let text = std::fs::read_to_string(&source).unwrap();
    let shown = "ui.Text(\"{c.value}\", role: .title, numeric: true)";
    assert!(text.contains(shown), "the counter shows its value:\n{text}");
    std::fs::write(&source, text.replace(shown, "std.big_number(c.value)")).unwrap();

    let run = |cwd: &Path| {
        let output = Command::new(env!("CARGO_BIN_EXE_tilolang"))
            .arg("check")
            .arg(&dir)
            .current_dir(cwd)
            .output_locked()
            .unwrap();
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    let (passed, said) = run(&dir);
    assert!(passed, "the checkout's library has `big_number`:\n{said}");
    let (passed, said) = run(scratch.path());
    assert!(!passed, "the packaged library has no `big_number`");
    assert!(said.contains("big_number"), "{said}");

    std::fs::write(
        lib.join("app.tl"),
        "app tilo.std \"Standard library\"\n\nview broken(\n",
    )
    .unwrap();
    let (passed, said) = run(&tree);
    assert!(
        !passed,
        "a library that does not parse is no library to compile against"
    );
    assert!(
        said.contains("does not compile") && said.contains("app.tl"),
        "it says the checkout's library is what failed, and where: {said}"
    );
}

/// Runs `check` on `dir`, writing with `--write-ids` when `write`: whether it passed, and what it
/// said.
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

/// The counter at `dir` without its decrement: the action, and the button that ran it.
fn drop_decrement(dir: &Path) {
    let source = dir.join("app.tl");
    let text = std::fs::read_to_string(&source).unwrap();
    let action = "/// Take one away from the counter.\naction decrement(c: Counter) (icon: .minus) {\n    c.value -= 1\n}\n\n";
    assert!(
        text.contains(action),
        "the counter declares decrement:\n{text}"
    );
    let text = text.replacen(action, "", 1);
    let button = "        button(decrement(c)) as minus\n";
    assert!(text.contains(button), "a button runs decrement:\n{text}");
    std::fs::write(&source, text.replacen(button, "", 1)).unwrap();
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
