//! Opening a document from search, end to end: Super+Space, the document's
//! name, Enter — through `ShellOverlayHost`, layout, winit key events, search
//! fan-out, `doc_open` routing, and
//! `tilo-launcher` invocation, asserting on the command actually executed.
//!
//! Two process-wide env vars make this assertable without a session:
//! `HOME` (with a matching XDG tree under it) is the session home the
//! Files provider indexes, and `TILO_LAUNCHER_BIN` (the launcher's
//! documented seam) points at a script that records its argv.
//!
//! The cargo runner already points XDG at a scratch profile. This test
//! must keep that layout under its own `HOME`; otherwise `indexable_home`
//! walks the runner profile and the document planted here is invisible.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use winit::event::{ElementState, Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::launcher;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::WorkspaceTree;

use crate::support::host::{Layout, dispatch_in, lay_out};

const W: f32 = 1280.0;
const H: f32 = 800.0;

fn lay_out_host(host: &mut ShellOverlayHost) -> Layout {
    lay_out(host, W, H)
}

/// Kept across events: the palette's input is CRDT-backed, so a fresh doc
/// per keystroke would lose every character but the last.
struct Session {
    doc: automerge::AutoCommit,
}

impl Session {
    fn new() -> Self {
        Self {
            doc: automerge::AutoCommit::new(),
        }
    }

    fn dispatch(
        &mut self,
        host: &mut ShellOverlayHost,
        layout: &Layout,
        modifiers: ModifiersState,
        event: &WindowEvent,
    ) {
        dispatch_in(
            &mut self.doc,
            host,
            &layout.taffy,
            layout.child_node,
            Modifiers::from(modifiers),
            event,
        );
    }

    fn press(
        &mut self,
        host: &mut ShellOverlayHost,
        layout: &Layout,
        modifiers: ModifiersState,
        key: &Key,
    ) {
        let event = ui::synthetic_input::key_input(key, ElementState::Pressed);
        self.dispatch(host, layout, modifiers, &event);
    }

    fn type_str(&mut self, host: &mut ShellOverlayHost, layout: &Layout, s: &str) {
        for c in s.chars() {
            self.press(
                host,
                layout,
                ModifiersState::empty(),
                &Key::Character(winit::keyboard::SmolStr::new(c.to_string())),
            );
        }
    }
}

/// Write a registry + manifest pair so `launcher::installed_apps()`
/// enumerates a package declaring `[providers].ext`.
fn install(dir: &Path, package_id: &str, name: &str, exts: &[&str]) {
    let payload = dir.join("payloads").join(package_id);
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    let manifest_path = payload.join("manifest.toml");
    let ext_list = exts
        .iter()
        .map(|e| format!("\"{e}\""))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        &manifest_path,
        format!(
            "[package]\nid = \"{package_id}\"\nname = \"{name}\"\n\
             version = \"1.0.0\"\nkind = \"app\"\n\
             [publisher]\nidentity = \"ed25519:{zeros}\"\n\
             [sandbox]\nnamespaces = []\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\
             [sandbox.net]\nmode = \"off\"\n\
             [entrypoint]\nbin = \"bin/app\"\n\
             type = \"gui\"\n[providers]\nmime = []\next = [{ext_list}]\n",
            zeros = "00".repeat(32)
        ),
    )
    .unwrap();
    let registry = dir.join("registry.toml");
    let mut existing = std::fs::read_to_string(&registry).unwrap_or_default();
    let _ = write!(
        existing,
        "[[entry]]\nid = '{package_id}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
        payload.display().to_string(),
        manifest_path.display().to_string()
    );
    std::fs::write(&registry, existing).unwrap();
}

fn install_recording_launcher(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let log = dir.join("launched.log");
    let bin = dir.join("tilo-launcher");
    std::fs::write(
        &bin,
        format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display()),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    log
}

