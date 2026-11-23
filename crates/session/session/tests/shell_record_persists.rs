//! The shell's own record is the person's real record: what they choose of their shell — the
//! style, where the dock rides and the style's own settings — survives
//! the session ending, and it is the same record their other devices read and change.
//!
//! The shell keeps those choices in the person's own workspace (`resources: .own`), one document
//! opened through the store every app's record of theirs is opened through. Where a note stands on the canvas is not the shell's: it is the
//! workspace's layout, kept in the workspace's own record. This proves the file half of both: a
//! session that sets three choices and places a note, and ends, and a session started after it on
//! the same device, reads all three back and shows the note where it stood, at its size. It fails
//! on a build that starts the shell on an empty record every time, or the canvas anew.

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

use tilo_compositor::OutputConfig;
use tilo_session::{Options, Session};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
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

/// A headless session showing the shell over the seeded apps, on this device's own state — a device
/// that holds its workspaces, as a person's own does (`state` is set up as one) — with its
/// sockets at `root` ([`crate::common::api_options`]): two calls on one test are
/// two sessions of the same device, as a restart is.
fn session(root: &Path, state: &Path) -> Session {
    // The seeded apps a development session starts with: installed in this run's store, published
    // for the person (notes, tasks, chat, mail).
    let seeded = crate::common::seeded_apps();
    let mut session = Session::headless(
        Options {
            services: seeded,
            state: Some(state.to_path_buf()),
            ..crate::common::api_options(root)
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    crate::common::kept_in_workspace(&mut session);
    session
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

/// Place `resource` on the canvas of the workspace in front, through the layout's published
/// `place`, as an agent puts it there.
fn place(session: &mut Session, resource: &str) -> u64 {
    let (workspace, row, strip, place) = {
        let tree = session.handle().read(Clone::clone);
        let workspace = tree.active_workspace().expect("a workspace is in front");
        (
            workspace.id().to_string(),
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
        .invoke("shell:layout", "place", &arguments)
        .unwrap_or_else(|error| panic!("place {resource}: {error:?}"));
    place
}

/// Where the canvas stands the resource `record` (by its published id): its point and size, once the
/// layout holds a place for it.
fn placed(session: &Session, record: &str) -> Option<(i64, i64, i64, i64)> {
    let reading = session.layout().borrow().reading();
    let place = reading
        .places
        .iter()
        .find(|place| place.resource == record)?;
    let strip = reading
        .strips
        .iter()
        .find(|strip| strip.key == place.strip)?;
    Some((
        strip.canvas_x,
        strip.canvas_y,
        strip.canvas_width,
        strip.canvas_height,
    ))
}

// The shell's record is the person's: the choices a person makes of their shell — the style,
// where the dock rides and the style's own setting — are the same after the session ends and a new
// one starts on this device, and the note they placed on the canvas stands at the same point and
// size, kept in the workspace's own layout.
//
// It fails on a build that starts the shell on an empty record: the second session draws the
// shipped defaults, and none of the three is there; and on one that starts the canvas anew.
#[test]
fn the_shell_choices_survive_the_session_ending() {
    let _settled = ui::motion::testing::settled();
    let _person = person_dir();
    // This device's own state, holding its workspaces: the identity is minted in its home, so its
    // engine opens and the canvas is the workspace's layout.
    let state = crate::common::device_state();
    let (note, stood, before) = {
        let root = crate::common::private_home();
        let mut session = session(root.path(), &state);
        crate::common::until(&mut session, "the shell's parts", |session| {
            crate::common::on_screen(session, "Notifications").is_some()
        });
        // The desktop style, as the switch's own action chooses it: a view of the layout the
        // shell ships (`so.tilo.shell:desktop`).
        let mut repl = crate::common::Api::open(&mut session, root.path());
        let (answer, failed) = repl.run(
            &mut session,
            "shell.show_style(shell.viewing, style: \"so.tilo.shell:desktop\")",
        );
        assert!(!failed, "the shell shows the desktop style: {answer}");
        session
            .run_shell_action("dock_on", vec![serde_json::json!("left")])
            .expect("the dock moves to the leading edge");
        session
            .run_shell_action("set_frames_snap", vec![serde_json::json!(false)])
            .expect("the Frames grid turns off");
        let note = note(&session, NOTE);
        place(&mut session, &note);
        crate::common::until(&mut session, "the note on the canvas", |session| {
            placed(session, &note).is_some()
        });
        let stood = placed(&session, &note).expect("the note on the canvas");
        assert_eq!(
            stood,
            (80, 60, 720, 620),
            "the note stands where it was placed, at its size"
        );
        // The record's file on this device is where the person's choices are kept.
        let path = session
            .shell_record_path()
            .expect("the shell keeps its record on this device")
            .to_path_buf();
        // The three choices, as the shell holds them now, before the session ends. The dock's
        // edge is a choice, kept as the case it names; the grid is a flag; the style a view
        // reference. The note is a place in the workspace's layout, not a choice of the shell's.
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
        (note, stood, held)
    };

    // A new session on the same device: what a restart is.
    let root = crate::common::private_home();
    let mut next = session(root.path(), &state);
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
        "every shell choice the first session held is the one the new session holds"
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
    // Where the note stands is the workspace's layout, kept in the workspace's own record: the new
    // session shows it at the same point and size.
    crate::common::until(&mut next, "the note back on the canvas", |session| {
        placed(session, &note).is_some()
    });
    assert_eq!(
        placed(&next, &note),
        Some(stood),
        "the note stands at the same point and size after the restart"
    );
}

/// The shell's own choices a test reads across a restart: the dock's edge, whether the Frames
/// grid is on, and the style shown, as each setting's record value.
#[derive(Debug, PartialEq)]
struct Choices {
    dock_edge: Option<serde_json::Value>,
    frames_snap: Option<serde_json::Value>,
    style: Option<serde_json::Value>,
}
