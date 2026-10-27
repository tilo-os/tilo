use std::path::Path;
use std::process::{Command, Stdio};

fn run(root: &Path, host: &Path, target: Option<&Path>, dump: &Path) -> serde_json::Value {
    drive(root, host, target, dump, &serde_json::json!([]))
}

fn drive(
    root: &Path,
    host: &Path,
    target: Option<&Path>,
    dump: &Path,
    actions: &serde_json::Value,
) -> serde_json::Value {
    std::fs::create_dir_all(host).unwrap();
    let mut command = Command::new(tilo_test_env::package_binary!("notes"));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("TILO_") || key.to_string_lossy().starts_with("QUICK_")
        {
            command.env_remove(key);
        }
    }
    for key in [
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_RUNTIME_DIR",
        "LOCALAPPDATA",
        "APPDATA",
        "HOME",
        "USERPROFILE",
        "TEMP",
        "TMP",
        "TMPDIR",
    ] {
        command.env(key, host);
    }
    command.current_dir(host);
    command
        .env("TILO_PACKAGE_ID", "so.tilo.notes")
        .arg("--data-dir")
        .arg(root)
        .arg("--headless-drive")
        .arg(serde_json::json!({"actions": actions, "dump": dump}).to_string());
    if let Some(target) = target {
        command.arg(target);
    }
    let stdout = host.join("stdout.log");
    let stderr = host.join("stderr.log");
    command
        .stdout(Stdio::from(std::fs::File::create(&stdout).unwrap()))
        .stderr(Stdio::from(std::fs::File::create(&stderr).unwrap()));
    let mut child = command.spawn().unwrap();
    let deadline = tilo_test_env::hang_deadline();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if ui::time::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!(
                "actual Notes CLI timed out: {}",
                std::fs::read_to_string(&stderr).unwrap()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(
        status.success(),
        "actual Notes CLI failed: {}\n{}",
        std::fs::read_to_string(stdout).unwrap(),
        std::fs::read_to_string(stderr).unwrap()
    );
    assert!(
        !host.join("so.tilo.notes/state.automerge").exists(),
        "widget-owned notebook must not open a substitute host document"
    );
    serde_json::from_slice(&std::fs::read(dump).unwrap()).unwrap()
}

/// The paragraphs of a page in a dumped notebook, in no particular order.
fn texts(page: &serde_json::Value) -> Vec<String> {
    page["v"]["body"]["blocks"]
        .as_object()
        .unwrap()
        .values()
        .filter_map(|block| block["text"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn positional_markdown_imports_and_persists_without_synthetic_input() {
    let temp = tempfile::tempdir().unwrap();
    let notebook = temp.path().join("Notebook ü");
    let existing = temp.path().join("Existing.md");
    let imported = temp.path().join("Grüße 日本語.md");
    let host = temp.path().join("host");
    std::fs::write(&existing, "# Existing\n\nKeep this body.").unwrap();
    std::fs::write(&imported, "# Imported\n\nUnicode body.").unwrap();
    let first = run(
        &notebook,
        &host,
        Some(&existing),
        &temp.path().join("first.json"),
    );
    let pages = first["pages"].as_object().unwrap();
    assert_eq!(pages.len(), 1);
    let old_id = pages.keys().next().unwrap();
    let old_page = pages[old_id].clone();
    let second = run(
        &notebook,
        &host,
        Some(&imported),
        &temp.path().join("second.json"),
    );
    assert_eq!(second["pages"].as_object().unwrap().len(), 2);
    assert_eq!(
        second["pages"][old_id]["v"]["title"],
        old_page["v"]["title"]
    );
    assert!(texts(&second["pages"][old_id]).contains(&"Keep this body.".to_owned()));
    let restored = run(&notebook, &host, None, &temp.path().join("restart.json"));
    assert_eq!(restored["pages"].as_object().unwrap().len(), 2);
    let new_page = restored["pages"]
        .as_object()
        .unwrap()
        .values()
        .find(|page| page["v"]["title"] == "Grüße 日本語")
        .unwrap();
    let mut body = texts(new_page);
    body.sort();
    assert_eq!(body, ["Imported", "Unicode body."]);
    assert_eq!(
        std::fs::read_to_string(existing).unwrap(),
        "# Existing\n\nKeep this body."
    );
    assert_eq!(
        std::fs::read_to_string(imported).unwrap(),
        "# Imported\n\nUnicode body."
    );
}

/// A launch opens the room's notebook, the one record every Notes window in
/// the room opens (not one per window), so they edit one notebook.
#[test]
fn a_launch_opens_the_rooms_notebook() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("notebook");
    run(
        &data,
        &temp.path().join("host"),
        None,
        &temp.path().join("dump.json"),
    );
    let room = data.join("so.tilo.notes").join("rooms").join("personal");
    assert!(room.join("state.automerge").is_file(), "{room:?}");
    assert!(
        !room.join("records").exists(),
        "no per-window record beside the room's notebook"
    );
}

/// The empty state's "New note" is found where it is drawn: its button
/// names its own bounds, so a click at them (a screen reader's, a test
/// driver's) creates the first note instead of landing on empty panel.
#[test]
fn clicking_new_note_where_its_button_says_it_is_creates_the_first_note() {
    let temp = tempfile::tempdir().unwrap();
    let dump = drive(
        &temp.path().join("notebook"),
        &temp.path().join("host"),
        None,
        &temp.path().join("dump.json"),
        &serde_json::json!([{"click_named": "notes.first_note"}]),
    );
    assert_eq!(dump["pages"].as_object().unwrap().len(), 1, "{dump}");
}
