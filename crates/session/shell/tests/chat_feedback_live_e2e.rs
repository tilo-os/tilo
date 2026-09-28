//! A report posted in #feedback with the real Chat binary, in the shell
//! itself: `shell --backend headless` launches Chat through `tilo-launcher`
//! and bubblewrap from Chat's own manifest and a runtime bundle the launcher
//! trusts, exactly as a person's session does. The session owns a public
//! room, adds #feedback there and marks it through Chat's own actions, and
//! posts a report the way an agent or the palette does. The report is a
//! comment record, which only a launched Chat's own record store can mint;
//! it must be kept, and show in Chat.
//!
//! It needs the `chat` and `tilo-launcher` binaries next to the shell's,
//! which a run naming their packages builds:
//!
//! ```text
//! rcargo test -p shell -p chat -p launcher --test it chat_feedback_live_e2e::
//! ```
//!
//! Without them the test fails and names that command: a test that cannot
//! observe its subject must not pass.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use shell::shell::replay::headless::{HeadlessOptions, HeadlessReplay};

const CHAT: &str = "so.tilo.chat";
const REPORT: &str = "Send does nothing when the To field ends with a comma";
/// Build the binaries this test runs, then run it. A test build does not
/// produce the apps' binaries, so the first step is not optional.
const RUN: &str = "rcargo build -p chat -p launcher -p shell --bins && \
                   rcargo test -p shell --test it chat_feedback_live_e2e::";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("the shell crate lives under <repo>/crates/session")
        .to_path_buf()
}

/// A binary Cargo built next to the shell's.
fn built(name: &str) -> PathBuf {
    let shell = PathBuf::from(env!("CARGO_BIN_EXE_shell"));
    let path = shell
        .parent()
        .expect("the shell binary has a directory")
        .join(name);
    assert!(
        path.exists(),
        "{} is not built, so this test cannot run the real app. Build it, then run the test: {RUN}",
        path.display()
    );
    path
}

fn later(by: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(by).unwrap_or(now)
}

fn log_text(root: &Path) -> String {
    String::from_utf8_lossy(&std::fs::read(root.join("shell.log")).unwrap_or_default()).into_owned()
}

/// The app store the session launches from: Chat installed from its own
/// manifest, and the runtime bundle it pins, signed by a publisher the
/// launcher is told to trust.
struct Store {
    apps: PathBuf,
    runtime: PathBuf,
    trusted: PathBuf,
}

fn install_chat(root: &Path) -> Store {
    let apps = root.join("apps");
    let payload = apps.join("payloads").join(CHAT);
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::copy(built("chat"), payload.join("bin").join("chat")).unwrap();
    let manifest_text =
        std::fs::read_to_string(repo_root().join("apps/chat/tilo-app.toml")).unwrap();
    let manifest = payload.join("manifest.toml");
    std::fs::write(&manifest, &manifest_text).unwrap();
    launcher::manifest::read_manifest(&manifest).expect("the launcher reads Chat's own manifest");
    std::fs::write(
        apps.join("registry.toml"),
        format!(
            "[[entry]]\nid = '{CHAT}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
            payload.display().to_string(),
            manifest.display().to_string()
        ),
    )
    .unwrap();
    let pinned: toml::Table = manifest_text.parse().unwrap();
    let version = pinned["package"]["runtime"]
        .as_str()
        .and_then(|runtime| runtime.strip_prefix("tilo-runtime/"))
        .expect("Chat pins a runtime bundle")
        .to_string();
    let runtime = root.join("runtime");
    let identity = runtime_bundle::Identity::generate();
    let identity_path = root.join("runtime-publisher.json");
    std::fs::write(&identity_path, identity.to_json_bytes()).unwrap();
    runtime_bundle::build_bundle_with(
        &runtime.join(&version),
        &version,
        &runtime_bundle::BuildOptions {
            from: None,
            arch: None,
            identity: Some(&identity_path),
            extra_libs: &[],
        },
    )
    .expect("the runtime bundle builds from this machine's libraries");
    let trusted = root.join("trusted-publishers.txt");
    std::fs::write(&trusted, format!("{}\n", identity.public_key_hex())).unwrap();
    Store {
        apps,
        runtime,
        trusted,
    }
}

struct Session {
    shell: HeadlessReplay,
    root: PathBuf,
}

impl Session {
    fn call(&self, request: &Value) -> Value {
        self.shell
            .call(request)
            .unwrap_or_else(|error| panic!("{request} failed: {error}\n{}", self.log()))
    }

    fn log(&self) -> String {
        let log = log_text(&self.root);
        let lines: Vec<&str> = log
            .lines()
            .filter(|line| !line.contains("skipping rule for"))
            .collect();
        format!(
            "last lines of the shell's log:\n{}",
            lines[lines.len().saturating_sub(120)..].join("\n")
        )
    }