fn launched_commands(log: &Path, want: usize) -> Vec<String> {
    let deadline = std::time::Instant::now()
        .checked_add(crate::support::HANG_BOUND)
        .expect("the hang bound fits in an Instant");
    while std::time::Instant::now() < deadline {
        if let Ok(s) = std::fs::read_to_string(log) {
            let lines: Vec<String> = s.lines().map(str::to_string).collect();
            if lines.len() >= want {
                return lines;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!(
        "expected {want} launch(es); the launcher recorded:\n{}",
        std::fs::read_to_string(log).unwrap_or_else(|_| "<nothing>".into())
    );
}

#[test]
#[serial_test::serial(process_env)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "st's only use is producing tree via tree(), read from through the inner \
block's final assert_eq!; nothing to drop earlier"
)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn documents_opened_from_the_palette_or_a_drop_open_in_their_own_app() {
    // Phase `activating_a_file_row_in_the_palette_opens_it_in_its_own_app`.
    {
        let tmp = tempfile::tempdir().expect("temp dir");
        let home = tmp.path().join("home");
        let docs = home.join("Documents");
        std::fs::create_dir_all(&docs).unwrap();
        let doc_path = docs.join("quarterly-report.odt");
        std::fs::write(&doc_path, b"quarterly numbers").unwrap();

        let apps = tmp.path().join("apps");
        std::fs::create_dir_all(&apps).unwrap();
        install(&apps, "so.tilo.word-processor", "Word Processor", &["odt"]);
        // A decoy claiming nothing and sorting first by display name: without a
        // second package, "resolve the owner" and "take whatever's installed"
        // look the same.
        install(&apps, "so.tilo.calculator", "Calculator", &[]);
        let runtime = tmp.path().join("run");
        std::fs::create_dir_all(&runtime).unwrap();
        let log = install_recording_launcher(&runtime);

        // Read on this thread and by the launch thread it spawns.
        // XDG must live under this HOME so FilesProvider walks it, not the
        // runner's scratch profile or a TILO_TEST_CONTROL cache home.
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", &home)
            .set("XDG_CONFIG_HOME", home.join(".config"))
            .set("XDG_CACHE_HOME", home.join(".cache"))
            .set("TILO_APPS_DATA_DIR", &apps)
            .set("XDG_RUNTIME_DIR", &runtime)
            .set("TILO_LAUNCHER_BIN", runtime.join("tilo-launcher"));
        env.remove("TILO_TEST_CONTROL");
        env.remove("TILO_IDENTITY_ROOT");
        launcher::invalidate_app_cache();
        launcher::clear_pending_placements();
        shell::shell::doc_open::clear_open_documents();
        launcher::set_wayland_socket(OsStr::new("wayland-test"));

        let tree = WorkspaceTree::bootstrap_demo();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let layout = lay_out_host(&mut host);
        let mut session = Session::new();

        // Super+Space — the global-scope palette chord the live binary binds.
        session.press(
            &mut host,
            &layout,
            ModifiersState::SUPER,
            &Key::Named(winit::keyboard::NamedKey::Space),
        );
        assert!(
            host.palette_visible(),
            "Super+Space must open the palette; without it there is no row to activate"
        );

        // The Files provider walks $HOME on a background thread, so re-type the
        // last character until the walk commits — exercising the production
        // per-keystroke refresh rather than reaching into the index.
        session.type_str(&mut host, &layout, "quarterly-repor");
        // The walk must happen; bound it by the hang limit, not a speed guess.
        let mut rows = Vec::new();
        let deadline = tilo_test_env::hang_deadline();
        while std::time::Instant::now() < deadline {
            session.type_str(&mut host, &layout, "t");
            session.press(
                &mut host,
                &layout,
                ModifiersState::empty(),
                &Key::Named(NamedKey::Backspace),
            );
            let (_, query, _, entries) = host.palette_snapshot();
            assert_eq!(query, "quarterly-repor", "the query must be what was typed");
            if entries
                .iter()
                .any(|(_, label)| label.contains("quarterly-report"))
            {
                rows = entries;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(
            rows.iter()
                .any(|(_, label)| label.contains("quarterly-report")),
            "the palette never offered the document; rows were {rows:#?}"
        );

        // Assert which row is selected first, or a palette that offered
        // something else looks identical to one that ignored the key.
        let (_, _, selected, entries) = host.palette_snapshot();
        let selected = selected.expect("a row is selected");
        let selected_label = entries.iter().find(|(id, _)| *id == selected).map_or_else(
            || panic!("selected row {selected} is not on offer: {entries:#?}"),
            |(_, label)| label.clone(),
        );
        assert!(
            selected_label.contains("quarterly-report"),
            "Enter would activate {selected_label:?}, not the document; rows were {entries:#?}"
        );
        session.press(
            &mut host,
            &layout,
            ModifiersState::empty(),
            &Key::Named(NamedKey::Enter),
        );

        let commands = launched_commands(&log, 1);
        assert_eq!(commands.len(), 1, "exactly one launch: {commands:#?}");
        let cmd = &commands[0];
        assert!(
            cmd.contains("--app-id so.tilo.word-processor"),
            "the document had to open in the package that declares .odt: {cmd}"
        );
        assert!(
            cmd.contains(&format!("--open {}", doc_path.display())),
            "and it had to be told which document: {cmd}"
        );

        // And the workspace holds a read grant for exactly that file, so the app
        // can read it inside its sandbox.
        {
            use shell::shell::state::{Effect, Scope, Subject};
            let st = chrome.borrow();
            let tree = st.tree();
            let ws = tree
                .desktop
                .active_workspace()
                .expect("an active workspace");
            let want = Subject::app("so.tilo.word-processor");
            let scopes: Vec<&Scope> = ws
                .capabilities
                .iter()
                .filter(|c| c.subject == want && c.effect == Effect::Grant)
                .map(|c| &c.scope)
                .collect();
            assert_eq!(
                scopes,
                vec![&Scope::FsPath(doc_path.display().to_string())],
                "one grant, for the document's own path"
            );
        }

        drop(env);
        launcher::invalidate_app_cache();
    }

    // Phase `dropping_a_document_on_a_group_opens_it_in_its_own_app`.
    {
        let tmp = tempfile::tempdir().expect("temp dir");
        let apps = tmp.path().join("apps");
        std::fs::create_dir_all(&apps).unwrap();
        install(&apps, "so.tilo.word-processor", "Word Processor", &["odt"]);
        // The decoy is what makes "the app that owns .odt" a different claim
        // from "whatever is installed".
        install(&apps, "so.tilo.calculator", "Calculator", &[]);
        let runtime = tmp.path().join("run");
        std::fs::create_dir_all(&runtime).unwrap();
        let log = install_recording_launcher(&runtime);
        let doc_path = tmp.path().join("dropped.odt");
        std::fs::write(&doc_path, b"dropped").unwrap();

        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", &apps)
            .set("XDG_RUNTIME_DIR", &runtime)
            .set("TILO_LAUNCHER_BIN", runtime.join("tilo-launcher"));
        launcher::invalidate_app_cache();
        launcher::clear_pending_placements();
        shell::shell::doc_open::clear_open_documents();
        launcher::set_wayland_socket(OsStr::new("wayland-test"));

        let tree = WorkspaceTree::bootstrap_demo();
        let strip_id = tree.desktop.workspaces[0].strips[0].id;
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let layout = lay_out_host(&mut host);
        let mut session = Session::new();

        let tabs_before: usize = {
            let st = chrome.borrow();
            let t = st.tree();
            t.desktop.workspaces[0].strips[0].window_groups[0]
                .tabs
                .len()
        };

        // winit's `DroppedFile` carries no coordinate, so put the pointer over
        // the group first.
        let over = host
            .group_at_for_tests(strip_id, 0)
            .and_then(|g| g.tab_rects().first().copied())
            .expect("a tab pill to aim at")
            .center();
        session.dispatch(
            &mut host,
            &layout,
            ModifiersState::empty(),
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(over.x, over.y),
            },
        );
        session.dispatch(
            &mut host,
            &layout,
            ModifiersState::empty(),
            &WindowEvent::DroppedFile(doc_path.clone()),
        );

        let commands = launched_commands(&log, 1);
        assert_eq!(commands.len(), 1, "exactly one launch: {commands:#?}");
        assert!(
            commands[0].contains("--app-id so.tilo.word-processor")
                && commands[0].contains(&format!("--open {}", doc_path.display())),
            "the drop had to open the document in its own app: {}",
            commands[0]
        );

        // And no placeholder tab named after the file: the document's window is
        // the tab, and it arrives when the app does.
        let st = chrome.borrow();
        let t = st.tree();
        let group = &t.desktop.workspaces[0].strips[0].window_groups[0];
        assert_eq!(
            group.tabs.len(),
            tabs_before,
            "the drop must not splice a stand-in tab: {:?}",
            group.tabs.iter().map(|t| &t.title).collect::<Vec<_>>()
        );
        assert!(
            !group.tabs.iter().any(|t| t.title.contains("dropped.odt")),
            "a tab named after the file is the bug this replaces"
        );
        drop(t);
        drop(st);

        drop(env);
        launcher::invalidate_app_cache();
    }
}
