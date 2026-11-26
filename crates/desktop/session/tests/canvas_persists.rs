//! Where a note stands on the canvas is the workspace's layout, kept in the workspace's own
//! record: a session that places a note and ends, and a session started after it on the same
//! device, shows the note where it stood, at its size. It fails on a build that starts the canvas
//! anew. The shell's own choices are the person's workspace's, and their restart is
//! `settings_roam`'s.

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

// A note placed on the canvas stands at the same point and size after the session ends and a new
// one starts on this device, kept in the workspace's own layout.
#[test]
fn the_canvas_survives_the_session_ending() {
    let _settled = tilo_ui_base::motion::testing::settled();
    // This device's own state, holding its workspaces: the identity is minted in its home, so its
    // engine opens and the canvas is the workspace's layout.
    let state = crate::common::device_state();
    let (note, stood) = {
        let root = crate::common::private_home();
        let mut session = session(root.path(), &state);
        crate::common::until(&mut session, "the shell's parts", |session| {
            crate::common::on_screen(session, "Notifications").is_some()
        });
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
        drop(session);
        (note, stood)
    };

    // A new session on the same device: what a restart is.
    let root = crate::common::private_home();
    let mut next = session(root.path(), &state);
    crate::common::until(&mut next, "the note back on the canvas", |session| {
        placed(session, &note).is_some()
    });
    assert_eq!(
        placed(&next, &note),
        Some(stood),
        "the note stands at the same point and size after the restart"
    );
}