    fn until<T>(
        &self,
        what: &str,
        within: Duration,
        mut done: impl FnMut(&Self) -> Option<T>,
    ) -> T {
        let deadline = later(within);
        loop {
            if let Some(value) = done(self) {
                return value;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}\n{}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn active_room(&self) -> Option<String> {
        let tree = self.call(&json!({"op": "state_dump"}))["state"]["workspace_tree"].clone();
        let desktop = if tree["desktop"].is_null() {
            tree
        } else {
            tree["desktop"].clone()
        };
        let active = desktop["active"].clone();
        desktop["workspaces"]
            .as_array()?
            .iter()
            .find(|workspace| workspace["id"] == active)
            .and_then(|workspace| workspace["room"].as_str().map(str::to_string))
    }

    /// Chat's published action `action` on `resource`, as a caller lists it.
    fn chat_action(&self, resource: &str, action: &str) -> Option<Value> {
        let listed = self.call(&json!({"op": "actions_enumerate", "query": "feedback"}));
        listed["answer"]["actions"]
            .as_array()?
            .iter()
            .find(|row| {
                row["provider"] == json!("app-actions")
                    && row["resource"] == json!(resource)
                    && row["id"]
                        .as_str()
                        .is_some_and(|id| id.ends_with(&format!("\u{1f}{action}")))
            })
            .cloned()
    }

    fn invoke(&self, row: &Value, args: &Value) {
        self.call(&json!({
            "op": "actions_invoke",
            "provider": row["provider"],
            "id": row["id"],
            "args": args,
        }));
    }

    /// Chat's semantic tree, as this session sees it.
    fn chat_tree(&self, surface: u64) -> Option<String> {
        let started = self
            .shell
            .call(&json!({
                "op": "inspect",
                "surface": surface,
                "query": {"kind": "subtree", "root": null, "depth": 64},
                "local": true,
            }))
            .ok()?;
        let inspection = started["answer"]["inspection"].as_u64()?;
        let deadline = later(Duration::from_secs(5));
        while Instant::now() < deadline {
            let answer = self
                .shell
                .call(&json!({"op": "inspect_answer", "inspection": inspection}))
                .ok()?;
            if answer["answer"]["pending"] != json!(true) {
                return Some(answer["answer"].to_string());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    fn chat_surface(&self) -> Option<u64> {
        let frames = self.call(&json!({"op": "foreign_frames"}));
        frames["foreign_frames"]["frames"]
            .as_array()?
            .iter()
            .find(|window| {
                window["app_id"].as_str() == Some(CHAT)
                    && window["frame_seq"].as_u64().is_some_and(|seq| seq > 0)
            })
            .and_then(|window| window["surface_id"].as_u64())
    }
}

#[test]
fn a_report_posted_in_feedback_by_the_real_chat_is_kept_and_shows() {
    let dir = tempfile::tempdir().unwrap();
    let store = install_chat(dir.path());
    let session_root = dir.path().join("s");
    let shell = HeadlessReplay::start(&HeadlessOptions {
        shell: PathBuf::from(env!("CARGO_BIN_EXE_shell")),
        root: session_root.clone(),
        apps_dir: Some(store.apps),
        runtime_bundle_dir: Some(store.runtime),
        trusted_publishers: Some(store.trusted),
        size: (1280, 800),
        settle: Duration::from_millis(1500),
    })
    .unwrap_or_else(|error| {
        panic!(
            "the headless shell did not start: {error}\n{}",
            log_text(&session_root)
        )
    });
    built("tilo-launcher");
    let session = Session {
        shell,
        root: session_root,
    };

    // A public room this session owns, made the active workspace from the
    // top bar, the way a person moves to it.
    let created = session.call(&json!({"op": "public_room_create", "name": "Community"}));
    let room = created["answer"]["room"]
        .as_str()
        .expect("the new room's id")
        .to_string();
    let deadline = later(Duration::from_secs(20));
    while session.active_room().as_deref() != Some(room.as_str()) {
        assert!(
            Instant::now() < deadline,
            "the top bar never made {room} active ({created}); the workspaces: {}\n{}",
            session.call(&json!({"op": "state_dump"}))["state"]["workspace_tree"],
            session.log()
        );
        session.call(&json!({"op": "click_named", "name": "topbar_workspace_down"}));
        std::thread::sleep(Duration::from_millis(400));
    }

    // Chat, opened by its address, in the room.
    session.until("Chat to open", Duration::from_secs(60), |s| {
        s.shell
            .call(&json!({"op": "actions_invoke", "provider": "applications", "id": CHAT}))
            .ok()
    });
    let surface = session.until(
        "Chat to paint",
        Duration::from_secs(90),
        Session::chat_surface,
    );

    let feedback = format!("chat.channel:{}", tilo_reference::FEEDBACK_CHANNEL);
    let add = session.until("Chat to offer #feedback", Duration::from_secs(90), |s| {
        s.chat_action("chat.workspace:ws_home", "add-feedback-channel")
    });
    session.invoke(&add, &json!({}));
    let mark = session.until("#feedback to exist", Duration::from_secs(90), |s| {
        s.chat_action(&feedback, "mark-feedback")
    });
    session.invoke(&mark, &json!({}));
    let open = session.until("#feedback to be marked", Duration::from_secs(90), |s| {
        s.chat_action(&feedback, "unmark-feedback")?;
        s.chat_action(&feedback, "open")
    });
    session.invoke(&open, &json!({}));

    let post = session.until(
        "Chat to offer posting in #feedback",
        Duration::from_secs(30),
        |s| s.chat_action(&feedback, "post"),
    );
    session.invoke(&post, &json!({"value": REPORT}));
    session.until("the report to show in Chat", Duration::from_secs(60), |s| {
        s.chat_tree(surface)
            .filter(|tree| tree.contains(REPORT))
            .map(|_| ())
    });
    assert!(
        !session.log().contains("a comment was not kept"),
        "the report was kept as the author's record\n{}",
        session.log()
    );
}
