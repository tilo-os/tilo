//! Replay with the real Notes binary, in the shell itself: `shell --backend
//! headless` launches Notes through `tilo-launcher` and bubblewrap, exactly
//! as a person's session does, from Notes' own package manifest and a signed
//! runtime bundle the launcher trusts. A person records typing into Notes
//! through the shell's `remark.*` actions; the session is replayed with
//! `replay.*` into a replay room, where a second Notes process must reach
//! what was typed; and review then checks the recording against its own
//! replay out of sight, which must find nothing below the toolkit.
//!
//! It needs the `notes` and `tilo-launcher` binaries next to the shell's,
//! which a run naming their packages builds:
//!
//! ```text
//! rcargo test -p shell -p notes -p launcher --test it replay_headless_e2e::
//! ```
//!
//! Without them the test fails and names that command: a test that cannot
//! observe its subject must not pass.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use shell::shell::replay::headless::{HeadlessOptions, HeadlessReplay};

const NOTES: &str = "so.tilo.notes";
const MARKER: &str = "Replayed words";
const RUN: &str = "rcargo test -p shell -p notes -p launcher --test it replay_headless_e2e::";

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
        "{} is not built, so this test cannot run the real app. Build it with the test: {RUN}",
        path.display()
    );
    path
}

fn later(by: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(by).unwrap_or(now)
}

/// The session's log. Apps write to it too, not always in UTF-8.
fn log_text(root: &Path) -> String {
    String::from_utf8_lossy(&std::fs::read(root.join("shell.log")).unwrap_or_default()).into_owned()
}

/// The app store this session launches from: Notes installed from its own
/// manifest, and the runtime bundle it pins, signed by a publisher the
/// launcher is told to trust.
struct Store {
    apps: PathBuf,
    runtime: PathBuf,
    trusted: PathBuf,
}

fn install_notes(root: &Path) -> Store {
    let apps = root.join("apps");
    let payload = apps.join("payloads").join(NOTES);
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::copy(built("notes"), payload.join("bin").join("notes")).unwrap();
    let source = repo_root().join("apps/notes/tilo-app.toml");
    let manifest_text = std::fs::read_to_string(&source).unwrap();
    let manifest = payload.join("manifest.toml");
    std::fs::write(&manifest, &manifest_text).unwrap();
    launcher::manifest::read_manifest(&manifest).expect("the launcher reads Notes' own manifest");
    std::fs::write(
        apps.join("registry.toml"),
        format!(
            "[[entry]]\nid = '{NOTES}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
            payload.display().to_string(),
            manifest.display().to_string()
        ),
    )
    .unwrap();

    let pinned: toml::Table = manifest_text.parse().unwrap();
    let version = pinned["package"]["runtime"]
        .as_str()
        .and_then(|runtime| runtime.strip_prefix("tilo-runtime/"))
        .expect("Notes pins a runtime bundle")
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
    {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", &apps);
        shell::shell::launcher::invalidate_app_cache();
        let offered = shell::shell::launcher::gui_apps();
        shell::shell::launcher::invalidate_app_cache();
        assert!(
            offered.iter().any(|app| app.package_id == NOTES),
            "the store lists Notes: {offered:?}"
        );
    }
    let trusted = root.join("trusted-publishers.txt");
    std::fs::write(&trusted, format!("{}\n", identity.public_key_hex())).unwrap();
    Store {
        apps,
        runtime,
        trusted,
    }
}

