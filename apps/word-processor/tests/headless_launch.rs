//! Run the actual production CLI against isolated Markdown fixtures. No GPU,
//! native window, mocked importer, or alternate app host is involved.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let temporary = std::env::temp_dir().canonicalize().unwrap();
        let unique = ui::time::try_unix_now_ns().unwrap();
        let root = temporary.join(format!(
            "wp-headless-launch-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        assert!(root.starts_with(&temporary));
        Self(root)
    }
    fn run(&self, name: &str, source: &Path) -> (bool, PathBuf) {
        let inspect = self.0.join(format!("{name}-inspect.json"));
        let payload = serde_json::json!({
            "inspect": inspect,
            "dump": self.0.join(format!("{name}-document.json")),
        });
        let mut command = Command::new(env!("CARGO_BIN_EXE_word-processor"));
        command
            .current_dir(&self.0)
            .arg("--headless-drive")
            .arg(payload.to_string())
            .arg(source);
        // Change only the child's environment, so parallel tests cannot point
        // one another's writers at a fixture or at the real user profile.
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
            "LOCALAPPDATA",
            "APPDATA",
            "HOME",
            "USERPROFILE",
            "TEMP",
            "TMP",
        ] {
            let directory = self.0.join(key);
            std::fs::create_dir_all(&directory).unwrap();
            command.env(key, directory);
        }
        command
            .env("TILO_PACKAGE_ID", "so.tilo.word-processor")
            .env("TILO_WORKSPACE_ID", "00000000-0000-0000-0000-000000000000");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let stdout = self.0.join(format!("{name}-stdout.txt"));
        let stderr = self.0.join(format!("{name}-stderr.txt"));
        command
            .stdout(Stdio::from(std::fs::File::create(stdout).unwrap()))
            .stderr(Stdio::from(std::fs::File::create(&stderr).unwrap()));
        let mut child = command.spawn().unwrap();
        let started = ui::time::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if started.elapsed() > tilo_test_env::hang_bound() {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!(
                    "word-processor headless fixture never exited within the hang bound: {}",
                    std::fs::read_to_string(stderr).unwrap()
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        (status.success(), inspect)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Whether the inspected tree under `node` holds a widget named `name`.
fn has_widget(node: &serde_json::Value, name: &str) -> bool {
    node["widget"] == name
        || node["children"]
            .as_array()
            .is_some_and(|children| children.iter().any(|child| has_widget(child, name)))
}

#[test]
fn positional_markdown_reaches_production_headless_root_without_stale_body_merge() {
    let fixture = Fixture::new();
    let source = fixture.0.join("document.md");
    for (name, marker, absent) in [
        ("first", "WP-FIRST-FILE-CONTENT", "WP-SECOND-FILE-CONTENT"),
        ("reopen", "WP-SECOND-FILE-CONTENT", "WP-FIRST-FILE-CONTENT"),
    ] {
        let contents = format!("# Fixture\n\n{marker}\n");
        std::fs::write(&source, &contents).unwrap();
        let (success, inspect) = fixture.run(name, &source);
        assert!(success, "actual headless CLI failed for {name}");
        let text = std::fs::read_to_string(inspect).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(
            has_widget(&value["root"], "ResourceApp"),
            "the framework app is the production root: {text}"
        );
        assert!(
            text.contains(marker),
            "supplied file text missing from actual production root: {text}"
        );
        assert!(
            !text.contains(absent),
            "headless merged an obsolete body despite native in-memory record policy"
        );
        assert_eq!(std::fs::read_to_string(&source).unwrap(), contents);
    }
    let (success, inspect) = fixture.run("missing", &fixture.0.join("missing.md"));
    assert!(
        !success,
        "missing input must fail, not open a blank fallback document"
    );
    assert!(!inspect.exists());
}
