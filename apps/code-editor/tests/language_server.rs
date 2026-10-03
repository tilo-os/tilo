//! End-to-end language-server test: a real `rust-analyzer` process, a real
//! Cargo project on disk, a real deliberate error, and the real diagnostic that
//! comes back.
//!
//! Unit tests cover the protocol parser; this test verifies project-root
//! resolution and diagnostics against a real filesystem directory.
//!
//! The test builds its own tiny crate rather than using this workspace: Tilo has
//! ~200 members and indexing it takes minutes, which is not a unit of work a
//! test suite should contain. The plumbing under test — root resolution,
//! `initialize`, answering the server's own requests, `didOpen`, `$/progress`,
//! `publishDiagnostics` — is identical at either size.
//!
//! The test requires `rust-analyzer` and `cargo` on PATH. It also verifies
//! `$/progress`, which depends on the client advertising work-done progress.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use ui::time::Duration;

use code_editor::lsp::{LspClient, LspEvent};
use code_editor::model::Language;

/// rust-analyzer has to run `cargo metadata` and build a crate graph before it
/// says anything. On a one-file crate that is seconds; the ceiling is generous
/// so a loaded machine reports the real failure rather than a timeout.
const BUDGET: Duration = Duration::from_secs(180);

/// A minimal but genuine Cargo project containing one deliberate error.
///
/// # Errors
/// Returns an error if the temp directory or its fixture files can't be
/// created — a real filesystem problem the caller (a `#[test]` fn, where
/// `.expect()` is the correct test-failure mode) should fail loudly on.
fn project_with_a_deliberate_error() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    std::fs::create_dir_all(root.join("src"))?;
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"tilo-lsp-probe\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
         [dependencies]\n",
    )?;
    // `parse_limitt` does not exist. rustc's message for this is stable and
    // specific, which is what makes it assertable.
    std::fs::write(
        root.join("src/main.rs"),
        "fn parse_limit(n: u64) -> u64 {\n    n * 2\n}\n\n\
         fn main() {\n    let v = parse_limitt(21);\n    println!(\"{v}\");\n}\n",
    )?;
    Ok(dir)
}

/// Drive a client until `want` returns true, or the budget runs out. Returns
/// every event seen, so a failure can report what actually happened.
fn pump_until(client: &mut LspClient, want: impl Fn(&[LspEvent]) -> bool) -> (Vec<LspEvent>, bool) {
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "test value, cannot overflow: BUDGET is a fixed few-minute Duration added to the current Instant"
    )]
    let deadline = ui::time::now() + BUDGET;
    let mut seen = Vec::new();
    while ui::time::now() < deadline {
        for ev in client.poll() {
            // Fold progress through the client the way the app does, so the
            // task table under test is the one the status bar reads.
            if let LspEvent::Progress(p) = &ev {
                client.note_progress(p);
            }
            seen.push(ev);
        }
        if want(&seen) {
            return (seen, true);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    (seen, false)
}

fn require_binary(name: &str) {
    let executable = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let found = std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|p| p.join(&executable).is_file()));
    assert!(
        found,
        "{name} is not on PATH. This test proves the editor's language server \
         produces real diagnostics against a real project; without {name} it \
         cannot observe its subject, so it fails rather than passing blindly."
    );
}

#[test]
fn rust_analyzer_indexes_visibly_and_reports_a_real_error_in_a_real_project() {
    // Each phase builds its own fixture project and starts its own server.
    rust_analyzer_reports_a_real_error_in_a_real_project();
    indexing_is_visible_while_it_happens();
}