/// The headless session and what the test reads of it.
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

    fn act(&self, id: &str, args: &Value) -> Value {
        self.shell
            .shell_action(id, args)
            .unwrap_or_else(|error| panic!("{id} failed: {error}\n{}", self.log()))
    }

    fn state(&self) -> Value {
        self.call(&json!({"op": "state_dump"}))["state"].clone()
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
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Every app window the shell tracks: `(surface, app, frames taken, rect)`.
    fn windows(&self) -> Vec<Value> {
        let answer = self.call(&json!({"op": "foreign_frames"}));
        answer["foreign_frames"]["frames"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// A painted Notes window other than `except`.
    fn painted_notes(&self, except: &[u64]) -> Option<Value> {
        self.windows().into_iter().find(|window| {
            window["app_id"].as_str() == Some(NOTES)
                && window["frame_seq"].as_u64().is_some_and(|seq| seq > 0)
                && window["surface_id"]
                    .as_u64()
                    .is_some_and(|surface| !except.contains(&surface))
        })
    }

    /// Notes' semantic tree behind `surface`, as this session sees it.
    fn inspect(&self, surface: u64) -> Option<Value> {
        self.try_inspect(surface).ok()
    }

    /// [`Self::inspect`], saying why there is no answer.
    fn try_inspect(&self, surface: u64) -> Result<Value, String> {
        let started = self
            .shell
            .call(&json!({
                "op": "inspect",
                "surface": surface,
                "query": {"kind": "subtree", "root": null, "depth": 64},
                "local": true,
            }))
            .map_err(|error| error.to_string())?;
        let inspection = started["answer"]["inspection"]
            .as_u64()
            .ok_or_else(|| format!("no inspection id in {started}"))?;
        let deadline = later(Duration::from_secs(5));
        while Instant::now() < deadline {
            let answer = self
                .shell
                .call(&json!({"op": "inspect_answer", "inspection": inspection}))
                .map_err(|error| error.to_string())?;
            if answer["answer"]["pending"] != json!(true) {
                return Ok(answer["answer"].clone());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err("the app did not answer within 5 s".into())
    }

    fn shows(&self, surface: u64, text: &str) -> bool {
        self.inspect(surface)
            .is_some_and(|tree| tree.to_string().contains(text))
    }

    /// Press where a person would: the pointer enters the window first,
    /// then the button goes down and up, each delivered to the app.
    fn click(&self, x: f64, y: f64) {
        self.call(&json!({"op": "pointer_move", "x": x, "y": y}));
        std::thread::sleep(Duration::from_millis(350));
        self.call(&json!({"op": "pointer_button", "pressed": true}));
        std::thread::sleep(Duration::from_millis(300));
        self.call(&json!({"op": "pointer_button", "pressed": false}));
    }
}

/// A semantic tree, one line per node: role, label, value and bounds.
fn outline(tree: &Value) -> String {
    fn walk(node: &Value, depth: usize, out: &mut String) {
        use std::fmt::Write as _;
        let named = node.get("role").is_some();
        if let Some(role) = node.get("role") {
            let _ = write!(
                out,
                "\n{}{} {} {} {}",
                "  ".repeat(depth),
                role,
                node["label"],
                node["value"],
                node["bounds"]
            );
        }
        let depth = depth.saturating_add(usize::from(named));
        match node {
            Value::Object(map) => map.values().for_each(|child| walk(child, depth, out)),
            Value::Array(items) => items.iter().for_each(|child| walk(child, depth, out)),
            _ => {}
        }
    }
    let mut out = String::new();
    walk(tree, 0, &mut out);
    out
}

/// The bounds of the first node of `role` named `label` in a semantic
/// tree, when it has bounds of its own.
fn find_named(tree: &Value, role: &str, label: &str) -> Option<Value> {
    match tree {
        Value::Object(map) => {
            if map.get("role") == Some(&json!(role)) && map.get("label") == Some(&json!(label)) {
                return map
                    .get("bounds")
                    .filter(|bounds| !bounds.is_null())
                    .cloned();
            }
            map.values()
                .find_map(|child| find_named(child, role, label))
        }
        Value::Array(items) => items
            .iter()
            .find_map(|child| find_named(child, role, label)),
        _ => None,
    }
}

impl Session {
    /// Launch Notes as a person does, from the applications the palette
    /// offers, and wait until it has painted and answers inspection.
    /// Returns its window.
    fn launch_notes(&self) -> Value {
        // Discovery keeps the applications index, as the palette does for a
        // person; Notes is offered once it is built.
        let deadline = later(Duration::from_secs(30));
        while let Err(error) = {
            self.call(&json!({"op": "actions_enumerate", "query": "Notes"}));
            self.shell
                .call(&json!({"op": "actions_invoke", "provider": "applications", "id": NOTES}))
        } {
            assert!(
                Instant::now() < deadline,
                "Notes was never offered ({error}); what is offered for it: {}\n{}",
                self.call(&json!({"op": "actions_enumerate", "query": "Notes"})),
                self.log()
            );
            std::thread::sleep(Duration::from_millis(250));
        }
        let window = self.until("Notes to paint", Duration::from_secs(60), |s| {
            s.painted_notes(&[])
        });
        let surface = window["surface_id"].as_u64().unwrap();
        let deadline = later(Duration::from_secs(30));
        while let Err(why) = self.try_inspect(surface) {
            assert!(
                Instant::now() < deadline,
                "Notes never answered inspection: {why}\ninspectable apps: {}\n{}",
                self.call(&json!({"op": "inspect_apps"})),
                self.log()
            );
            std::thread::sleep(Duration::from_millis(250));
        }
        window
    }

    /// Open the first note of Notes' empty notebook by pressing its "New
    /// note" button where Notes says it is, and return where the note's
    /// body is on screen.
    fn open_first_note(&self, window: &Value) -> (f64, f64) {
        let surface = window["surface_id"].as_u64().unwrap();
        let rect = &window["rect"];
        let number = |key: &str| rect[key].as_f64().unwrap_or_default();
        let (left, top) = (number("x"), number("y"));
        let button = self.until("Notes' \"New note\" button", Duration::from_secs(10), |s| {
            s.inspect(surface)
                .and_then(|tree| find_named(&tree, "button", "New note"))
        });
        let at = |key: &str| button[key].as_f64().unwrap_or_default();
        self.click(
            left + at("x") + at("width") / 2.0,
            top + at("y") + at("height") / 2.0,
        );
        let deadline = later(Duration::from_secs(10));
        let body = loop {
            let tree = self.inspect(surface).unwrap_or_default();
            if find_named(&tree, "button", "New note").is_none()
                && let Some(body) = find_named(&tree, "multiline_text_input", "Block text")
            {
                break body;
            }
            assert!(
                Instant::now() < deadline,
                "pressing \"New note\" at {button} did not open a note with a body.\nits tree: {}\n{}",
                outline(&tree),
                self.log()
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        (
            left + body["x"].as_f64().unwrap() + 24.0,
            top + body["y"].as_f64().unwrap() + 12.0,
        )
    }

    /// Record typing [`MARKER`] into Notes at `(x, y)` through the remark
    /// composer, and return the session record that then awaits review.
    fn record_typing(&self, surface: u64, (x, y): (f64, f64)) -> String {
        self.act("remark.open", &json!({}));
        self.act("remark.record.start", &json!({}));
        self.until(
            "the recording's first snapshot",
            Duration::from_secs(30),
            |s| {
                s.state()["chrome"]["feedback"]["recording"]["snapshots"]
                    .as_u64()
                    .is_some_and(|snapshots| snapshots > 0)
                    .then_some(())
            },
        );
        self.click(x, y);
        self.call(&json!({"op": "type", "text": MARKER}));
        self.until(
            "Notes to show what was typed",
            Duration::from_secs(20),
            |s| s.shows(surface, MARKER).then_some(()),
        );
        // Let the recording take its frames of the typed state.
        std::thread::sleep(Duration::from_secs(2));
        self.act("remark.record.stop", &json!({}));
        self.until(
            "the session to await review",
            Duration::from_secs(20),
            |s| {
                s.state()["chrome"]["feedback"]["awaiting_review"][0]["record"]
                    .as_str()
                    .map(str::to_string)
            },
        )
    }

    /// Replay `record` shown, stand at its end until the Notes replayed in
    /// the replay room shows what was typed, then close the replay.
    fn replay_to_its_end(&self, record: &str, recorded_surface: u64) {
        self.act("replay.open", &json!({"record": record}));
        let status = self.until("the replay to open", Duration::from_secs(20), |s| {
            let status = s.state()["chrome"]["replay"].clone();
            (!status.is_null()).then_some(status)
        });
        assert_eq!(
            status["apps"],
            json!([NOTES]),
            "the replay starts Notes: {status}"
        );
        let duration = status["duration_ms"].as_u64().unwrap();
        self.act("replay.seek", &json!({"at_ms": duration}));
        // The replay room is shown, so its windows are the ones on screen:
        // Notes, started again in it.
        let replayed = self.until(
            "the replayed Notes to reach what was typed",
            Duration::from_secs(90),
            |s| {
                s.windows()
                    .iter()
                    .filter(|window| window["app_id"] == json!(NOTES))
                    .filter_map(|window| window["surface_id"].as_u64())
                    .filter(|surface| *surface != recorded_surface)
                    .find(|surface| s.shows(*surface, MARKER))
            },
        );
        let status = self.state()["chrome"]["replay"].clone();
        assert_eq!(
            status["published"], status["events"],
            "the replay stands at its end: {status}"
        );
        self.act("replay.close", &json!({}));
        self.until(
            "the replayed Notes to close",
            Duration::from_secs(30),
            |s| {
                (!s.windows()
                    .iter()
                    .any(|window| window["surface_id"].as_u64() == Some(replayed)))
                .then_some(())
            },
        );
    }

    /// Open review of the recording and wait for its replay check.
    fn review_verdict(&self) -> Value {
        self.act("remark.review.open", &json!({}));
        let deadline = later(Duration::from_secs(180));
        loop {
            let state = self.state();
            let verdict = state["chrome"]["remark"]["review"]["verdict"].clone();
            if !verdict.is_null() && verdict["state"] != json!("checking") {
                return verdict;
            }
            assert!(
                Instant::now() < deadline,
                "review's replay check did not finish.\nverdict: {verdict}\nchecks: {}\n{}",
                state["chrome"]["remark"]["review"]["checks"],
                self.log()
            );
            std::thread::sleep(Duration::from_millis(500));
        }
    }
}

#[test]
fn a_session_recorded_in_real_notes_replays_to_its_end_and_checks_as_logic() {
    let dir = tempfile::tempdir().unwrap();
    let store = install_notes(dir.path());
    let root = dir.path().join("s");
    let shell = HeadlessReplay::start(&HeadlessOptions {
        shell: PathBuf::from(env!("CARGO_BIN_EXE_shell")),
        root: root.clone(),
        apps_dir: Some(store.apps),
        runtime_bundle_dir: Some(store.runtime),
        trusted_publishers: Some(store.trusted),
        size: (1280, 800),
        settle: Duration::from_millis(1500),
    })
    .unwrap_or_else(|error| {
        panic!(
            "the headless shell did not start: {error}\n{}",
            log_text(&root)
        )
    });
    built("tilo-launcher");
    let session = Session { shell, root };

    let window = session.launch_notes();
    let surface = window["surface_id"].as_u64().unwrap();
    let person_was_in = session.state()["workspace_tree"]["desktop"]["active"].clone();
    let body = session.open_first_note(&window);
    let record = session.record_typing(surface, body);
    session.replay_to_its_end(&record, surface);

    let verdict = session.review_verdict();
    assert_eq!(
        verdict["state"],
        json!("checked"),
        "review checked the recording: {verdict}\n{}",
        session.log()
    );
    assert!(
        verdict["compared"]
            .as_u64()
            .is_some_and(|compared| compared > 0),
        "frames were compared: {verdict}"
    );
    assert_eq!(
        verdict["layer"],
        json!("logic"),
        "a recording checked against its own replay matches it: {verdict}"
    );
    assert_eq!(
        session.state()["workspace_tree"]["desktop"]["active"],
        person_was_in,
        "the check never moved the person"
    );
}
