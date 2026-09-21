use std::path::Path;
use std::process::{Command, Stdio};

fn run(root: &Path, host: &Path, target: Option<&Path>, dump: &Path) -> serde_json::Value {
    std::fs::create_dir_all(host).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_notes"));
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
        .env("TILO_NOTES_ROOT", root)
        .env("TILO_PACKAGE_ID", "dev.tilo.notes")
        .arg("--headless-drive")
        .arg(serde_json::json!({"actions": [], "dump": dump}).to_string());
    if let Some(target) = target {
        command.arg(target);
    }
    let stdout = host.join("stdout.log");
    let stderr = host.join("stderr.log");
    command
        .stdout(Stdio::from(std::fs::File::create(&stdout).unwrap()))
        .stderr(Stdio::from(std::fs::File::create(&stderr).unwrap()));
    let mut child = command.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
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
        !host.join("dev.tilo.notes/state.automerge").exists(),
        "widget-owned notebook must not open a substitute host document"
    );
    serde_json::from_slice(&std::fs::read(dump).unwrap()).unwrap()
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
    assert_eq!(second["pages"][old_id]["title"], old_page["title"]);
    assert_eq!(
        second["pages"][old_id]["blocks"][1]["text"],
        "Keep this body."
    );
    let restored = run(&notebook, &host, None, &temp.path().join("restart.json"));
    assert_eq!(restored["pages"].as_object().unwrap().len(), 2);
    let new_page = restored["pages"]
        .as_object()
        .unwrap()
        .values()
        .find(|page| page["title"] == "Grüße 日本語")
        .unwrap();
    assert_eq!(new_page["blocks"][0]["text"], "Imported");
    assert_eq!(new_page["blocks"][1]["text"], "Unicode body.");
    assert_eq!(
        std::fs::read_to_string(existing).unwrap(),
        "# Existing\n\nKeep this body."
    );
    assert_eq!(
        std::fs::read_to_string(imported).unwrap(),
        "# Imported\n\nUnicode body."
    );
}
