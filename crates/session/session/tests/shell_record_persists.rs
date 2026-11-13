//! The shell's own record is the person's real record: what they choose of their desk — the
//! style, where the dock rides, the style's own settings, and the places they arranged — survives
//! the session ending, and it is the same record their other devices read and change.
//!
//! The shell keeps those choices in its own record (`records: .own`), one document opened through
//! the store every app's own record of the person's is opened through, and carried on the
//! person's own cluster. This proves the file half of it: a session that sets four choices and
//! ends, and a session started after it on the same device, reads all four back. It fails on a
//! build that starts the shell on an empty record every time, which is what it did before.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::path::Path;
use std::time::Duration;

use tilo_compositor::OutputConfig;
use tilo_service::Questions;
use tilo_session::{ActionPlane, Options, Session};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
const PERSON: &str = "Pat";
/// The note the test places on the canvas: a seeded one the notes app holds.
const NOTE: &str = "Groceries";

/// The person's own directory, where their apps' records of themselves are kept: pinned for the
/// whole process, so two sessions a test starts on this device read the one store. Set before any
/// session opens a record, as the launcher sets it before it starts an app.
fn person_dir() -> &'static Path {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("tilo-shell-record-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("the person's directory");
        tilo_test_env::pin("TILO_PERSON_DIR", &dir);
        dir
    })
}

