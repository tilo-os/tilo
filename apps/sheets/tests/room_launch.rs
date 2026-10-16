#![allow(clippy::unwrap_used, reason = "test assertions")]
//! Where a Sheets launch keeps its workbook.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn launch(host: &Path, env: &[(&str, &str)]) {
    std::fs::create_dir_all(host).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_sheets"));
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
    let status = command
        .current_dir(host)
        .env("TILO_PACKAGE_ID", "so.tilo.sheets")
        .envs(env.iter().copied())
        .arg("--data-dir")
        .arg(host)
        .arg("--headless-drive")
        .arg(serde_json::json!({"actions": [], "dump": host.join("dump.json")}).to_string())
        .stdin(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "sheets exited with {status}");
}

fn record_dirs(root: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let mut pending: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                names.push(entry.file_name().to_string_lossy().into_owned());
                pending.push(entry.path());
            }
        }
    }
    names
}

/// Launched into a room, Sheets opens the room's default record, the one
/// every other device's first Sheets window (a browser's too) opens, so
/// they edit one workbook. Nothing beside it holds a copy of the workbook.
#[test]
fn a_room_launch_opens_the_rooms_default_record() {
    let temp = tempfile::tempdir().unwrap();
    let host = temp.path().join("room");
    launch(
        &host,
        &[
            ("TILO_WORKSPACE_ID", "095789bd-bf1b-4215-9d78-78129060e306"),
            ("TILO_PARTICIPATION", "author"),
        ],
    );
    let dirs = record_dirs(&host);
    let room_default = ui::record::RecordId::room_default().storage_key();
    assert!(dirs.contains(&room_default), "{dirs:?}");
    assert!(
        !host.join("tilo/sheets/workbook.automerge").exists(),
        "the workbook is the record, not a file of the app's own"
    );
}