fn rust_analyzer_reports_a_real_error_in_a_real_project() {
    let mut wake_scope = ui::wake::WakeScope::new();
    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    wake_scope
        .bind(move |event| {
            let _ = wake_tx.send(event);
        })
        .unwrap();
    let _wake_guard = wake_scope.enter();
    require_binary("rust-analyzer");
    require_binary("cargo");

    let project = project_with_a_deliberate_error().expect("build the fixture project");
    let root = project.path();
    let main_rs = root.join("src/main.rs");

    let mut client = LspClient::start(Language::Rust, root)
        .expect("rust-analyzer starts against a real directory");
    assert_eq!(client.server_name(), "rust-analyzer");
    assert_eq!(
        client.root(),
        root,
        "the server is rooted at the directory we opened, not a synthetic path"
    );

    let uri = code_editor::lsp::path_to_uri(&main_rs);
    let text = std::fs::read_to_string(&main_rs).unwrap();
    client.did_open(&uri, "rust", 1, &text);
    // rustc-backed diagnostics arrive in response to a save, which is why the
    // editor sends this after every successful write.
    client.did_save(&uri);

    let (events, ok) = pump_until(&mut client, |seen| {
        seen.iter().any(
            |e| matches!(e, LspEvent::Diagnostics { diagnostics, .. } if !diagnostics.is_empty()),
        )
    });

    assert!(
        ok,
        "no diagnostics arrived within {BUDGET:?}. Events seen: {}",
        summarise(&events)
    );
    let wake = wake_rx
        .try_recv()
        .expect("the server must wake an idle editor when events arrive");
    assert!(wake_scope.accepts(&wake));

    // The diagnostic must be the real one, for the real file, at the real spot.
    let (diag_uri, diags) = events
        .iter()
        .rev()
        .find_map(|e| match e {
            LspEvent::Diagnostics { uri, diagnostics } if !diagnostics.is_empty() => {
                Some((uri.clone(), diagnostics.clone()))
            }
            _ => None,
        })
        .expect("checked above");

    assert_eq!(
        diag_uri, uri,
        "the diagnostic is against the file we opened"
    );
    let found = diags
        .iter()
        .find(|d| d.message.contains("parse_limitt") || d.message.contains("cannot find"))
        .unwrap_or_else(|| {
            panic!(
                "expected an error naming the undefined function; got: {:?}",
                diags.iter().map(|d| &d.message).collect::<Vec<_>>()
            )
        });
    assert_eq!(
        found.severity, 1,
        "an unresolved name is an error, not a hint"
    );
    assert_eq!(
        found.range.start.line, 5,
        "the error is on the line the call is on (0-based)"
    );

    // And it projects onto the Problems panel in 1-based coordinates.
    let panel = found.to_panel();
    assert_eq!(panel.line, 6);
    assert!(panel.message.contains("parse_limitt") || panel.message.contains("cannot find"));
}

fn indexing_is_visible_while_it_happens() {
    // The other half of the fix: a server that takes minutes must not be
    // indistinguishable from a broken one. rust-analyzer only sends
    // `$/progress` if the client advertised `window.workDoneProgress`, so this
    // asserts on the capability having actually been sent.
    require_binary("rust-analyzer");
    require_binary("cargo");

    let project = project_with_a_deliberate_error().expect("build the fixture project");
    let mut client = LspClient::start(Language::Rust, project.path())
        .expect("rust-analyzer starts against a real directory");

    let (events, ok) = pump_until(&mut client, |seen| {
        seen.iter().any(|e| matches!(e, LspEvent::Progress(_)))
    });
    assert!(
        ok,
        "the server never reported progress, so the editor could not show an \
         indexing state. Events seen: {}",
        summarise(&events)
    );

    let titled = events
        .iter()
        .any(|e| matches!(e, LspEvent::Progress(p) if !p.title.is_empty()));
    assert!(titled, "at least one progress notification names its task");
}

/// A compact description of what a pump saw, for failure messages.
fn summarise(events: &[LspEvent]) -> String {
    if events.is_empty() {
        return "(nothing at all — the server never answered)".to_string();
    }
    let mut out = Vec::new();
    for e in events {
        out.push(match e {
            LspEvent::Initialized => "Initialized".to_string(),
            LspEvent::Exited => "Exited".to_string(),
            LspEvent::Progress(p) => format!(
                "Progress({}{})",
                p.title,
                p.percentage.map(|v| format!(" {v}%")).unwrap_or_default()
            ),
            LspEvent::Diagnostics { uri, diagnostics } => {
                format!("Diagnostics({} for {uri})", diagnostics.len())
            }
            LspEvent::Completion(i) => format!("Completion({})", i.len()),
            LspEvent::Hover(_) => "Hover".to_string(),
            LspEvent::Definition(l) => format!("Definition({})", l.len()),
        });
    }
    out.join(", ")
}

/// A guard that the module the test drives is the one the app drives.
#[test]
fn the_editor_roots_its_server_at_a_real_directory() {
    let d = tempfile::tempdir().unwrap();
    let missing = d.path().join("nope");
    let err = LspClient::start(Language::Rust, &missing)
        .err()
        .expect("a path that is not a directory must be refused");
    assert!(err.contains("not a directory"), "got: {err}");

    // And a real one produces a real `file://` URI for its workspace root.
    let uri = code_editor::lsp::path_to_uri(d.path());
    assert_eq!(
        std::fs::canonicalize(url::Url::parse(&uri).unwrap().to_file_path().unwrap()).unwrap(),
        std::fs::canonicalize(d.path()).unwrap()
    );
}