/// A headless session showing the shell over the seeded apps, on this device's own state, with its
/// agent door and the test's own program admitted to act as the person: two calls on one test are
/// two sessions of the same device, as a restart is.
fn session(root: &Path, state: &Path) -> Session {
    // The seeded apps a development session starts with: installed in this run's store, published
    // for the person (notes, tasks, chat, mail).
    let seeded = crate::common::seeded_apps();
    let program = std::env::current_exe().expect("this test's program");
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
            services: vec![Box::new(seeded)],
            questions: Some(Questions::new()),
            action_plane: ActionPlane::At(root.join("plane.sock")),
            agent_door: Some(root.join("agent.sock")),
            admit_repl: vec![program],
            state: Some(state.to_path_buf()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    step(&mut session);
    session
}

fn step(session: &mut Session) {
    session.step(Duration::from_millis(1)).expect("a frame");
}

/// The note `title` the notes app holds, by its published id.
fn note(session: &Session, title: &str) -> String {
    fn find(resources: &[tilo_actions::ResourceSpec], title: &str) -> Option<String> {
        resources.iter().find_map(|resource| {
            if resource.label == title && resource.kind.ends_with(".Note") {
                Some(resource.id.as_str().to_owned())
            } else {
                find(&resource.children, title)
            }
        })
    }
    find(&session.resources(), title).unwrap_or_else(|| panic!("the seeded note {title:?}"))
}

/// Place `resource` on the canvas of the workspace in front, through the arrangement's published
/// `place`, as an agent puts it there.
fn place(session: &mut Session, resource: &str) -> u64 {
    let (workspace, row, strip, place) = {
        let tree = session.handle().read(Clone::clone);
        let workspace = tree.active_workspace().expect("a workspace is in front");
        (
            workspace.slot.0.to_string(),
            workspace.strips.first().expect("a row").id.0,
            session.handle().mint_id(),
            session.handle().mint_id(),
        )
    };
    let arguments = serde_json::json!({
        "workspace": workspace,
        "row": row,
        "strip": strip,
        "place": place,
        "resource": resource,
        "x": 80,
        "y": 60,
        "width": 720,
        "height": 620,
    })
    .as_object()
    .expect("arguments")
    .clone();
    session
        .invoke("shell:arrangement", "place", &arguments)
        .unwrap_or_else(|error| panic!("place {resource}: {error:?}"));
    place
}

/// Whether the arrangement holds a place for the resource `record`, by its published id.
fn placed(session: &Session, record: &str) -> bool {
    session
        .arrangement()
        .borrow()
        .reading()
        .places
        .iter()
        .any(|place| place.record == record)
}

// The shell's record is the person's: the four choices a person makes of their desk — the style,
// where the dock rides, the style's own setting, and where a note stands on the canvas — are the
// same after the session ends and a new one starts on this device.
//
// It fails on a build that starts the shell on an empty record: the second session draws the
// shipped defaults, and none of the four is there.
#[test]
fn the_desk_choices_survive_the_session_ending() {
    let _settled = ui::motion::testing::settled();
    let _person = person_dir();
    let state = tempfile::tempdir().expect("a state directory for this device");
    let (note, before) = {
        let root = crate::common::private_home();
        let mut session = session(root.path(), state.path());
        crate::common::until(&mut session, "the shell's parts", |session| {
            crate::common::on_screen(session, "Notifications").is_some()
        });
        // The desktop style, as the switch's own action chooses it: a view of the arrangement the
        // shell ships (`so.tilo.shell:desktop`).
        let mut repl = crate::common::Repl::connect(&root.path().join("agent.sock"));
        repl.opened(&mut session);
        let (answer, failed) = repl.run(
            &mut session,
            "shell.show_style(shell.viewing, shown: \"so.tilo.shell:desktop\")",
        );
        assert!(!failed, "the desk shows the desktop: {answer}");
        session
            .run_shell_setting("dock_on", vec![serde_json::json!("left")])
            .expect("the dock moves to the leading edge");
        session
            .run_shell_setting("set_frames_snap", vec![serde_json::json!(false)])
            .expect("the Frames grid turns off");
        let note = note(&session, NOTE);
        place(&mut session, &note);
        crate::common::until(&mut session, "the note on the canvas", |session| {
            placed(session, &note)
        });
        // The record's file on this device is where the person's choices are kept.
        let path = session
            .shell_record_path()
            .expect("the shell keeps its record on this device")
            .to_path_buf();
        // The four choices, as the shell holds them now, before the session ends. The dock's edge
        // is a choice, kept as the case it names; the grid is a flag; the style a view reference;
        // and the note a place in the arrangement.
        let held = Choices {
            dock_edge: session.shell_setting("dock_edge"),
            frames_snap: session.shell_setting("frames_snap"),
            style: session.shell_setting("style"),
        };
        assert!(
            held.dock_edge.is_some() && held.frames_snap.is_some() && held.style.is_some(),
            "all three settings read from the record: {held:?}"
        );
        assert_eq!(
            held.frames_snap,
            Some(serde_json::Value::Bool(false)),
            "the Frames grid is off"
        );
        assert_eq!(
            held.style,
            Some(serde_json::Value::String("so.tilo.shell:desktop".into())),
            "the desktop style is the one shown"
        );
        // The record's file holds them, not only the session's memory: a session that ends on its
        // own writes the copy, and one taken down with the changes still in hand writes it as it
        // lets go.
        drop(session);
        assert!(
            path.exists(),
            "the record's file is written: {}",
            path.display()
        );
        (note, held)
    };

    // A new session on the same device: what a restart is.
    let root = crate::common::private_home();
    let mut next = session(root.path(), state.path());
    crate::common::until(&mut next, "the shell's record drawn", |session| {
        session.shell_setting("style").is_some()
    });
    let after = Choices {
        dock_edge: next.shell_setting("dock_edge"),
        frames_snap: next.shell_setting("frames_snap"),
        style: next.shell_setting("style"),
    };
    assert_eq!(
        after, before,
        "every desk choice the first session held is the one the new session holds"
    );
    assert_eq!(
        after.frames_snap,
        Some(serde_json::Value::Bool(false)),
        "the Frames grid is still off after the restart"
    );
    assert_eq!(
        after.style,
        Some(serde_json::Value::String("so.tilo.shell:desktop".into())),
        "the desktop style is still the one shown"
    );
    crate::common::until(&mut next, "the note back on the canvas", |session| {
        placed(session, &note)
    });
    assert!(
        placed(&next, &note),
        "the note is still on the canvas after the restart"
    );
}

/// The shell's desk choices a test reads across a restart: the dock's edge, whether the Frames
/// grid is on, and the style shown, as each setting's record value.
#[derive(Debug, PartialEq)]
struct Choices {
    dock_edge: Option<serde_json::Value>,
    frames_snap: Option<serde_json::Value>,
    style: Option<serde_json::Value>,
}
