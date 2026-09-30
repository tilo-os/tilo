//! Exercise the installed-style CLI dispatch, including an idle launch import.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions fail closed when the actual CLI cannot be observed"
)]

use std::path::Path;

fn run_calendar(directory: &Path, source: Option<&Path>, name: &str) -> serde_json::Value {
    let dump = directory.join(format!("{name}.json"));
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_calendar"));
    command.current_dir(directory);
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("TILO_") || name.starts_with("QUICK_") {
            command.env_remove(key);
        }
    }
    for key in [
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_RUNTIME_DIR",
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
    ] {
        let path = directory.join(key);
        std::fs::create_dir_all(&path).unwrap();
        command.env(key, path);
    }
    command.env("TILO_PACKAGE_ID", "so.tilo.calendar.cli-test");
    if let Some(source) = source {
        command.arg(source);
    }
    command.arg("--headless-drive").arg(
        serde_json::json!({
            "actions": [], "dump": dump,
        })
        .to_string(),
    );
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let stdout = directory.join(format!("{name}-stdout.txt"));
    let stderr = directory.join(format!("{name}-stderr.txt"));
    command
        .stdout(std::process::Stdio::from(
            std::fs::File::create(&stdout).unwrap(),
        ))
        .stderr(std::process::Stdio::from(
            std::fs::File::create(&stderr).unwrap(),
        ));
    let mut child = command.spawn().expect("launch actual Calendar binary");
    let started = ui::time::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > tilo_test_env::hang_bound() {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!(
                "Calendar {name} never exited within the hang bound: {}",
                std::fs::read_to_string(&stderr).unwrap()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(
        status.success(),
        "Calendar {name}: {}\n{}",
        std::fs::read_to_string(stdout).unwrap(),
        std::fs::read_to_string(stderr).unwrap()
    );
    serde_json::from_slice(&std::fs::read(dump).expect("CLI must write requested state")).unwrap()
}

#[test]
fn positional_ics_imports_and_persists_without_synthetic_input() {
    let temporary = std::env::temp_dir().canonicalize().unwrap();
    let directory = temporary.join(format!("calendar-cli-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("Kalender – Zürich 日本.ics");
    let payload = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:idle-launch\r\nSUMMARY:Idle launch import\r\nDTSTART:20260919T090000\r\nDTEND:20260919T100000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    std::fs::write(&source, payload).unwrap();
    let imported = run_calendar(&directory, Some(&source), "imported");
    let restarted = run_calendar(&directory, None, "restarted");
    for state in [&imported, &restarted] {
        let events = state["events"].as_object().expect("imported events map");
        assert_eq!(events.len(), 1);
        let event = events.values().next().unwrap();
        assert_eq!(event["title"], "Idle launch import");
        assert_eq!(event["start"], "2026-09-19T09:00");
    }
    assert_eq!(std::fs::read_to_string(&source).unwrap(), payload);
    let directory = directory.canonicalize().unwrap();
    assert!(directory.starts_with(&temporary));
    std::fs::remove_dir_all(directory).unwrap();
}
